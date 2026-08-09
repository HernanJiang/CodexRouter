param([string]$ConfigPath = '')

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
# RouterAdmin nests UserData; re-export the helpers into this session so
# on-demand recovery can resolve data/config paths under StrictMode.
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$configPath = if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    Get-RouterConfigPath -RouterRoot $routerRoot
} else { [IO.Path]::GetFullPath($ConfigPath) }
if (-not (Test-Path -LiteralPath $configPath)) {
    [Console]::Out.WriteLine((@{ nextCheckSeconds = 0; summary = 'configuration missing' } | ConvertTo-Json -Compress))
    exit 0
}
$config = ConvertFrom-Json -InputObject (Get-Content -LiteralPath $configPath -Raw)
$oauthSelectionProperty = $config.PSObject.Properties['oauthAccountIds']
$oauthAccountIds = if ($null -eq $oauthSelectionProperty) { @() } else {
    @($oauthSelectionProperty.Value | ForEach-Object { [long]$_ } | Where-Object { $_ -gt 0 } | Select-Object -Unique)
}
foreach ($model in @($config.models | Where-Object { $_.source -eq 'oauth' -and [long]$_.oauthAccountId -gt 0 })) {
    $id = [long]$model.oauthAccountId
    if ($oauthAccountIds -notcontains $id) { $oauthAccountIds += $id }
}
if (@($oauthAccountIds).Count -eq 0) {
    [Console]::Out.WriteLine((@{ nextCheckSeconds = 0; summary = 'no selected OAuth accounts' } | ConvertTo-Json -Compress))
    exit 0
}

$session = New-RouterAdminSession
$nextCheckSeconds = [long]::MaxValue
$deferred = 0
$probed = 0
$recovered = 0
$isolated = 0
$healthy = 0
$probeIntervalSeconds = 5L * 60L * 60L
$observationPath = Join-Path (Get-RouterDataRoot -RouterRoot $routerRoot) 'state\oauth-recovery-observations.json'
$observations = @{}
if (Test-Path -LiteralPath $observationPath) {
    try {
        $saved = Get-Content -LiteralPath $observationPath -Raw | ConvertFrom-Json
        foreach ($entry in @($saved.entries)) {
            if ([long]$entry.accountId -gt 0) { $observations[[long]$entry.accountId] = $entry }
        }
    } catch { $observations = @{} }
}
try {
    $group = @(Get-RouterGroups -Session $session | Where-Object { [string]$_.name -eq 'Codex-Router' } | Select-Object -First 1)
    if ($group.Count -eq 0) { throw 'Codex-Router group was not found.' }
    $groupId = [long]$group[0].id

    foreach ($accountId in $oauthAccountIds) {
        try {
            $account = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId")
            if ([string]$account.type -ne 'oauth') { continue }
            $observation = if ($observations.ContainsKey($accountId)) { $observations[$accountId] } else { $null }
            $observedResetAt = if ($null -eq $observation) { $null } else { $observation.resetAt }
            $state = Get-RouterOAuthRecoveryState -Account $account `
                -ObservedResetAt $observedResetAt `
                -ObservedExhausted:($null -ne $observation -and [bool]$observation.exhausted)
            $nextCheckSeconds = [Math]::Min($nextCheckSeconds, [long]$state.NextCheckSeconds)

            if ($state.Action -eq 'defer') {
                if (Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $false -Account $account) {
                    $isolated++
                }
                $deferred++
                continue
            }
            if ($state.Action -eq 'healthy') {
                [void](Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $true -Account $account)
                $healthy++
                continue
            }

            $nextProbeProperty = if ($null -eq $observation) { $null } else { $observation.PSObject.Properties['nextProbeAt'] }
            $nextProbeAt = if ($null -eq $nextProbeProperty) { $null } else {
                ConvertTo-RouterResetAtUtc -Value $nextProbeProperty.Value
            }
            if ($null -ne $nextProbeAt -and $nextProbeAt -gt [DateTimeOffset]::UtcNow) {
                [void](Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $false -Account $account)
                $remaining = [Math]::Max(1L, [long][Math]::Ceiling(($nextProbeAt - [DateTimeOffset]::UtcNow).TotalSeconds))
                $nextCheckSeconds = [Math]::Min($nextCheckSeconds, $remaining)
                $deferred++
                continue
            }

            if (Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $false -Account $account) {
                $isolated++
            }
            $availableModelData = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId/models" -TimeoutSec 15)
            $modelId = [string](@($availableModelData | ForEach-Object {
                $idProperty = $_.PSObject.Properties['id']
                if ($null -ne $idProperty) { [string]$idProperty.Value }
            } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1))
            if ([string]::IsNullOrWhiteSpace($modelId)) { continue }
            $probed++
            $probeAt = [DateTimeOffset]::UtcNow
            $priorResetProperty = if ($null -eq $observation) { $null } else { $observation.PSObject.Properties['resetAt'] }
            $priorObservedProperty = if ($null -eq $observation) { $null } else { $observation.PSObject.Properties['observedAt'] }
            $observations[$accountId] = [pscustomobject][ordered]@{
                accountId = $accountId
                exhausted = $true
                resetAt = if ($null -eq $priorResetProperty) { '' } else { [string]$priorResetProperty.Value }
                observedAt = if ($null -eq $priorObservedProperty) { $probeAt.UtcDateTime.ToString('o') } else { [string]$priorObservedProperty.Value }
                lastProbeAt = $probeAt.UtcDateTime.ToString('o')
                nextProbeAt = $probeAt.AddSeconds($probeIntervalSeconds).UtcDateTime.ToString('o')
                recentError = ''
            }
            $nextCheckSeconds = [Math]::Min($nextCheckSeconds, $probeIntervalSeconds)
            try {
                $testResult = Get-RouterResponseData (Invoke-RouterApi `
                    -Session $session `
                    -Method POST `
                    -Path "/api/v1/admin/accounts/$accountId/test" `
                    -Body @{ model_id = $modelId; prompt = 'Reply with OK.'; mode = 'minimal' } `
                    -TimeoutSec 90)
                $successProperty = if ($null -eq $testResult) { $null } else { $testResult.PSObject.Properties['success'] }
                if ($null -ne $successProperty -and -not [bool]$successProperty.Value) {
                    throw 'OAuth recovery probe was rejected by the upstream provider.'
                }
                $refreshed = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId")
                [void](Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $true -Account $refreshed)
                [void]$observations.Remove($accountId)
                $recovered++
            } catch {
                $observations[$accountId].recentError = 'probe_failed'
            }
        } catch {
            $nextCheckSeconds = [Math]::Min($nextCheckSeconds, $probeIntervalSeconds)
        }
    }
} finally {
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}

$observationDocument = [ordered]@{ entries = @($observations.Values) } | ConvertTo-Json -Depth 6
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1')
Write-RouterTextFileAtomic -Path $observationPath -Text $observationDocument

# Membership changed, so the Codex menu, the group model list, and the composite
# routes must be realigned immediately. Otherwise a recovered subscription stays
# invisible until the next manual Apply, and a newly parked one keeps a route
# that can only answer 503.
if (($recovered + $isolated) -gt 0) {
    try {
        & (Join-Path $PSScriptRoot 'Sync-RouterRoutingState.ps1') -ConfigPath $configPath -Quiet |
            ForEach-Object { [Console]::Error.WriteLine([string]$_) }
    } catch {
        [Console]::Error.WriteLine('CR-FLAG ROUTING-SYNC-FAILED reason=recovery-followup')
    }
}

$result = [ordered]@{
    nextCheckSeconds = if ($nextCheckSeconds -eq [long]::MaxValue) { 0L } else { $nextCheckSeconds }
    summary = "healthy=$healthy deferred=$deferred probed=$probed recovered=$recovered isolated=$isolated"
    routingSynced = ($recovered + $isolated) -gt 0
}
[Console]::Out.WriteLine(($result | ConvertTo-Json -Compress))
