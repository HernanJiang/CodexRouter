Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force

$secretInput = [Console]::In.ReadToEnd()
$tokens = @($secretInput -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
if ($tokens.Count -eq 0) { throw 'No Grok authorization code / SSO token was provided.' }

$session = New-RouterAdminSession
try {
    $group = Get-RouterGroups -Session $session |
        Where-Object { $_.name -eq 'Codex-Router' } |
        Select-Object -First 1
    if (-not $group) { throw 'Apply a Codex-Router configuration before importing Grok authorization.' }

    $priority = 1
    $configPath = Get-RouterConfigPath -RouterRoot $routerRoot
    if (Test-Path -LiteralPath $configPath) {
        $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
        $priority = [int](Get-RouterOAuthRoutingPriorities -OAuthFallback $config.oauthFallback).OAuthPriority
    }

    $response = Invoke-RouterApi `
        -Session $session `
        -Method POST `
        -Path '/api/v1/admin/grok/sso-to-oauth' `
        -TimeoutSec ([Math]::Min(300, 90 + 30 * $tokens.Count)) `
        -Body @{
            sso_tokens = $tokens
            name = 'Grok OAuth'
            notes = 'Imported by Codex-Router using an authorization code / SSO token.'
            group_ids = @([long]$group.id)
            credentials = @{}
            concurrency = 3
            priority = $priority
            rate_multiplier = 1
            auto_pause_on_expired = $false
        }
    $data = Get-RouterResponseData $response
    $created = @($data.created)
    $failed = @($data.failed)
    foreach ($account in $created) {
        $idProperty = $account.PSObject.Properties['id']
        if ($null -ne $idProperty -and [long]$idProperty.Value -gt 0) {
            try {
                [void](Set-RouterScheduledRecovery -Session $session -AccountId ([long]$idProperty.Value) -ModelId 'grok-4.5')
            } catch { }
        }
    }
    [ordered]@{
        created = $created.Count
        failed = $failed.Count
        message = if ($created.Count -gt 0) { 'Grok authorization imported.' } else { 'No Grok account was created.' }
        errors = @($failed | ForEach-Object {
            $errorProperty = $_.PSObject.Properties['error']
            if ($null -ne $errorProperty) { [string]$errorProperty.Value } else { 'Unknown conversion error' }
        })
    } | ConvertTo-Json -Depth 5 -Compress
    if ($created.Count -eq 0) { exit 2 }
} finally {
    $secretInput = $null
    if ($tokens) {
        for ($index = 0; $index -lt $tokens.Count; $index++) { $tokens[$index] = $null }
    }
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
