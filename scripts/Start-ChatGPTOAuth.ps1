param(
    [int]$TimeoutSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
Add-Type -AssemblyName System.Web

function Send-CallbackResponse {
    param([Parameter(Mandatory)][Net.Sockets.TcpClient]$Client, [Parameter(Mandatory)][string]$Message)
    $body = "<!doctype html><html><head><meta charset=`"utf-8`"><title>Codex Router</title></head><body><h2>$Message</h2><p>You can close this tab and return to Codex.</p></body></html>"
    $bodyBytes = [Text.Encoding]::UTF8.GetBytes($body)
    $header = "HTTP/1.1 200 OK`r`nContent-Type: text/html; charset=utf-8`r`nContent-Length: $($bodyBytes.Length)`r`nConnection: close`r`n`r`n"
    $headerBytes = [Text.Encoding]::ASCII.GetBytes($header)
    $stream = $Client.GetStream()
    try {
        $stream.Write($headerBytes, 0, $headerBytes.Length)
        $stream.Write($bodyBytes, 0, $bodyBytes.Length)
        $stream.Flush()
    } finally {
        [Array]::Clear($bodyBytes, 0, $bodyBytes.Length)
        [Array]::Clear($headerBytes, 0, $headerBytes.Length)
        $stream.Dispose()
    }
}

$lifecycleLock = Enter-RouterLifecycleLock `
    -RouterRoot $routerRoot `
    -TimeoutMilliseconds 10000 `
    -Operation 'Start ChatGPT OAuth'
$previousLifecycleLockMarker = [Environment]::GetEnvironmentVariable(
    'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
    'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_LIFECYCLE_LOCK_HELD', [string]$PID, 'Process')
try {
    & "$routerRoot\scripts\Start-Router.ps1" -RepairUnhealthy | Out-Null
$session = New-RouterAdminSession
$group = (Get-RouterGroups -Session $session | Where-Object { $_.name -in @('Codex-Router', 'Codex Unified Router') } | Select-Object -First 1)
if (-not $group) { throw 'Run the Codex-Router one-click configuration before starting ChatGPT OAuth.' }
$routerConfigPath = Get-RouterConfigPath -RouterRoot $routerRoot
$oauthPriority = 1
if (Test-Path -LiteralPath $routerConfigPath) {
    $routerConfig = Get-Content -LiteralPath $routerConfigPath -Raw | ConvertFrom-Json
    $oauthPriority = [int](Get-RouterOAuthRoutingPriorities -OAuthFallback $routerConfig.oauthFallback).OAuthPriority
}

$existing = Get-RouterAccounts -Session $session | Where-Object { $_.name -eq 'ChatGPT Plus OAuth' } | Select-Object -First 1
if ($existing) {
    $planId = Set-RouterScheduledRecovery -Session $session -AccountId ([long]$existing.id) -ModelId 'gpt-5.6-sol'
    Write-Output "ChatGPT Plus OAuth account $($existing.id) is configured with hourly recovery plan $planId."
    exit 0
}

$redirectUri = 'http://localhost:1455/auth/callback'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 1455)
$listener.Start()
try {
    $authResponse = Invoke-RouterApi `
        -Session $session `
        -Method POST `
        -Path '/api/v1/admin/openai/generate-auth-url' `
        -Body @{ redirect_uri = $redirectUri }
    $auth = Get-RouterResponseData -Response $authResponse
    if (-not $auth.auth_url -or -not $auth.session_id) { throw 'Sub2API returned an incomplete OAuth authorization response.' }

    Start-Process ([string]$auth.auth_url)
    Write-Output 'Browser opened. Complete the ChatGPT login and authorization.'

    $acceptTask = $listener.AcceptTcpClientAsync()
    if (-not $acceptTask.Wait([TimeSpan]::FromSeconds($TimeoutSeconds))) {
        throw "OAuth callback timed out after $TimeoutSeconds seconds."
    }
    $client = $acceptTask.Result
    try {
        $reader = [IO.StreamReader]::new($client.GetStream(), [Text.Encoding]::ASCII, $false, 4096, $true)
        try {
            $requestLine = $reader.ReadLine()
            while ($reader.ReadLine()) { }
        } finally {
            $reader.Dispose()
        }
        if ($requestLine -notmatch '^GET\s+(\S+)\s+HTTP/') { throw 'OAuth callback request was not recognized.' }
        $callback = [Uri]("http://localhost" + $Matches[1])
        $query = [Web.HttpUtility]::ParseQueryString($callback.Query)
        if ($query['error']) { throw "OAuth authorization failed: $($query['error'])" }
        $code = $query['code']
        $state = $query['state']
        if (-not $code -or -not $state) { throw 'OAuth callback did not contain code and state.' }
        Send-CallbackResponse -Client $client -Message 'Authorization received successfully.'
    } finally {
        $client.Dispose()
    }

    $createResponse = Invoke-RouterApi `
        -Session $session `
        -Method POST `
        -Path '/api/v1/admin/openai/create-from-oauth' `
        -Body @{
            session_id = [string]$auth.session_id
            code = $code
            state = $state
            redirect_uri = $redirectUri
            name = 'ChatGPT Plus OAuth'
            concurrency = 3
            priority = $oauthPriority
            group_ids = @([long]$group.id)
        }
    $account = Get-RouterResponseData -Response $createResponse
    $planId = Set-RouterScheduledRecovery -Session $session -AccountId ([long]$account.id) -ModelId 'gpt-5.6-sol'

    [pscustomobject]@{
        Account = $account.name
        AccountId = $account.id
        Priority = $oauthPriority
        Recovery = 'hourly'
        RecoveryPlanId = $planId
    } | Format-List
} finally {
    $listener.Stop()
    $code = $null
    $state = $null
    if ($session) { $session.Headers.Clear() }
}
} finally {
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
        $previousLifecycleLockMarker,
        'Process')
    Exit-RouterLifecycleLock -Lock $lifecycleLock
}
