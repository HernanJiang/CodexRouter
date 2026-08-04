param(
    [Parameter(Mandatory)]
    [ValidateSet('openai', 'anthropic', 'gemini', 'antigravity', 'grok')]
    [string]$Provider,
    [int]$TimeoutSeconds = 600,
    [switch]$ComplianceAccepted,
    [switch]$ProbeOnly,
    [switch]$PrepareOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$prepareStage = 'components'

function Get-OAuthPrepareFailureCode {
    param(
        [Parameter(Mandatory)][string]$Stage,
        [Parameter(Mandatory)][string]$ErrorText
    )
    if ($ErrorText -match 'ROUTER_LIFECYCLE_(BUSY|DEFERRED)') {
        return 'ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY'
    }
    if ($ErrorText -match 'ROUTER_(INSTALL_ROOT|PORT)_CONFLICT') {
        return 'ROUTER_OAUTH_PREPARE_ROUTER_START'
    }
    switch ($Stage) {
        'components' { 'ROUTER_OAUTH_PREPARE_COMPONENTS' }
        'lifecycle_lock' { 'ROUTER_OAUTH_PREPARE_LIFECYCLE_BUSY' }
        'router_health' { 'ROUTER_OAUTH_PREPARE_ROUTER_START' }
        'router_start' { 'ROUTER_OAUTH_PREPARE_ROUTER_START' }
        'admin_login' { 'ROUTER_OAUTH_PREPARE_ADMIN_LOGIN' }
        'compliance_probe' { 'ROUTER_OAUTH_PREPARE_COMPLIANCE' }
        default { 'ROUTER_OAUTH_PREPARE_PROCESS' }
    }
}

function Write-OAuthPrepareFailure {
    param(
        [Parameter(Mandatory)][string]$Stage,
        [Parameter(Mandatory)][string]$ErrorText
    )
    [ordered]@{
        status = 'error'
        provider = $Provider
        stage = $Stage
        code = (Get-OAuthPrepareFailureCode -Stage $Stage -ErrorText $ErrorText)
    } | ConvertTo-Json -Compress | Write-Output
}

function Start-OAuthPreparationPage {
    param([Parameter(Mandatory)][string]$ProviderName)

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $nonce = [Guid]::NewGuid().ToString('N')
    $safeProvider = [Net.WebUtility]::HtmlEncode($ProviderName)
    $pagePath = Join-Path ([IO.Path]::GetTempPath()) "codex-router-oauth-$PID-$nonce.html"
    $html = @"
<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Cache-Control" content="no-store">
<title>Codex-Router OAuth</title>
<style>
*{box-sizing:border-box}body{margin:0;background:#f3f6f8;color:#17232d;font-family:"Segoe UI","Microsoft YaHei",sans-serif;letter-spacing:0}.bar{height:6px;background:#177e89}.shell{max-width:720px;margin:0 auto;padding:72px 28px}.brand{font-size:14px;font-weight:700;text-transform:uppercase;color:#52636f}.title{margin:28px 0 12px;font-size:32px;line-height:1.25}.copy{font-size:16px;line-height:1.7;color:#536570}.status{display:flex;align-items:center;gap:14px;margin-top:34px;padding-top:22px;border-top:1px solid #cad4da}.spinner{width:22px;height:22px;border:3px solid #c6d6db;border-top-color:#177e89;border-radius:50%;animation:spin .8s linear infinite}.error .spinner{display:none}.error #state{color:#b42318}@keyframes spin{to{transform:rotate(360deg)}}
</style>
</head>
<body>
<div class="bar"></div>
<main class="shell" id="shell">
  <div class="brand">Codex-Router</div>
  <h1 class="title">&#x6B63;&#x5728;&#x51C6;&#x5907; $safeProvider &#x5B89;&#x5168;&#x767B;&#x5F55;</h1>
  <p class="copy">Preparing the secure $safeProvider sign-in.</p>
  <div class="status"><div class="spinner"></div><div id="state">&#x6B63;&#x5728;&#x542F;&#x52A8;&#x672C;&#x5730;&#x670D;&#x52A1;&#x5E76;&#x8FDE;&#x63A5;&#x6388;&#x6743;&#x7AEF;&#x70B9;...</div></div>
</main>
<script>
const endpoint='http://127.0.0.1:$port/oauth-ready?nonce=$nonce';
const shell=document.getElementById('shell');
const state=document.getElementById('state');
async function poll(){
  const controller=new AbortController();
  const timer=setTimeout(()=>controller.abort(),800);
  try{
    const response=await fetch(endpoint,{cache:'no-store',signal:controller.signal});
    if(response.ok){
      const data=await response.json();
      if(data.status==='ready'&&/^https?:\/\//.test(data.url)){location.replace(data.url);return;}
      if(data.status==='error'){shell.classList.add('error');state.textContent=data.message||'OAuth preparation failed.';return;}
    }
  }catch(_error){}finally{clearTimeout(timer)}
  setTimeout(poll,350);
}
poll();
</script>
</body>
</html>
"@
    [IO.File]::WriteAllText($pagePath, $html, [Text.UTF8Encoding]::new($false))
    Start-Process -FilePath $pagePath | Out-Null
    return [pscustomobject]@{
        Listener = $listener
        Nonce = $nonce
        PagePath = $pagePath
    }
}

function Complete-OAuthPreparationPage {
    param(
        [AllowNull()]$State,
        [string]$AuthorizationUrl = '',
        [string]$FailureMessage = ''
    )
    if ($null -eq $State -or $null -eq $State.Listener) { return $false }

    $payload = if ($AuthorizationUrl) {
        @{ status = 'ready'; url = $AuthorizationUrl }
    } else {
        @{ status = 'error'; message = $FailureMessage }
    }
    $bodyBytes = [Text.UTF8Encoding]::new($false).GetBytes(($payload | ConvertTo-Json -Compress))
    # A ready preparation tab normally polls within 350 ms. Falling back after
    # two seconds avoids making users wait through a long browser handoff when
    # local-file polling is blocked by browser policy.
    $deadline = [DateTime]::UtcNow.AddSeconds(2)
    $delivered = $false
    try {
        while (-not $delivered -and [DateTime]::UtcNow -lt $deadline) {
            $acceptTask = $State.Listener.AcceptTcpClientAsync()
            while (-not $acceptTask.IsCompleted -and [DateTime]::UtcNow -lt $deadline) {
                [void]$acceptTask.Wait(200)
            }
            if (-not $acceptTask.IsCompleted) { break }
            $client = $acceptTask.Result
            try {
                $stream = $client.GetStream()
                $stream.ReadTimeout = 1000
                $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::ASCII, $false, 2048, $true)
                try {
                    $requestLine = $reader.ReadLine()
                    while ($reader.ReadLine()) { }
                } finally {
                    $reader.Dispose()
                }
                if (-not $requestLine -or $requestLine -notlike "*nonce=$($State.Nonce)*") { continue }
                $header = "HTTP/1.1 200 OK`r`nContent-Type: application/json; charset=utf-8`r`nAccess-Control-Allow-Origin: *`r`nCache-Control: no-store`r`nContent-Length: $($bodyBytes.Length)`r`nConnection: close`r`n`r`n"
                $headerBytes = [Text.Encoding]::ASCII.GetBytes($header)
                try {
                    $stream.Write($headerBytes, 0, $headerBytes.Length)
                    $stream.Write($bodyBytes, 0, $bodyBytes.Length)
                    $stream.Flush()
                    $delivered = $true
                } catch { }
                finally { [Array]::Clear($headerBytes, 0, $headerBytes.Length) }
            } catch { }
            finally { $client.Dispose() }
        }
    } finally {
        [Array]::Clear($bodyBytes, 0, $bodyBytes.Length)
        $State.Listener.Stop()
        $State.Listener = $null
        Remove-Item -LiteralPath $State.PagePath -Force -ErrorAction SilentlyContinue
    }
    return $delivered
}

function Stop-OAuthPreparationPage {
    param([AllowNull()]$State)
    if ($null -eq $State) { return }
    if ($null -ne $State.Listener) {
        $State.Listener.Stop()
        $State.Listener = $null
    }
    Remove-Item -LiteralPath $State.PagePath -Force -ErrorAction SilentlyContinue
}

$oauthPreparation = $null
if (-not $ProbeOnly -and -not $PrepareOnly) {
    try { $oauthPreparation = Start-OAuthPreparationPage -ProviderName $Provider }
    catch { $oauthPreparation = $null }
}

try {
    $prepareStage = 'components'
    Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
    Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
    Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
    Add-Type -AssemblyName System.Web
} catch {
    if ($PrepareOnly) {
        Write-OAuthPrepareFailure -Stage $prepareStage -ErrorText $_.Exception.Message
    }
    [void](Complete-OAuthPreparationPage -State $oauthPreparation -FailureMessage 'Codex-Router could not load the local OAuth components.')
    exit 1
}

function Send-CallbackResponse {
    param(
        [Parameter(Mandatory)][Net.Sockets.TcpClient]$Client,
        [Parameter(Mandatory)][string]$Message
    )
    $body = "<!doctype html><html><head><meta charset=`"utf-8`"><title>Codex-Router OAuth</title></head><body><h2>$Message</h2><p>Authorization has returned to Codex-Router. You can close this tab.</p></body></html>"
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

function Receive-OAuthCallback {
    param(
        [Parameter(Mandatory)][Net.Sockets.TcpListener]$Listener,
        [Parameter(Mandatory)][int]$Timeout
    )
    $acceptTask = $Listener.AcceptTcpClientAsync()
    if (-not $acceptTask.Wait([TimeSpan]::FromSeconds($Timeout))) {
        throw "OAuth callback timed out after $Timeout seconds."
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
        if ($requestLine -notmatch '^GET\s+(\S+)\s+HTTP/') {
            throw 'OAuth callback request was not recognized.'
        }
        $callback = [Uri]("http://localhost" + $Matches[1])
        $query = [Web.HttpUtility]::ParseQueryString($callback.Query)
        if ($query['error']) { throw "OAuth authorization failed: $($query['error'])" }
        if (-not $query['code']) { throw 'OAuth callback did not contain an authorization code.' }
        Send-CallbackResponse -Client $client -Message 'Authorization received successfully.'
        return [pscustomobject]@{
            Code = [string]$query['code']
            State = [string]$query['state']
        }
    } finally {
        $client.Dispose()
    }
}

function Copy-Fields {
    param(
        [Parameter(Mandatory)][Collections.IDictionary]$Source,
        [Parameter(Mandatory)][string[]]$Names
    )
    $result = @{}
    foreach ($name in $Names) {
        if ($Source.Contains($name) -and $null -ne $Source[$name] -and [string]$Source[$name] -ne '') {
            $result[$name] = $Source[$name]
        }
    }
    return $result
}

function ConvertTo-PlainHashtable {
    param([AllowNull()]$Value)
    if ($null -eq $Value) { return $null }
    if ($Value -is [System.Collections.IDictionary]) {
        $result = @{}
        foreach ($key in $Value.Keys) { $result[[string]$key] = ConvertTo-PlainHashtable $Value[$key] }
        return $result
    }
    if ($Value -is [pscustomobject]) {
        $result = @{}
        foreach ($property in $Value.PSObject.Properties) {
            $result[$property.Name] = ConvertTo-PlainHashtable $property.Value
        }
        return $result
    }
    if ($Value -is [System.Collections.IEnumerable] -and $Value -isnot [string]) {
        return @($Value | ForEach-Object { ConvertTo-PlainHashtable $_ })
    }
    return $Value
}

function Get-OptionalString {
    param([Parameter(Mandatory)]$Object, [Parameter(Mandatory)][string]$Name)
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property -or $null -eq $property.Value) { return '' }
    return [string]$property.Value
}

try {
    $prepareStage = 'lifecycle_lock'
    $lifecycleLock = Enter-RouterLifecycleLock `
        -RouterRoot $routerRoot `
        -TimeoutMilliseconds 10000 `
        -Operation $(if ($PrepareOnly) { "Prepare $Provider OAuth" } else { "Start $Provider OAuth" })
} catch {
    if ($PrepareOnly) {
        Write-OAuthPrepareFailure -Stage $prepareStage -ErrorText $_.Exception.Message
    }
    [void](Complete-OAuthPreparationPage -State $oauthPreparation -FailureMessage 'Another Codex-Router operation is still running. Retry in a moment.')
    exit 1
}
$previousLifecycleLockMarker = [Environment]::GetEnvironmentVariable(
    'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
    'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_LIFECYCLE_LOCK_HELD', [string]$PID, 'Process')
try {
    $prepareStage = 'router_health'
    $routerHealthy = $false
    try {
        $health = Invoke-LoopbackRestMethod `
            -Method GET `
            -Uri "$(Get-RouterBaseUri)/health" `
            -TimeoutSec 3
        $routerHealthy = $null -ne $health
    } catch { }
    if (-not $routerHealthy) {
        $prepareStage = 'router_start'
        & (Join-Path $PSScriptRoot 'Initialize-Router.ps1') | Out-Null
        & (Join-Path $PSScriptRoot 'Start-Router.ps1') | Out-Null
    }
$prepareStage = 'admin_login'
$session = New-RouterAdminSession
$listener = $null
$credentials = $null
$tokenMap = $null
try {
    $prepareStage = 'compliance_probe'
    $compliance = Get-RouterResponseData (
        Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/compliance'
    )
    if ($PrepareOnly) {
        [ordered]@{
            status = 'ready'
            provider = $Provider
            stage = 'ready'
            code = 'ok'
            complianceRequired = [bool]$compliance.required
        } | ConvertTo-Json -Compress
        return
    }
    if ($compliance.required) {
        if (-not $ComplianceAccepted) {
            throw 'Accept the Codex-Router and Sub2API deployment terms in the app before starting OAuth.'
        }
        [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/compliance/accept' -Body @{
            phrase = [string]$compliance.ack_phrase_zh
            language = 'zh'
        })
    }
    $group = Get-RouterGroups -Session $session |
        Where-Object { $_.name -in @('Codex-Router', 'Codex Unified Router') } |
        Select-Object -First 1
    if (-not $group) {
        $group = Get-RouterResponseData (
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/groups' -IdempotencyKey 'codex-router-oauth-onboarding-group-v1' -Body @{
                name = 'Codex-Router'
                description = 'Single-user local Codex multi-model router managed by Codex-Router.'
                platform = 'openai'
                rate_multiplier = 1.0
                is_exclusive = $false
                subscription_type = 'standard'
                status = 'active'
                allow_messages_dispatch = $false
                allow_live = $false
                require_oauth_only = $false
                models_list_config = @{ enabled = $false; models = @() }
            }
        )
    }

    $priority = 1
    $configPath = Get-RouterConfigPath -RouterRoot $routerRoot
    if (Test-Path -LiteralPath $configPath) {
        $routerConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
        $priority = [int](Get-RouterOAuthRoutingPriorities -OAuthFallback $routerConfig.oauthFallback).OAuthPriority
    }

    $geminiOAuthType = 'google_one'
    $geminiProjectID = ''
    $geminiTierID = 'google_one_free'
    if ($Provider -eq 'gemini' -and -not $ProbeOnly) {
        $detectedProjectIDs = @()
        foreach ($googleAccount in @(Get-RouterAccounts -Session $session)) {
            $platform = Get-OptionalString -Object $googleAccount -Name 'platform'
            $accountType = Get-OptionalString -Object $googleAccount -Name 'type'
            if ($platform -notin @('gemini', 'antigravity') -or $accountType -ne 'oauth') { continue }
            $googleAccountID = Get-OptionalString -Object $googleAccount -Name 'id'
            if (-not $googleAccountID) { continue }
            try {
                $googleDetail = Get-RouterResponseData (
                    Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$googleAccountID"
                )
                $credentialsProperty = $googleDetail.PSObject.Properties['credentials']
                if ($null -ne $credentialsProperty -and $null -ne $credentialsProperty.Value) {
                    $candidateProjectID = Get-OptionalString -Object $credentialsProperty.Value -Name 'project_id'
                    if ($candidateProjectID) { $detectedProjectIDs += $candidateProjectID.Trim() }
                }
            } catch { }
        }
        $detectedProjectIDs = @($detectedProjectIDs | Sort-Object -Unique)
        if ($detectedProjectIDs.Count -eq 1) { $geminiProjectID = $detectedProjectIDs[0] }

        Write-Host ''
        Write-Host 'Gemini login mode:' -ForegroundColor Cyan
        Write-Host '  1. Google One / personal Gemini quota (default)'
        Write-Host '  2. GCP Gemini Code Assist quota'
        $geminiMode = (Read-Host 'Select 1 or 2').Trim()
        if ($geminiMode -eq '2') {
            $geminiOAuthType = 'code_assist'
            $geminiTierID = 'gcp_standard'
        }
        if ($geminiProjectID) {
            $enteredProjectID = (Read-Host "Google Cloud Project ID (press Enter to use detected: $geminiProjectID)").Trim()
            if ($enteredProjectID) { $geminiProjectID = $enteredProjectID }
        } else {
            $geminiProjectID = (Read-Host 'Google Cloud Project ID (optional; press Enter to auto-detect)').Trim()
        }
    }

    $automaticCallback = $Provider -in @('openai', 'antigravity', 'grok')
    $callbackPort = switch ($Provider) {
        'openai' { 1455 }
        'antigravity' { 8085 }
        'grok' { 56121 }
        default { 0 }
    }
    if ($automaticCallback) {
        # OAuth callbacks are local-only. Binding to all interfaces can trigger
        # a Windows Firewall consent dialog and is unnecessary here.
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $callbackPort)
        $listener.Start()
    }

    $authRequest = switch ($Provider) {
        'openai' {
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/openai/generate-auth-url' -Body @{
                redirect_uri = 'http://localhost:1455/auth/callback'
            }
        }
        'anthropic' {
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts/generate-auth-url' -Body @{}
        }
        'gemini' {
            $geminiAuthBody = @{
                oauth_type = $geminiOAuthType
                tier_id = $geminiTierID
            }
            if ($geminiProjectID) { $geminiAuthBody['project_id'] = $geminiProjectID }
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/gemini/oauth/auth-url' -Body $geminiAuthBody
        }
        'antigravity' {
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/antigravity/oauth/auth-url' -Body @{}
        }
        'grok' {
            Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/grok/oauth/auth-url' -Body @{}
        }
    }
    $auth = Get-RouterResponseData $authRequest
    if (-not $auth.auth_url -or -not $auth.session_id) {
        throw "Sub2API returned an incomplete $Provider OAuth authorization response."
    }

    if ($ProbeOnly) {
        $authUri = [Uri][string]$auth.auth_url
        [pscustomobject]@{
            Provider = $Provider
            AuthorizationHost = $authUri.Host
            SessionCreated = $true
        }
        return
    }

    $authorizationOpened = Complete-OAuthPreparationPage `
        -State $oauthPreparation `
        -AuthorizationUrl ([string]$auth.auth_url)
    if (-not $authorizationOpened) {
        Start-Process ([string]$auth.auth_url)
    }
    Write-Host ''
    Write-Host "Opened the official $Provider authorization page." -ForegroundColor Cyan
    if ($automaticCallback) {
        Write-Host 'Complete login in the browser. Codex-Router will receive the callback automatically.'
        $callback = Receive-OAuthCallback -Listener $listener -Timeout $TimeoutSeconds
        $code = $callback.Code
        $state = if ($callback.State) { $callback.State } else { Get-OptionalString -Object $auth -Name 'state' }
    } else {
        Write-Host 'Complete login in the browser, copy the authorization code shown by the provider,'
        Write-Host 'then paste it below. The code is used only for this local OAuth exchange.'
        $code = Read-Host 'Authorization code'
        $state = Get-OptionalString -Object $auth -Name 'state'
    }
    if ([string]::IsNullOrWhiteSpace($code)) { throw 'No authorization code was provided.' }

    if ($Provider -eq 'openai') {
        if ([string]::IsNullOrWhiteSpace($state)) { throw 'OpenAI callback did not contain state.' }
        $accountResponse = Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/openai/create-from-oauth' -Body @{
            session_id = [string]$auth.session_id
            code = $code
            state = $state
            redirect_uri = 'http://localhost:1455/auth/callback'
            name = 'ChatGPT OAuth'
            concurrency = 3
            priority = $priority
            group_ids = @([long]$group.id)
        }
        $account = Get-RouterResponseData $accountResponse
    } else {
        $exchangeResponse = switch ($Provider) {
            'anthropic' {
                Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts/exchange-code' -Body @{
                    session_id = [string]$auth.session_id
                    code = $code.Trim()
                }
            }
            'gemini' {
                $geminiExchangeBody = @{
                    session_id = [string]$auth.session_id
                    state = $state
                    code = $code.Trim()
                    oauth_type = $geminiOAuthType
                    tier_id = $geminiTierID
                }
                if ($geminiProjectID) { $geminiExchangeBody['project_id'] = $geminiProjectID }
                Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/gemini/oauth/exchange-code' -Body $geminiExchangeBody
            }
            'antigravity' {
                Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/antigravity/oauth/exchange-code' -Body @{
                    session_id = [string]$auth.session_id
                    state = $state
                    code = $code.Trim()
                }
            }
            'grok' {
                Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/grok/oauth/exchange-code' -Body @{
                    session_id = [string]$auth.session_id
                    state = $state
                    code = $code.Trim()
                }
            }
        }
        $tokens = Get-RouterResponseData $exchangeResponse
        $tokenMap = ConvertTo-PlainHashtable $tokens
        $extra = @{}
        $credentials = switch ($Provider) {
            'anthropic' {
                $result = @{} + $tokenMap
                [void]$result.Remove('extra')
                foreach ($name in @('org_uuid', 'account_uuid', 'email_address')) {
                    if ($result.Contains($name)) { $extra[$name] = $result[$name] }
                }
                $result
            }
            'gemini' {
                if ($tokenMap.Contains('extra') -and $tokenMap.extra -is [System.Collections.IDictionary]) {
                    $extra = @{} + $tokenMap.extra
                }
                Copy-Fields -Source $tokenMap -Names @(
                    'access_token', 'refresh_token', 'token_type', 'expires_at',
                    'scope', 'project_id', 'oauth_type', 'tier_id'
                )
            }
            'antigravity' {
                Copy-Fields -Source $tokenMap -Names @(
                    'access_token', 'refresh_token', 'token_type', 'expires_at',
                    'project_id', 'email'
                )
            }
            'grok' {
                foreach ($name in @('email', 'subscription_tier', 'entitlement_status')) {
                    if ($tokenMap.Contains($name)) { $extra[$name] = $tokenMap[$name] }
                }
                $result = Copy-Fields -Source $tokenMap -Names @(
                    'access_token', 'refresh_token', 'id_token', 'token_type',
                    'expires_at', 'client_id', 'scope', 'email', 'sub', 'team_id',
                    'subscription_tier', 'entitlement_status'
                )
                $result['base_url'] = 'https://cli-chat-proxy.grok.com/v1'
                $result
            }
        }
        $displayName = switch ($Provider) {
            'anthropic' { 'Claude OAuth' }
            'gemini' { 'Gemini OAuth' }
            'antigravity' { 'Antigravity OAuth' }
            'grok' { 'Grok OAuth' }
        }
        $accountResponse = Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts' -Body @{
            name = $displayName
            notes = 'Created by Codex-Router direct OAuth flow.'
            platform = $Provider
            type = 'oauth'
            credentials = $credentials
            extra = $extra
            concurrency = 3
            priority = $priority
            rate_multiplier = 1
            group_ids = @([long]$group.id)
            auto_pause_on_expired = $false
        }
        $account = Get-RouterResponseData $accountResponse
    }

    Write-Host ''
    Write-Host "OAuth account created: $($account.name) (ID $($account.id))" -ForegroundColor Green
    Write-Host 'Codex-Router will now add this account to the active profile and refresh usage statistics.'
} catch {
    if ($PrepareOnly) {
        Write-OAuthPrepareFailure -Stage $prepareStage -ErrorText $_.Exception.Message
    }
    [void](Complete-OAuthPreparationPage -State $oauthPreparation -FailureMessage 'OAuth preparation failed. Return to Codex-Router and retry.')
    Write-Host ''
    Write-Host "OAuth failed: $($_.Exception.Message)" -ForegroundColor Red
    if ($Provider -in @('anthropic', 'gemini')) {
        [void](Read-Host 'Press Enter to close this window')
    }
    exit 1
} finally {
    if ($listener) { $listener.Stop() }
    $code = $null
    $state = $null
    if ($credentials) { $credentials.Clear() }
    if ($tokenMap) { $tokenMap.Clear() }
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
} catch {
    if ($PrepareOnly) {
        Write-OAuthPrepareFailure -Stage $prepareStage -ErrorText $_.Exception.Message
    }
    [void](Complete-OAuthPreparationPage -State $oauthPreparation -FailureMessage 'The local Router could not start. Return to Codex-Router and retry.')
    Write-Host "OAuth startup failed: $($_.Exception.Message)" -ForegroundColor Red
    if ($Provider -in @('anthropic', 'gemini')) {
        [void](Read-Host 'Press Enter to close this window')
    }
    exit 1
} finally {
    Stop-OAuthPreparationPage -State $oauthPreparation
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
        $previousLifecycleLockMarker,
        'Process')
    Exit-RouterLifecycleLock -Lock $lifecycleLock
}
