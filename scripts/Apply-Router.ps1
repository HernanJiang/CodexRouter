Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$configPath = Get-RouterConfigPath -RouterRoot $routerRoot
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot
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
if ($models.Count -eq 0) { throw 'At least one model is required.' }
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
$catalogPath = Join-Path $routerRoot 'config\model-catalog.json'

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

Write-Output '[1/7] Initializing local credentials and database...'
& (Join-Path $PSScriptRoot 'Initialize-Router.ps1')
Write-Output '[2/7] Starting PostgreSQL, Redis, and Sub2API...'
& (Join-Path $PSScriptRoot 'Start-Router.ps1')
Write-Output '[3/7] Local services are ready; signing in to the admin API...'
$session = New-RouterAdminSession
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

# Older profiles selected OAuth accounts but did not persist one OAuth model row
# per account. Discovery is runtime-only: it repairs fallback pairing without
# mutating the user's profile, and explicit rows remain authoritative.
$selectedOAuthModels = @($explicitSelectedOAuthModels)
foreach ($oauthAccountId in $oauthAccountIds) {
    if (@($explicitSelectedOAuthModels | Where-Object {
        [long]$_.oauthAccountId -eq $oauthAccountId
    }).Count -gt 0) { continue }
    foreach ($modelId in @(Get-RouterDiscoveredOAuthModelsForAccount `
        -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount `
        -AccountId $oauthAccountId)) {
        $selectedOAuthModels += [pscustomobject][ordered]@{
            model = $modelId
            source = 'oauth'
            oauthAccountId = $oauthAccountId
        }
    }
}

$routePlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $config `
    -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount)
$defaultModel = Get-RouterDefaultPublicModelId -RouterConfig $config -RoutePlan $routePlan
& (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
    -ConfigPath $configPath `
    -OutputPath $catalogPath `
    -DiscoveredOAuthModelsByAccount $discoveredOAuthModelsByAccount | Write-Output

$modelNames = @($routePlan | Where-Object { $_.IncludeInCatalog } | ForEach-Object {
    [string]$_.PublicModelId
} | Where-Object { $_ } | Select-Object -Unique)
Write-Output '[5/7] Creating or updating model channels...'
$groupName = 'Codex-Router'
$groups = @(Get-RouterGroups -Session $session)
$group = $groups | Where-Object { $_.name -eq $groupName } | Select-Object -First 1
$groupBody = @{
    name = $groupName
    description = 'Single-user local Codex multi-model router managed by Codex-Router.'
    platform = 'openai'
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
    $group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/groups' -Body $groupBody -IdempotencyKey 'codex-router-group-v2')
}
$groupId = [long]$group.id
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
        $mapping = [ordered]@{}
        foreach ($requestModelId in @($route.RequestModelIds)) {
            $mapping[[string]$requestModelId] = [string]$model.model
        }
        $canonicalModelID = Get-RouterCanonicalModelId -ModelId ([string]$model.model)
        $isOAuthFallback = [bool]$route.IsOAuthFallback
        $shouldJoinRouter = [bool]$route.JoinRouter
        $effectivePriority = [Math]::Max(1, [int]$model.priority)
        if ($isOAuthFallback) {
            $matchingSelectedPriorities = @($models | Where-Object {
                (Get-ModelSource $_) -ne 'oauth' -and
                (Get-RouterCanonicalModelId -ModelId ([string]$_.model)) -eq $canonicalModelID -and
                (Test-RouterFallbackChannelSelected `
                    -Selections $fallbackChannelSelections `
                    -ModelId ([string]$_.model) `
                    -BaseUrl ([string]$_.baseURL))
            } | ForEach-Object { [Math]::Max(1, [int]$_.priority) })
            $minimumMatchingPriority = [int](
                $matchingSelectedPriorities | Measure-Object -Minimum).Minimum
            $effectivePriority = Get-RouterEffectiveApiPriority `
                -ConfiguredPriority ([Math]::Max(1, [int]$model.priority)) `
                -MinimumMatchingPriority $minimumMatchingPriority `
                -ApiBasePriority $fallbackPriority `
                -OAuthPriority $officialPriority `
                -PreferOAuth ([bool]$routePriorities.PreferOAuth)
        }
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
            -Extra $extra
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
        } else {
            [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts' -Body $body)
            Write-Output "Created channel: $accountName"
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
        $recoveryState = Get-RouterOAuthRecoveryState -Account $account
        $selected = $selectedByConfig -and -not [bool]$recoveryState.ShouldIsolate
        $nextGroups = @($groupIds | Where-Object { $_ -ne $groupId })
        if ($selected) { $nextGroups += $groupId }
        $oauthUpdate = @{
            priority = $officialPriority
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
    $shouldUseManagedProxy = $isRouterMember -and
        $isManagedChannel
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

# Sub2API honors provider reset timestamps when a rate-limit response includes
# one. The hourly auto-recovery probe covers providers that do not publish a
# reset time, so an exhausted OAuth account stays out of the request path until
# it can serve traffic again instead of delaying every request before fallback.
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
            Write-Warning "OAuth account $oauthAccountID returned no discoverable models; hourly recovery was not configured."
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
        Write-Output "OAuth hourly recovery delegated to Codex-Router: account $oauthAccountID / $($probeModel[0]) / disabled overlapping plans $disabledPlanCount"
    } catch {
        Write-Warning "Could not configure hourly recovery for OAuth account $oauthAccountID`: $($_.Exception.Message)"
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
$text = New-CodexRouterConfig `
    -Content $text `
    -Model $defaultModel `
    -CatalogPath $catalogPath `
    -LocalApiKey $localKey `
    -BaseUrl $sub2apiHost `
    -ReasoningEffort $modelDefaults.ReasoningEffort `
    -FastMode $modelDefaults.FastMode `
    -PermissionSourceContent $permissionSource
if (Test-Path -LiteralPath $codexConfig) {
    $backup = "$codexConfig.codex-router-$(Get-Date -Format 'yyyyMMdd-HHmmss-fff').bak"
    [IO.File]::Copy($codexConfig, $backup, $false)
    Limit-CodexRouterBackups `
        -Directory (Split-Path -Parent $codexConfig) `
        -Filter 'config.toml.codex-router-*.bak' `
        -Keep 3
}
Write-RouterTextFileAtomic -Path $codexConfig -Text $text
Set-CodexUserEnvironmentVariable -Name 'CODEX_ROUTER_API_KEY' -Value $localKey

try {
    & (Join-Path $PSScriptRoot 'Install-OpenCodeIntegration.ps1') `
        -RouterConfigPath $configPath `
        -BaseUrl $sub2apiHost | Write-Output
} catch {
    Write-Warning "OpenCode integration was not updated: $($_.Exception.Message)"
}

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
Write-Output "Codex configuration written to: $codexConfig"
Write-Output 'Local access key is stored in Windows Credential Manager and the current user environment.'
Write-Output '[7/7] Deployment complete.'
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
