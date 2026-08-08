Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$configPath = Get-RouterConfigPath -RouterRoot $routerRoot
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot
# Stable catalog path: package folders change between releases, but Codex must
# keep reading the same model menu after Apply/restart. Resolve it here, while
# UserData.psm1 is still imported in this scope.
$catalogPath = Join-Path (Get-RouterUserDataRoot -RouterRoot $routerRoot) 'model-catalog.json'
$packageCatalogPath = Join-Path $routerRoot 'config\model-catalog.json'
if (-not (Test-Path -LiteralPath $configPath)) { throw "Configuration not found: $configPath" }

Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CodexIntegration.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ProxyDiscovery.psm1') -Force
$configLock = Enter-RouterConfigLock -RouterRoot $routerRoot -TimeoutMilliseconds 10000
$previousLockMarker = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', 'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', '1', 'Process')
$lifecycleLock = $null
$previousLifecycleLockMarker = [Environment]::GetEnvironmentVariable(
    'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
    'Process')
$session = $null
$localKey = $null
try {
$lifecycleLock = Enter-RouterLifecycleLock `
    -RouterRoot $routerRoot `
    -TimeoutMilliseconds 10000 `
    -Operation 'Apply Router configuration'
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_LIFECYCLE_LOCK_HELD', [string]$PID, 'Process')
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$sub2apiPidPath = Join-Path $dataRoot 'pids\sub2api.pid'
$sub2apiPid = 0
if (Test-Path -LiteralPath $sub2apiPidPath) {
    [void][int]::TryParse(
        ([IO.File]::ReadAllText($sub2apiPidPath)).Trim(),
        [ref]$sub2apiPid)
}
if ($sub2apiPid -gt 0) {
    $sub2apiProcess = Get-Process -Id $sub2apiPid -ErrorAction SilentlyContinue
    if ($null -ne $sub2apiProcess) {
        $expectedSub2Api = [IO.Path]::GetFullPath((Join-Path $routerRoot 'app\sub2api.exe'))
        $actualSub2Api = try { [IO.Path]::GetFullPath($sub2apiProcess.Path) } catch { '' }
        if ($actualSub2Api.Equals($expectedSub2Api, [StringComparison]::OrdinalIgnoreCase)) {
            $sub2apiUri = [Uri](Get-RouterBaseUri)
            Assert-RouterServiceInterruptionAllowed `
                -ProcessId $sub2apiPid `
                -Port $sub2apiUri.Port `
                -Operation 'Apply Router configuration'
        }
    }
}
$proxyProperty = $config.PSObject.Properties['proxy']
$proxyConfig = if ($null -eq $proxyProperty) { $null } else { $proxyProperty.Value }
$proxySettings = Resolve-RouterProxySettings -ProxyConfig $proxyConfig -ProxyPassword $null
if ($proxySettings.Mode -eq 'unsupported') {
    throw "ROUTER_PROXY_UNSUPPORTED: $($proxySettings.Diagnostic)"
}
if ($null -ne $proxySettings.ProxyUrl -and [bool]$proxySettings.HasCredentials) {
    throw 'ROUTER_PROXY_CREDENTIAL_STORAGE_UNSUPPORTED: The detected proxy uses authentication. This release will not copy proxy credentials into the Sub2API database.'
}
$models = @($config.models)
if ($models.Count -eq 0) {
    throw "ROUTER_DEPLOY_NO_MODELS: at least one model is required, but $configPath has none."
}
foreach ($configuredModel in $models) {
    if ([string]$configuredModel.model -eq 'gpt-5.6') {
        $configuredModel.model = 'gpt-5.6-sol'
        if ([string]$configuredModel.alias -in @('gpt-5.6', 'GPT-5.6 (Sol)')) {
            $configuredModel.alias = 'ChatGPT-5.6-Sol'
        }
    }
}
if ([string]$config.defaultModel -eq 'gpt-5.6') { $config.defaultModel = 'gpt-5.6-sol' }
$routePlan = @(Get-RouterModelRoutePlan -RouterConfig $config)
$defaultModel = Get-RouterDefaultPublicModelId -RouterConfig $config -RoutePlan $routePlan

function New-RandomLocalKey {
    $buffer = [byte[]]::new(32)
    $generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $generator.GetBytes($buffer)
        return 'sk-local-' + ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant()
    } finally {
        $generator.Dispose()
        [Array]::Clear($buffer, 0, $buffer.Length)
    }
}

function Get-ModelSource($Model) {
    $property = $Model.PSObject.Properties['source']
    if ($null -eq $property -or [string]::IsNullOrWhiteSpace([string]$property.Value)) { return 'apikey' }
    return ([string]$property.Value).Trim().ToLowerInvariant()
}

# Stable, greppable deployment flags. Every value is a machine-generated id,
# count, platform, or an already sanitized reason - never a secret or a path.
function Write-RouterFlag {
    param(
        [Parameter(Mandatory)][string]$Code,
        [hashtable]$Data = @{}
    )
    $sanitize = {
        param([string]$Value)
        $text = ([string]$Value).Trim()
        if ([string]::IsNullOrWhiteSpace($text)) { return 'none' }
        $text = [Text.RegularExpressions.Regex]::Replace($text, '\s+', '-')
        $text = [Text.RegularExpressions.Regex]::Replace($text, '[^A-Za-z0-9._:/~+-]', '')
        if ($text.Length -gt 80) { $text = $text.Substring(0, 80) }
        if ([string]::IsNullOrWhiteSpace($text)) { return 'none' }
        return $text
    }
    $pairs = @($Data.GetEnumerator() | Sort-Object Name | ForEach-Object {
        "$($_.Key)=$(& $sanitize ([string]$_.Value))"
    })
    if ($pairs.Count -gt 0) {
        Write-Output ("CR-FLAG $Code " + ($pairs -join ' '))
    } else {
        Write-Output "CR-FLAG $Code"
    }
}

Write-Output '[1/7] Initializing local credentials and database...'
& (Join-Path $PSScriptRoot 'Initialize-Router.ps1')
Write-RouterFlag 'STAGE-01-INIT-OK'
Write-Output '[2/7] Starting PostgreSQL, Redis, and Sub2API...'
& (Join-Path $PSScriptRoot 'Start-Router.ps1') -RepairUnhealthy
Write-RouterFlag 'STAGE-02-SERVICES-OK'
Write-Output '[3/7] Local services are ready; signing in to the admin API...'
$session = New-RouterAdminSession
Write-RouterFlag 'STAGE-03-ADMIN-OK'
Write-Output '[4/7] Checking Sub2API compliance status...'
$compliance = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/compliance')
if ($compliance.required) {
    $acceptedProperty = $config.PSObject.Properties['acceptCompliance']
    if ($null -eq $acceptedProperty -or -not [bool]$acceptedProperty.Value) {
        throw 'You must read and accept the Sub2API deployment and operation compliance commitment in Codex-Router.'
    }
    [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/compliance/accept' -Body @{
        phrase = [string]$compliance.ack_phrase_zh
        language = 'zh'
    })
    Write-Output 'Sub2API compliance acknowledgement recorded for this local administrator.'
}
& (Join-Path $PSScriptRoot 'Ensure-Sub2ApiAdmin.ps1') | Write-Output
$session.Headers.Clear()
$session = New-RouterAdminSession

$existingAccounts = @(Get-RouterAccounts -Session $session)
$oauthSelectionProperty = $config.PSObject.Properties['oauthAccountIds']
$oauthSelectionInitialized = $null -ne $oauthSelectionProperty
$oauthAccountIds = if ($oauthSelectionInitialized) {
    @($oauthSelectionProperty.Value | ForEach-Object { [long]$_ } | Where-Object { $_ -gt 0 } | Select-Object -Unique)
} else { @() }
$routePriorities = Get-RouterOAuthRoutingPriorities -OAuthFallback $config.oauthFallback
$officialPriority = [int]$routePriorities.OAuthPriority
$fallbackPriority = [int]$routePriorities.ApiPriority
$fallbackSelectionsProperty = $config.PSObject.Properties['fallbackChannelSelections']
$fallbackChannelSelections = if ($null -eq $fallbackSelectionsProperty) {
    $null
} else {
    $fallbackSelectionsProperty.Value
}
$explicitSelectedOAuthModels = @($models | Where-Object {
    (Get-ModelSource $_) -eq 'oauth' -and
    (-not $oauthSelectionInitialized -or $oauthAccountIds -contains [long]$_.oauthAccountId)
})
$discoveredOAuthModelsByAccount = @{}
$oauthModelDiscoveryFailures = @{}
if ([bool]$routePriorities.Enabled) {
    foreach ($oauthAccountId in $oauthAccountIds) {
        try {
            $availableModelData = Get-RouterResponseData (Invoke-RouterApi `
                -Session $session `
                -Method GET `
                -Path "/api/v1/admin/accounts/$oauthAccountId/models" `
                -TimeoutSec 15)
            $itemsProperty = if ($null -eq $availableModelData) {
                $null
            } else {
                $availableModelData.PSObject.Properties['items']
            }
            $availableModels = if ($null -ne $itemsProperty) {
                @($itemsProperty.Value)
            } else {
                @($availableModelData)
            }
            $availableModelIds = @($availableModels | ForEach-Object {
                if ($null -eq $_) { return }
                $idProperty = $_.PSObject.Properties['id']
                if ($null -ne $idProperty -and
                    -not [string]::IsNullOrWhiteSpace([string]$idProperty.Value)) {
                    ([string]$idProperty.Value).Trim()
                }
            } | Select-Object -Unique)
            $discoveredOAuthModelsByAccount[[string]$oauthAccountId] = $availableModelIds
            if ($availableModelIds.Count -eq 0) {
                Write-Warning "OAuth account $oauthAccountId returned no discoverable models; implicit API fallback matching was skipped."
            }
        } catch {
            $oauthModelDiscoveryFailures[[string]$oauthAccountId] = $_.Exception.Message
            Write-Warning "Could not discover models for OAuth account $oauthAccountId; explicit model configuration remains active: $($_.Exception.Message)"
        }
    }
}

# Only manually imported OAuth model rows route or recover. Account enrollment
# alone keeps the credential available in the OAuth manager without overriding
# the user's configured third-party model list.
$selectedOAuthModels = @($explicitSelectedOAuthModels)

$routePlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $config `
    -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount)
$defaultModel = Get-RouterDefaultPublicModelId -RouterConfig $config -RoutePlan $routePlan
& (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
    -ConfigPath $configPath `
    -OutputPath $catalogPath `
    -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount | Write-Output
[IO.Directory]::CreateDirectory((Split-Path -Parent $packageCatalogPath)) | Out-Null
Copy-Item -LiteralPath $catalogPath -Destination $packageCatalogPath -Force

$modelNames = @($routePlan | Where-Object { $_.IncludeInCatalog } | ForEach-Object {
    [string]$_.PublicModelId
} | Where-Object { $_ } | Select-Object -Unique)
Write-RouterFlag 'STAGE-04-COMPLIANCE-OK'
Write-Output '[5/7] Creating or updating model channels...'
$groupName = 'Codex-Router'
$groups = @(Get-RouterGroups -Session $session)
$group = $groups | Where-Object { $_.name -eq $groupName } | Select-Object -First 1
$groupBody = @{
    name = $groupName
    description = 'Single-user local Codex multi-model router managed by Codex-Router.'
    # Sub2API schedules OAuth accounts by group platform. A single openai group
    # cannot select grok/antigravity/gemini OAuth accounts, so the Router group
    # is composite and each public model gets an explicit composite route.
    platform = 'composite'
    rate_multiplier = 1.0
    is_exclusive = $false
    subscription_type = 'standard'
    status = 'active'
    allow_messages_dispatch = $false
    allow_live = $false
    require_oauth_only = $false
    models_list_config = @{ enabled = $true; models = $modelNames }
}
if ($group) {
    $group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/groups/$($group.id)" -Body $groupBody)
} else {
    $group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/groups' -Body $groupBody -IdempotencyKey 'codex-router-group-v3-composite')
}
$groupId = [long]$group.id
$accountPlatformById = Get-RouterAccountPlatformMap -Session $session
$compositeRoutes = @(Get-RouterCompositeRoutePlan `
    -RoutePlan $routePlan `
    -AccountPlatformById $accountPlatformById)
$compositeSync = Sync-RouterCompositeRoutes `
    -Session $session `
    -GroupId $groupId `
    -CompositeRoutes $compositeRoutes
Write-Output (
    'Composite routes: desired={0}; created={1}; updated={2}; removed={3}' -f
    [int]$compositeSync.Desired,
    [int]$compositeSync.Created,
    [int]$compositeSync.Updated,
    [int]$compositeSync.Removed)
$managedAccountNames = @()
$managedAccountTargets = @{}
$directReachabilityByTarget = @{}

for ($modelIndex = 0; $modelIndex -lt $models.Count; $modelIndex++) {
    $model = $models[$modelIndex]
    $route = $routePlan[$modelIndex]
    $source = Get-ModelSource $model
    if ($source -eq 'oauth') { continue }
    $credentialName = [string]$model.credentialName
    if ([string]::IsNullOrWhiteSpace($credentialName)) { throw "Model '$($model.model)' has no credential reference." }
    $apiKey = Get-RouterCredential -Name $credentialName -AllowMissing
    if ([string]::IsNullOrWhiteSpace($apiKey)) { throw "Missing API Key for model '$($model.model)'. Edit the model and enter its API Key." }
    try {
        $accountName = 'Codex-Router / ' + $(if ($model.alias) { [string]$model.alias } else { [string]$model.model })
        $managedAccountNames += $accountName
        $managedAccountTargets[$accountName] = ([string]$model.baseURL).TrimEnd('/')
        $upstreamModelId = Get-RouterUpstreamModelId `
            -ModelId ([string]$model.model) `
            -BaseUrl ([string]$model.baseURL)
        $mapping = [ordered]@{}
        foreach ($requestModelId in @($route.RequestModelIds)) {
            $mapping[[string]$requestModelId] = $upstreamModelId
        }
        # Also accept the raw configured id when normalization rewrites it
        # (for example OpenRouter claude/* -> anthropic/*).
        if (-not $mapping.Contains([string]$model.model)) {
            $mapping[[string]$model.model] = $upstreamModelId
        }
        $canonicalModelID = Get-RouterCanonicalModelId -ModelId ([string]$model.model)
        $isOAuthFallback = [bool]$route.IsOAuthFallback
        $shouldJoinRouter = [bool]$route.JoinRouter
        $effectivePriority = [Math]::Max(1, [int]$model.priority)
        if ($isOAuthFallback) {
            $matchingSelectedModels = @($models | Where-Object {
                (Get-ModelSource $_) -ne 'oauth' -and
                (Test-RouterSameModel -LeftModelId ([string]$_.model) -RightModelId ([string]$model.model)) -and
                (Test-RouterFallbackChannelSelected `
                    -Selections $fallbackChannelSelections `
                    -ModelId ([string]$_.model) `
                    -BaseUrl ([string]$_.baseURL))
            })
            $matchingSelectedPriorities = @($matchingSelectedModels | ForEach-Object { [Math]::Max(1, [int]$_.priority) })
            $minimumMatchingPriority = [int](
                $matchingSelectedPriorities | Measure-Object -Minimum).Minimum
            $effectivePriority = Get-RouterEffectiveApiPriority `
                -ConfiguredPriority ([Math]::Max(1, [int]$model.priority)) `
                -MinimumMatchingPriority $minimumMatchingPriority `
                -ApiBasePriority $fallbackPriority `
                -OAuthPriority $officialPriority `
                -PreferOAuth ([bool]$routePriorities.PreferOAuth)
        }
        # Preserve user order inside each tier while enforcing subscription
        # OAuth -> Coding Plan -> third-party API across all route shapes.
        $effectivePriority += 1000 * (Get-RouterChannelTier -Model $model)
        $existing = $existingAccounts | Where-Object { $_.name -eq $accountName } | Select-Object -First 1
        $channelGroupIds = @()
        if ($existing) {
            $existingDetail = Get-RouterResponseData (Invoke-RouterApi `
                -Session $session `
                -Method GET `
                -Path "/api/v1/admin/accounts/$($existing.id)")
            $existingGroupsProperty = $existingDetail.PSObject.Properties['group_ids']
            if ($null -ne $existingGroupsProperty) {
                $channelGroupIds = @($existingGroupsProperty.Value | ForEach-Object { [long]$_ })
            }
            $channelGroupIds = @($channelGroupIds | Where-Object { $_ -ne $groupId })
        }
        if ($shouldJoinRouter) { $channelGroupIds += $groupId }
        $credentials = @{
            base_url = ([string]$model.baseURL).TrimEnd('/')
            api_key = $apiKey
            model_mapping = $mapping
        }
        $extra = @{}
        if ($model.extra -and ([string]$model.extra).Trim() -ne '{}') {
            $extraObject = ([string]$model.extra) | ConvertFrom-Json
            foreach ($property in $extraObject.PSObject.Properties) { $extra[$property.Name] = $property.Value }
        }
        $channelPolicy = Get-RouterOpenAIChannelPolicy `
            -BaseUrl ([string]$model.baseURL) `
            -Extra $extra `
            -ModelId ([string]$model.model)
        $extra = $channelPolicy.Extra
        if (@($channelPolicy.OpenAICapabilities).Count -gt 0) {
            $credentials.openai_capabilities = @($channelPolicy.OpenAICapabilities)
        }
        # Marks only API channels participating in OAuth failover. Sub2API uses
        # this provenance to remove OAuth-bound response/reasoning artifacts when
        # the exhausted OAuth account was pre-isolated before request selection.
        $extra.codex_router_oauth_fallback = [bool]$isOAuthFallback
        $targetBaseUrl = ([string]$model.baseURL).TrimEnd('/')
        $directFallbackEligible = $false
        if ([string]$proxySettings.Mode -eq 'proxy') {
            if (-not $directReachabilityByTarget.ContainsKey($targetBaseUrl)) {
                $directReachabilityByTarget[$targetBaseUrl] = Test-RouterDirectTargetReachability `
                    -TargetUri $targetBaseUrl
            }
            $directFallbackEligible = Test-RouterDirectFallbackEligible `
                -ProxySettings $proxySettings `
                -TargetUri $targetBaseUrl `
                -DirectReachable ([bool]$directReachabilityByTarget[$targetBaseUrl])
        }
        # This flag permits one same-account direct retry only for a verified
        # pre-HTTP proxy/DNS/TCP/TLS connection failure. Provider HTTP 5xx
        # responses remain terminal for this logical provider attempt.
        $extra.proxy_direct_fallback = [bool]$directFallbackEligible
        $body = @{
            name = $accountName
            platform = 'openai'
            type = 'apikey'
            credentials = $credentials
            extra = $extra
            concurrency = 8
            priority = $effectivePriority
            rate_multiplier = 1.0
            group_ids = @($channelGroupIds | Select-Object -Unique)
            confirm_mixed_channel_risk = $true
        }
        if ($existing) {
            [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/accounts/$($existing.id)" -Body $body)
            Write-Output "Updated channel: $accountName"
        Write-RouterFlag $(if ($isOAuthFallback) { 'API-FALLBACK-CHANNEL' } else { 'API-CHANNEL' }) @{ model = [string]$route.PublicModelId; priority = $effectivePriority; joined = $(if ($shouldJoinRouter) { 'yes' } else { 'no' }) }
        } else {
            [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts' -Body $body)
            Write-Output "Created channel: $accountName"
        Write-RouterFlag $(if ($isOAuthFallback) { 'API-FALLBACK-CHANNEL' } else { 'API-CHANNEL' }) @{ model = [string]$route.PublicModelId; priority = $effectivePriority; joined = $(if ($shouldJoinRouter) { 'yes' } else { 'no' }) }
        }

        # A proxy is a transport property of this logical channel, not a second
        # quota account. Duplicating one URL/key/model behind proxy and direct
        # accounts replays application-level 5xx responses to the same provider
        # and lets one upstream incident cool the complete fallback pool.
    } finally {
        $apiKey = $null
    }
}

# A route profile owns one complete set of OAuth bindings. Remove stale managed
# API-key channels and unselected OAuth accounts from the Router group while
# preserving any unrelated Sub2API group memberships.
$existingAccounts = @(Get-RouterAccounts -Session $session)
$managedProxyState = Sync-RouterManagedProxy -Session $session -ProxySettings $proxySettings
$staleLocalProxyIds = @()
try {
    $proxyResponse = Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/proxies?page=1&page_size=200'
    $proxyData = Get-RouterResponseData -Response $proxyResponse
    $knownProxies = if ($null -ne $proxyData.PSObject.Properties['items']) { @($proxyData.items) } else { @($proxyData) }
    $staleLocalProxyIds = @($knownProxies | Where-Object {
        [string]$_.host -eq '127.0.0.1' -and
        [int]$_.port -in @(17897, 17898) -and
        [string]$_.name -in @('Local Adaptive HTTP', 'Local Clash HTTP')
    } | ForEach-Object { [long]$_.id })
} catch {
    Write-Warning 'Could not audit legacy local account proxies; existing custom proxy assignments were left unchanged.'
}
$routerManagedProxyIds = @(
    @($staleLocalProxyIds)
    [long]$managedProxyState.ManagedProxyId
) | Where-Object { $_ -gt 0 } | Select-Object -Unique
$proxyAssigned = 0
$proxyReplaced = 0
$proxyCleared = 0
$proxyCustomPreserved = 0
$proxyBypassed = 0
$isolatedOAuthAccountIds = @{}
foreach ($accountSummary in $existingAccounts) {
    $accountID = [long]$accountSummary.id
    # The list endpoint intentionally returns a compact account summary and
    # may omit group_ids. Always load the account detail before reconciling
    # Router membership so StrictMode cannot abort a valid deployment.
    $account = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountID")
    $groupIdsProperty = $account.PSObject.Properties['group_ids']
    $groupIds = if ($null -eq $groupIdsProperty) { @() } else { @($groupIdsProperty.Value | ForEach-Object { [long]$_ }) }
    $isRouterMember = $groupIds -contains $groupId
    $proxyIdProperty = $account.PSObject.Properties['proxy_id']
    $currentProxyId = if ($null -eq $proxyIdProperty -or $null -eq $proxyIdProperty.Value) {
        $null
    } else {
        [long]$proxyIdProperty.Value
    }
    if ([string]$account.type -eq 'oauth') {
        if (-not $oauthSelectionInitialized) { continue }
        $selectedByConfig = $oauthAccountIds -contains $accountID
        $accountOAuthModels = @($explicitSelectedOAuthModels | Where-Object {
            [long]$_.oauthAccountId -eq $accountID
        })
        $hasExplicitOAuthModels = $accountOAuthModels.Count -gt 0
        $hasApiFallbackForAccount = $false
        if ($hasExplicitOAuthModels -and [bool]$routePriorities.Enabled) {
            foreach ($oauthModel in $accountOAuthModels) {
                # Never name this $matches: the regex engine inside the identity
                # helpers owns that automatic variable and would clobber the count.
                $fallbackMatches = @($models | Where-Object {
                    (Get-ModelSource $_) -ne 'oauth' -and
                    (Test-RouterSameModel -LeftModelId ([string]$_.model) -RightModelId ([string]$oauthModel.model)) -and
                    (Test-RouterFallbackChannelSelected `
                        -Selections $fallbackChannelSelections `
                        -ModelId ([string]$_.model) `
                        -BaseUrl ([string]$_.baseURL))
                })
                if ($fallbackMatches.Count -gt 0) {
                    $hasApiFallbackForAccount = $true
                    break
                }
            }
        }
        $recoveryState = Get-RouterOAuthRecoveryState -Account $account
        if ($selectedByConfig -and $hasExplicitOAuthModels -and [bool]$recoveryState.ShouldIsolate) {
            $isolatedOAuthAccountIds[$accountID] = [string]$recoveryState.Reason
        }
        # Quota state machine:
        #   quota available            -> account joins the group and wins on priority
        #   quota exhausted            -> account leaves the group (no wasted attempt,
        #                                 so no extra latency) and the same-model API
        #                                 channel serves instead
        #   quota restored (recovery)  -> Invoke-OAuthRecovery re-joins the account
        # Account enrollment without imported OAuth models never joins the group.
        $selected = $selectedByConfig -and $hasExplicitOAuthModels -and -not [bool]$recoveryState.ShouldIsolate
        $nextGroups = @($groupIds | Where-Object { $_ -ne $groupId })
        if ($selected) { $nextGroups += $groupId }
        # Keep each OAuth account's own scheduler priority so multi-account
        # same-platform ordering (P1/P2/...) survives Apply. Only fall back to
        # the profile OAuth priority when the account has no usable value.
        $accountPriority = $officialPriority
        $priorityProperty = $account.PSObject.Properties['priority']
        if ($null -ne $priorityProperty -and $null -ne $priorityProperty.Value) {
            try {
                $parsedPriority = [int]$priorityProperty.Value
                if ($parsedPriority -ge 1 -and $parsedPriority -le 999) {
                    $accountPriority = $parsedPriority
                }
            } catch { }
        }
        $accountPlatform = [string]$account.platform
        if (-not $selectedByConfig) {
            Write-RouterFlag 'OAUTH-SKIP-UNSELECTED' @{ account = $accountID; platform = $accountPlatform }
        } elseif (-not $hasExplicitOAuthModels) {
            Write-RouterFlag 'OAUTH-SKIP-NO-MODELS' @{ account = $accountID; platform = $accountPlatform }
        } elseif ([bool]$recoveryState.ShouldIsolate) {
            Write-RouterFlag $(if ($hasApiFallbackForAccount) { 'OAUTH-PARKED-WITH-FALLBACK' } else { 'OAUTH-PARKED-NO-FALLBACK' }) @{
                account = $accountID
                platform = $accountPlatform
                reason = [string]$recoveryState.Reason
                reset = [string]$recoveryState.ResetAt
                models = $accountOAuthModels.Count
            }
        } else {
            Write-RouterFlag 'OAUTH-PRIMARY' @{
                account = $accountID
                platform = $accountPlatform
                priority = $accountPriority
                models = $accountOAuthModels.Count
                fallback = $(if ($hasApiFallbackForAccount) { 'yes' } else { 'no' })
            }
        }
        $oauthUpdate = @{
            priority = $accountPriority
            group_ids = @($nextGroups | Select-Object -Unique)
            confirm_mixed_channel_risk = $true
        }
        $proxyDecision = Get-RouterAccountProxyReconciliation `
            -CurrentProxyId $currentProxyId `
            -RouterManagedProxyIds $routerManagedProxyIds `
            -DesiredProxyId ([long]$managedProxyState.DesiredProxyId) `
            -ShouldUseManagedProxy $selected
        if ($proxyDecision.Action -in @('assign', 'replace', 'clear')) {
            # Sub2API uses proxy_id=0, not JSON null, to express an explicit clear.
            $oauthUpdate.proxy_id = [long]$proxyDecision.ProxyId
        } elseif ($proxyDecision.Action -eq 'preserve-custom') {
            $proxyCustomPreserved++
        }
        [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/accounts/$accountID" -Body $oauthUpdate)
        if ($selectedByConfig -and [bool]$recoveryState.ShouldIsolate) {
            Write-Output "OAuth account $accountID isolated until recovery: $($recoveryState.Reason)"
        }
        switch ($proxyDecision.Action) {
            'assign' { $proxyAssigned++ }
            'replace' { $proxyReplaced++ }
            'clear' { $proxyCleared++ }
        }
        continue
    }
    $isManagedChannel = [string]$account.name -in $managedAccountNames
    $isOAuthFallbackChannel = $false
    if ($isManagedChannel) {
        $extraProperty = $account.PSObject.Properties['extra']
        if ($null -ne $extraProperty -and $null -ne $extraProperty.Value) {
            $flag = $extraProperty.Value.PSObject.Properties['codex_router_oauth_fallback']
            if ($null -ne $flag) { $isOAuthFallbackChannel = [bool]$flag.Value }
        }
    }
    $shouldUseManagedProxy = $isRouterMember -and $isManagedChannel
    if ($shouldUseManagedProxy -and [long]$managedProxyState.DesiredProxyId -gt 0) {
        $target = [string]$managedAccountTargets[[string]$account.name]
        if (-not [string]::IsNullOrWhiteSpace($target) -and
            (Test-RouterProxyBypass -TargetUri $target -NoProxy ([string]$proxySettings.NoProxy))) {
            $shouldUseManagedProxy = $false
            $proxyBypassed++
        }
    }
    $proxyDecision = Get-RouterAccountProxyReconciliation `
        -CurrentProxyId $currentProxyId `
        -RouterManagedProxyIds $routerManagedProxyIds `
        -DesiredProxyId ([long]$managedProxyState.DesiredProxyId) `
        -ShouldUseManagedProxy $shouldUseManagedProxy
    if ($proxyDecision.Action -in @('assign', 'replace', 'clear')) {
        Set-RouterAccountProxy `
            -Session $session `
            -AccountId $accountID `
            -ProxyId ([long]$proxyDecision.ProxyId)
    } elseif ($proxyDecision.Action -eq 'preserve-custom') {
        $proxyCustomPreserved++
    }
    switch ($proxyDecision.Action) {
        'assign' { $proxyAssigned++ }
        'replace' { $proxyReplaced++ }
        'clear' { $proxyCleared++ }
    }
    if ([string]$account.name -like 'Codex-Router / *' -and $isRouterMember -and [string]$account.name -notin $managedAccountNames) {
        [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/accounts/$accountID" -Body @{
            group_ids = @($groupIds | Where-Object { $_ -ne $groupId })
        })
    }
}
Write-Output (
    'Outbound proxy reconciliation: source={0}; resource={1}; assigned={2}; replaced={3}; cleared={4}; custom-preserved={5}; bypassed={6}' -f
    [string]$managedProxyState.Source,
    [string]$managedProxyState.Action,
    $proxyAssigned,
    $proxyReplaced,
    $proxyCleared,
    $proxyCustomPreserved,
    $proxyBypassed)

# Deterministic routing reconciliation. This block always runs so every Apply
# ends in the same verified state: healthy subscription quota first, quota-out
# subscriptions parked, and same-model third-party API channels serving instead.
$servableRoutes = @(Get-RouterServableCatalogRoutes `
    -RoutePlan $routePlan `
    -IsolatedOAuthAccountIds $isolatedOAuthAccountIds `
    -OAuthAccountIds $oauthAccountIds `
    -OAuthSelectionInitialized:$oauthSelectionInitialized)
# Composite routes must also keep API fallback rows that stay hidden from the
# Codex menu, otherwise the cross-platform fallback route is deleted right after
# it is created (this used to break Gemini/Grok fallback on every Apply).
$servableRoutingRoutes = @(Get-RouterServableRoutingRoutes `
    -RoutePlan $routePlan `
    -IsolatedOAuthAccountIds $isolatedOAuthAccountIds `
    -OAuthAccountIds $oauthAccountIds `
    -OAuthSelectionInitialized:$oauthSelectionInitialized)
$servableModelNames = @($servableRoutes | ForEach-Object { [string]$_.PublicModelId } | Where-Object { $_ } | Select-Object -Unique)
if ($servableModelNames.Count -eq 0) {
    throw 'ROUTER_DEPLOY_NO_SERVABLE_MODEL: no model can be served right now. Add an API channel, or wait for a subscription quota reset.'
}
$removedUnavailable = @($modelNames | Where-Object { $servableModelNames -notcontains $_ })
Write-Output (
    'Catalog availability filter: kept={0}; removed-unavailable={1}' -f
    $servableModelNames.Count,
    ($(if ($removedUnavailable.Count -gt 0) { $removedUnavailable -join ', ' } else { 'none' }))
)
Write-RouterFlag 'CATALOG-FILTER' @{ kept = $servableModelNames.Count; dropped = $removedUnavailable.Count }
foreach ($dropped in $removedUnavailable) {
    Write-RouterFlag 'CATALOG-DROPPED' @{ model = $dropped; reason = 'no-servable-account' }
}
foreach ($route in $servableRoutes) {
    $servedBy = if ($null -eq $route.PSObject.Properties['ServedBy']) { 'api' } else { [string]$route.ServedBy }
    $oauthId = 0
    try { $oauthId = [long]$route.Model.oauthAccountId } catch { $oauthId = 0 }
    Write-RouterFlag 'CATALOG-MODEL' @{
        model = [string]$route.PublicModelId
        served = $servedBy
        suffix = $(if ($servedBy -eq 'oauth') { 'oauth' } else { 'none' })
        account = $(if ($servedBy -eq 'oauth' -and $oauthId -gt 0) { $oauthId } else { 0 })
    }
    if ($servedBy -eq 'api' -and (Get-RouterModelSource -Model $route.Model) -eq 'oauth') {
        Write-RouterFlag 'FALLBACK-ACTIVE' @{
            model = [string]$route.PublicModelId
            account = $oauthId
            reason = 'subscription-quota-parked'
        }
    }
}
$groupBody.models_list_config = @{ enabled = $true; models = @($servableModelNames) }
$group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/groups/$groupId" -Body $groupBody)
$compositeRoutes = @(Get-RouterCompositeRoutePlan `
    -RoutePlan $servableRoutingRoutes `
    -AccountPlatformById $accountPlatformById `
    -ExcludedOAuthAccountIds @($isolatedOAuthAccountIds.Keys))
$compositeSync = Sync-RouterCompositeRoutes `
    -Session $session `
    -GroupId $groupId `
    -CompositeRoutes $compositeRoutes
Write-Output (
    'Composite routes (servable): desired={0}; created={1}; updated={2}; removed={3}' -f
    [int]$compositeSync.Desired,
    [int]$compositeSync.Created,
    [int]$compositeSync.Updated,
    [int]$compositeSync.Removed)
Write-RouterFlag 'COMPOSITE-SYNC' @{
    desired = [int]$compositeSync.Desired
    created = [int]$compositeSync.Created
    updated = [int]$compositeSync.Updated
    removed = [int]$compositeSync.Removed
}
foreach ($composite in $compositeRoutes) {
    Write-RouterFlag 'COMPOSITE-ROUTE' @{
        model = [string]$composite.PublicModelId
        platform = [string]$composite.TargetPlatform
        priority = [int]$composite.Priority
    }
}
$defaultModel = Get-RouterDefaultPublicModelId -RouterConfig $config -RoutePlan $servableRoutes
& (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
    -ConfigPath $configPath `
    -OutputPath $catalogPath `
    -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount `
    -RoutePlan $servableRoutes | Write-Output
[IO.Directory]::CreateDirectory((Split-Path -Parent $packageCatalogPath)) | Out-Null
Copy-Item -LiteralPath $catalogPath -Destination $packageCatalogPath -Force
$modelNames = @($servableModelNames)

# Sub2API honors provider reset timestamps when a rate-limit response includes
# one. Codex-Router owns OAuth recovery probes and only runs them on app start
# and when the user opens the usage monitor / OAuth pages — never as an extra
# hop on every chat request. Disable any leftover Sub2API scheduled plans so
# they cannot inject probes into the live request path.
foreach ($oauthAccountID in $oauthAccountIds) {
    $configuredOAuthModels = @($selectedOAuthModels | Where-Object {
        [long]$_.oauthAccountId -eq $oauthAccountID
    })
    if ($configuredOAuthModels.Count -eq 0) { continue }

    try {
        $availableModelIDs = @(Get-RouterDiscoveredOAuthModelsForAccount `
            -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount `
            -AccountId $oauthAccountID)
        if (-not [bool]$routePriorities.Enabled) {
            $availableModelData = Get-RouterResponseData (Invoke-RouterApi `
                -Session $session `
                -Method GET `
                -Path "/api/v1/admin/accounts/$oauthAccountID/models")
            $availableModelIDs = @($availableModelData | ForEach-Object {
                $idProperty = $_.PSObject.Properties['id']
                if ($null -ne $idProperty -and -not [string]::IsNullOrWhiteSpace([string]$idProperty.Value)) {
                    [string]$idProperty.Value
                }
            })
        } elseif ($oauthModelDiscoveryFailures.Contains([string]$oauthAccountID)) {
            continue
        }
        if ($availableModelIDs.Count -eq 0) {
            Write-Warning "OAuth account $oauthAccountID returned no discoverable models; on-demand recovery was not configured."
            continue
        }

        $probeModel = @($configuredOAuthModels | ForEach-Object { [string]$_.model } | Where-Object {
            $_ -in $availableModelIDs
        } | Select-Object -First 1)
        if ($probeModel.Count -eq 0) {
            $configuredIDs = @($configuredOAuthModels | ForEach-Object { [string]$_.model }) -join ', '
            Write-Warning "OAuth account $oauthAccountID does not currently advertise the imported model(s): $configuredIDs. Recovery will probe '$($availableModelIDs[0])'."
            $probeModel = @($availableModelIDs[0])
        }

        $disabledPlanCount = Disable-RouterScheduledRecoveryPlans `
            -Session $session `
            -AccountId $oauthAccountID
        Write-Output "OAuth on-demand recovery delegated to Codex-Router: account $oauthAccountID / $($probeModel[0]) / disabled overlapping plans $disabledPlanCount"
    } catch {
        Write-Warning "Could not configure on-demand recovery for OAuth account $oauthAccountID`: $($_.Exception.Message)"
    }
}

$localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
if ([string]::IsNullOrWhiteSpace($localKey)) {
    $localKey = New-RandomLocalKey
    Set-RouterCredential -Name 'LocalApiKey' -Secret $localKey
}
$keyResponse = Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/keys?page=1&page_size=200'
$keyData = Get-RouterResponseData $keyResponse
$keys = if ($null -ne $keyData.PSObject.Properties['items']) { @($keyData.items) } else { @($keyData) }
if (-not ($keys | Where-Object { $_.key -eq $localKey } | Select-Object -First 1)) {
    [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/keys' -Body @{
        name = $groupName
        group_id = $groupId
        custom_key = $localKey
        quota = 0
    } -IdempotencyKey 'codex-router-local-key-v2')
}

$codexHome = if ($config.deploy.codexHome) { [string]$config.deploy.codexHome } elseif ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex' }
Write-Output '[6/7] Writing Codex configuration and the local access key...'
[IO.Directory]::CreateDirectory($codexHome) | Out-Null
$codexConfig = Join-Path $codexHome 'config.toml'
$modelDefaults = Get-CodexRouterModelDefaults `
    -RouterConfig $config `
    -CatalogPath $catalogPath `
    -Model $defaultModel
$text = if (Test-Path -LiteralPath $codexConfig) { [IO.File]::ReadAllText($codexConfig) } else { '' }
$permissionSource = Get-CodexPermissionSourceContent -CodexConfigPath $codexConfig -Content $text
$sub2apiHost = if ($config.deploy.sub2apiHost) { [string]$config.deploy.sub2apiHost } else { 'http://127.0.0.1:18080' }
# Always keep Codex account/sign-in mode. Never force API-only login, never
# rewrite auth.json. Local Router auth stays on experimental_bearer_token.
$text = New-CodexRouterConfig `
    -Content $text `
    -Model $defaultModel `
    -CatalogPath $catalogPath `
    -LocalApiKey $localKey `
    -BaseUrl $sub2apiHost `
    -ReasoningEffort $modelDefaults.ReasoningEffort `
    -FastMode $modelDefaults.FastMode `
    -RequireOpenAiAuth $true `
    -PermissionSourceContent $permissionSource `
    -CodexHome $codexHome
if (Test-Path -LiteralPath $codexConfig) {
    $backup = "$codexConfig.codex-router-$(Get-Date -Format 'yyyyMMdd-HHmmss-fff').bak"
    [IO.File]::Copy($codexConfig, $backup, $false)
    Limit-CodexRouterBackups `
        -Directory (Split-Path -Parent $codexConfig) `
        -Filter 'config.toml.codex-router-*.bak' `
        -Keep 3
}
Write-RouterTextFileAtomic -Path $codexConfig -Text $text

# OpenCode is outside product scope: never rewrite the user's OpenCode config.

$startWithWindows = $true
$autostartProperty = $config.deploy.PSObject.Properties['startWithWindows']
if ($null -ne $autostartProperty) {
    $startWithWindows = [bool]$autostartProperty.Value
}
if ($startWithWindows) {
    & (Join-Path $PSScriptRoot 'Register-Autostart.ps1') | Write-Output
} else {
    & (Join-Path $PSScriptRoot 'Unregister-Autostart.ps1') | Write-Output
}

Write-Output "Configured $($models.Count) model channel(s)."
Write-RouterFlag 'STAGE-06-CODEX-OK' @{ channels = $models.Count }
Write-Output "Codex configuration written to: $codexConfig"
Write-Output 'Local access key is stored in Windows Credential Manager and the current user environment.'
Write-Output '[7/7] Deployment complete.'
Write-RouterFlag 'STAGE-07-DONE'
[Console]::Error.WriteLine('[codex-router:deployment-complete]')
$localKey = $null
$session.Headers.Clear()
} finally {
    $localKey = $null
    if ($null -ne $session -and $null -ne $session.Headers) { $session.Headers.Clear() }
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
        $previousLifecycleLockMarker,
        'Process')
    Exit-RouterLifecycleLock -Lock $lifecycleLock
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_CONFIG_LOCK_HELD',
        $previousLockMarker,
        'Process')
    Exit-RouterConfigLock -Lock $configLock
}
