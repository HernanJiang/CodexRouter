# Routing-only reconciliation. Recomputes the Codex model menu, the Sub2API
# group model list, and the composite routes from the live OAuth quota state.
# It never touches services, credentials, or API channels, so it is safe to run
# right after an OAuth quota recovery probe without adding request latency.
param(
    [string]$ConfigPath = '',
    [switch]$Quiet
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force

$configPath = if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    Get-RouterConfigPath -RouterRoot $routerRoot
} else { [IO.Path]::GetFullPath($ConfigPath) }
if (-not (Test-Path -LiteralPath $configPath)) {
    Write-Output 'CR-FLAG ROUTING-SYNC-SKIPPED reason=configuration-missing'
    exit 0
}

$catalogPath = Join-Path (Get-RouterUserDataRoot -RouterRoot $routerRoot) 'model-catalog.json'
$packageCatalogPath = Join-Path $routerRoot 'config\model-catalog.json'
$config = ConvertFrom-Json -InputObject (Get-Content -LiteralPath $configPath -Raw)

$oauthSelectionProperty = $config.PSObject.Properties['oauthAccountIds']
$oauthSelectionInitialized = $null -ne $oauthSelectionProperty
$oauthAccountIds = if ($oauthSelectionInitialized) {
    @($oauthSelectionProperty.Value | ForEach-Object { [long]$_ } | Where-Object { $_ -gt 0 } | Select-Object -Unique)
} else { @() }

$session = New-RouterAdminSession
$group = @(Get-RouterGroups -Session $session | Where-Object { [string]$_.name -eq 'Codex-Router' } | Select-Object -First 1)
if ($group.Count -eq 0) {
    Write-Output 'CR-FLAG ROUTING-SYNC-SKIPPED reason=group-missing'
    exit 0
}
$groupId = [long]$group[0].id

$routePlan = @(Get-RouterModelRoutePlan -RouterConfig $config -DiscoveredOAuthModelsByAccount @{})
$configModels = @(if ($null -ne $config.PSObject.Properties['models']) { $config.models })
$isolatedOAuthAccountIds = @{}
$membershipChanges = 0
foreach ($accountId in $oauthAccountIds) {
    try {
        $account = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId")
        if ([string]$account.type -ne 'oauth') { continue }
        $state = Get-RouterOAuthRecoveryState -Account $account
        $hasModels = @($configModels | Where-Object {
            $sourceProperty = $_.PSObject.Properties['source']
            $source = if ($null -eq $sourceProperty) { 'apikey' } else { ([string]$sourceProperty.Value).Trim().ToLowerInvariant() }
            $accountProperty = $_.PSObject.Properties['oauthAccountId']
            $modelAccount = if ($null -eq $accountProperty) { 0 } else { [long]$accountProperty.Value }
            $source -eq 'oauth' -and $modelAccount -eq [long]$accountId
        }).Count -gt 0
        if ([bool]$state.ShouldIsolate) {
            $isolatedOAuthAccountIds[$accountId] = [string]$state.Reason
        }
        # Self-healing membership: the subscription is in the live pool only while
        # it has imported models and usable quota. This keeps a recovered account
        # preferred again and a drained one fully out of the request path.
        $shouldBeMember = $hasModels -and -not [bool]$state.ShouldIsolate
        if (Set-RouterAccountGroupMembership -Session $session -AccountId $accountId -GroupId $groupId -Enabled $shouldBeMember -Account $account) {
            $membershipChanges++
            Write-Output ("CR-FLAG {0} account={1} platform={2}" -f
                $(if ($shouldBeMember) { 'OAUTH-REJOINED' } else { 'OAUTH-PARKED' }),
                $accountId,
                [string]$account.platform)
        }
    } catch {
        Write-Output "CR-FLAG ROUTING-SYNC-ACCOUNT-UNREADABLE account=$accountId"
    }
}

$servableRoutes = @(Get-RouterServableCatalogRoutes `
    -RoutePlan $routePlan `
    -IsolatedOAuthAccountIds $isolatedOAuthAccountIds `
    -OAuthAccountIds $oauthAccountIds `
    -OAuthSelectionInitialized:$oauthSelectionInitialized)
if (@($servableRoutes).Count -eq 0) {
    Write-Output 'CR-FLAG ROUTING-SYNC-SKIPPED reason=no-servable-model'
    exit 0
}
$servableRoutingRoutes = @(Get-RouterServableRoutingRoutes `
    -RoutePlan $routePlan `
    -IsolatedOAuthAccountIds $isolatedOAuthAccountIds `
    -OAuthAccountIds $oauthAccountIds `
    -OAuthSelectionInitialized:$oauthSelectionInitialized)
$servableModelNames = @($servableRoutes | ForEach-Object { [string]$_.PublicModelId } | Where-Object { $_ } | Select-Object -Unique)

$currentModelNames = @()
$listProperty = $group[0].PSObject.Properties['models_list_config']
if ($null -ne $listProperty -and $null -ne $listProperty.Value) {
    $modelsProperty = $listProperty.Value.PSObject.Properties['models']
    if ($null -ne $modelsProperty) { $currentModelNames = @($modelsProperty.Value | ForEach-Object { [string]$_ }) }
}
$listChanged = @(Compare-Object -ReferenceObject @($currentModelNames) -DifferenceObject @($servableModelNames)).Count -gt 0
if ($listChanged) {
    $groupBody = @{
        name = 'Codex-Router'
        description = 'Single-user local Codex multi-model router managed by Codex-Router.'
        platform = 'composite'
        rate_multiplier = 1.0
        is_exclusive = $false
        subscription_type = 'standard'
        status = 'active'
        allow_messages_dispatch = $false
        allow_live = $false
        require_oauth_only = $false
        models_list_config = @{ enabled = $true; models = @($servableModelNames) }
    }
    [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/groups/$groupId" -Body $groupBody)
}

$accountPlatformById = Get-RouterAccountPlatformMap -Session $session
$compositeRoutes = @(Get-RouterCompositeRoutePlan `
    -RoutePlan $servableRoutingRoutes `
    -AccountPlatformById $accountPlatformById `
    -ExcludedOAuthAccountIds @($isolatedOAuthAccountIds.Keys))
$compositeSync = Sync-RouterCompositeRoutes `
    -Session $session `
    -GroupId $groupId `
    -CompositeRoutes $compositeRoutes

& (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
    -ConfigPath $configPath `
    -OutputPath $catalogPath `
    -DiscoveredOAuthModelsByAccount @{} `
    -RoutePlan $servableRoutes | Out-Null
[IO.Directory]::CreateDirectory((Split-Path -Parent $packageCatalogPath)) | Out-Null
Copy-Item -LiteralPath $catalogPath -Destination $packageCatalogPath -Force

foreach ($route in $servableRoutes) {
    $servedBy = if ($null -eq $route.PSObject.Properties['ServedBy']) { 'api' } else { [string]$route.ServedBy }
    Write-Output "CR-FLAG ROUTING-SYNC-MODEL model=$([string]$route.PublicModelId) served=$servedBy"
}
Write-Output (
    'CR-FLAG ROUTING-SYNC-OK models={0} listChanged={1} created={2} updated={3} removed={4} parked={5} membership={6}' -f
    @($servableModelNames).Count,
    $(if ($listChanged) { 'yes' } else { 'no' }),
    [int]$compositeSync.Created,
    [int]$compositeSync.Updated,
    [int]$compositeSync.Removed,
    $isolatedOAuthAccountIds.Count,
    $membershipChanges)
if (-not $Quiet) {
    Write-Output "Routing state synchronized: $(@($servableModelNames).Count) servable model(s)."
}
