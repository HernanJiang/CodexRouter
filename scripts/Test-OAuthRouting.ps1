Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

function Assert-Equal($Expected, $Actual, [string]$Message) {
    if ($Expected -ne $Actual) {
        throw "$Message Expected '$Expected', got '$Actual'."
    }
}

$legacy = [pscustomobject]@{
    enabled = $true
    officialPriority = 1
    fallbackPriority = 100
}
$legacyPriorities = Get-RouterOAuthRoutingPriorities -OAuthFallback $legacy
Assert-Equal $true $legacyPriorities.PreferOAuth 'Legacy OAuth preference changed.'
Assert-Equal 1 $legacyPriorities.OAuthPriority 'Legacy OAuth priority changed.'
Assert-Equal 100 $legacyPriorities.ApiPriority 'Legacy API fallback priority changed.'

$apiFirst = [pscustomobject]@{
    enabled = $true
    preferOAuth = $false
    officialPriority = 1
    fallbackPriority = 100
}
$apiFirstPriorities = Get-RouterOAuthRoutingPriorities -OAuthFallback $apiFirst
Assert-Equal $false $apiFirstPriorities.PreferOAuth 'API-first preference was ignored.'
Assert-Equal 100 $apiFirstPriorities.OAuthPriority 'OAuth was not moved behind the API channel.'
Assert-Equal 1 $apiFirstPriorities.ApiPriority 'The API channel did not receive first priority.'

$disabled = Get-RouterOAuthRoutingPriorities -OAuthFallback ([pscustomobject]@{
    enabled = $false
    preferOAuth = $false
    officialPriority = 4
    fallbackPriority = 40
})
Assert-Equal 1 $disabled.OAuthPriority 'Disabled fallback changed standalone OAuth priority.'
Assert-Equal 10 $disabled.ApiPriority 'Disabled fallback changed the legacy API default.'

Assert-Equal 'gpt-5.6-sol' (Get-RouterCanonicalModelId -ModelId 'OpenAI/GPT-5.6') 'Legacy Sol model normalization changed.'

$cursorStyleNames = [ordered]@{
    'openai/gpt-5.6-sol-fast' = 'ChatGPT-5.6-Sol-Fast'
    'gpt-5.4-mini' = 'ChatGPT-5.4-Mini'
    'gpt-5.3-codex-high' = 'ChatGPT-5.3-Codex-High'
    'anthropic/claude-opus-5-fast' = 'Claude-Opus-5-Fast'
    'claude-sonnet-4-6-20260501' = 'Claude-Sonnet-4.6'
    'google/gemini-3-1-pro' = 'Gemini-3.1-Pro'
    'gemini-3-pro-image-preview' = 'Gemini-3-Pro-Image-Preview'
    'deepseek/deepseek-v3.2' = 'DeepSeek-V3.2'
    'x-ai/grok-4.5' = 'Grok-4.5'
    'cursor-composer-2.5' = 'Composer-2.5'
    'z-ai/glm-5.2' = 'GLM-5.2'
}
foreach ($modelId in $cursorStyleNames.Keys) {
    Assert-Equal $cursorStyleNames[$modelId] `
        (Get-RouterRecommendedDisplayName -ModelId $modelId) `
        "Cursor-style display name changed for $modelId."
}

$routeConfig = ('{
  "defaultModel":"gpt-5.6-sol",
  "oauthFallback":{"enabled":true,"preferOAuth":true,"officialPriority":1,"fallbackPriority":100},
  "oauthAccountIds":[42],
  "fallbackChannelSelections":{},
  "models":[
    {"model":"gpt-5.6-sol","alias":"Account quota","source":"oauth","oauthAccountId":42},
    {"model":"gpt-5.6-sol","alias":"Chiral quota","baseURL":"https://api.430123.xyz/v1","credentialName":"ModelApiKey-chiral"},
    {"model":"openai/gpt-5.6-sol","alias":"OpenRouter quota","baseURL":"https://openrouter.ai/api/v1","credentialName":"ModelApiKey-openrouter"}
  ]
}') | ConvertFrom-Json
$mergedPlan = @(Get-RouterModelRoutePlan -RouterConfig $routeConfig)
Assert-Equal 1 @($mergedPlan | Where-Object IncludeInCatalog).Count 'Enabled fallback did not merge the Codex menu.'
Assert-Equal 'gpt-5.6-sol' (Get-RouterDefaultPublicModelId -RouterConfig $routeConfig -RoutePlan $mergedPlan) 'Merged default route is incorrect.'
foreach ($apiRoute in @($mergedPlan | Where-Object Source -eq 'apikey')) {
    Assert-Equal $true $apiRoute.IsOAuthFallback 'Matching API route was not marked as OAuth fallback.'
    Assert-Equal $true $apiRoute.JoinRouter 'Automatic matching did not join a fallback API route.'
    Assert-Equal 'gpt-5.6-sol' ([string]$apiRoute.RequestModelIds[0]) 'Fallback API route does not accept the OAuth model ID.'
}

$implicitConfig = ('{
  "defaultModel":"openai/gpt-5.6-sol",
  "oauthFallback":{"enabled":true,"preferOAuth":true,"officialPriority":1,"fallbackPriority":100},
  "oauthAccountIds":[42],
  "fallbackChannelSelections":{},
  "models":[
    {"model":"openai/gpt-5.6-sol","alias":"Chiral quota","baseURL":"https://api.430123.xyz/v1","credentialName":"ModelApiKey-chiral","priority":10},
    {"model":"deepseek/deepseek-v4-pro","alias":"DeepSeek quota","baseURL":"https://openrouter.ai/api/v1","credentialName":"ModelApiKey-openrouter","priority":20}
  ]
}') | ConvertFrom-Json
$implicitDiscovery = @{
    '42' = @([pscustomobject]@{ id = 'gpt-5.6-sol' })
}
$implicitPlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $implicitConfig `
    -DiscoveredOAuthModelsByAccount $implicitDiscovery)
$implicitFallback = @($implicitPlan | Where-Object {
    $_.CanonicalModelId -eq 'gpt-5.6-sol'
}) | Select-Object -First 1
Assert-Equal $true $implicitFallback.IsOAuthFallback 'Implicit same-name API route was not marked as OAuth fallback.'
Assert-Equal $true $implicitFallback.JoinRouter 'Implicit same-name API route did not join the Router group.'
Assert-Equal $true $implicitFallback.IncludeInCatalog 'Implicit OAuth binding lost its catalog representative.'
Assert-Equal 'gpt-5.6-sol' $implicitFallback.PublicModelId 'Implicit OAuth binding did not expose the discovered OAuth model ID.'
Assert-Equal 'gpt-5.6-sol' ([string]$implicitFallback.RequestModelIds[0]) 'Implicit fallback mapping does not accept the discovered OAuth model ID.'
$differentNameRoute = @($implicitPlan | Where-Object {
    $_.CanonicalModelId -eq 'deepseek-v4-pro'
}) | Select-Object -First 1
Assert-Equal $false $differentNameRoute.IsOAuthFallback 'A different-name API route was incorrectly paired with OAuth.'
Assert-Equal $true $differentNameRoute.JoinRouter 'An unrelated API route was removed from the Router group.'

$explicitAuthorityConfig = ('{
  "oauthFallback":{"enabled":true,"preferOAuth":true,"officialPriority":1,"fallbackPriority":100},
  "oauthAccountIds":[42],
  "models":[
    {"model":"gpt-5.6-sol","source":"oauth","oauthAccountId":42},
    {"model":"gpt-5.6-sol","baseURL":"https://api.430123.xyz/v1","credentialName":"ModelApiKey-chiral","priority":10},
    {"model":"gpt-5.6-luna","baseURL":"https://api.luna.example/v1","credentialName":"ModelApiKey-luna","priority":20}
  ]
}') | ConvertFrom-Json
$explicitAuthorityPlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $explicitAuthorityConfig `
    -DiscoveredOAuthModelsByAccount @{'42' = @('gpt-5.6-sol', 'gpt-5.6-luna')})
Assert-Equal $true (@($explicitAuthorityPlan | Where-Object {
    $_.Source -eq 'apikey' -and $_.CanonicalModelId -eq 'gpt-5.6-sol'
})[0].IsOAuthFallback) 'Explicit OAuth model compatibility changed.'
Assert-Equal $false (@($explicitAuthorityPlan | Where-Object {
    $_.Source -eq 'apikey' -and $_.CanonicalModelId -eq 'gpt-5.6-luna'
})[0].IsOAuthFallback) 'Discovery overrode an account with explicit OAuth model rows.'

$discoveryFailurePlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $implicitConfig `
    -DiscoveredOAuthModelsByAccount @{})
$failureFallback = @($discoveryFailurePlan | Where-Object {
    $_.CanonicalModelId -eq 'gpt-5.6-sol'
}) | Select-Object -First 1
Assert-Equal $false $failureFallback.IsOAuthFallback 'A failed model discovery fabricated an OAuth fallback pair.'
Assert-Equal $true $failureFallback.JoinRouter 'A failed model discovery removed the API route.'
Assert-Equal $true $failureFallback.IncludeInCatalog 'A failed model discovery removed the API model from the catalog.'
Assert-Equal 'openai/gpt-5.6-sol' $failureFallback.PublicModelId 'A failed model discovery did not preserve the configured API route.'

$routeConfig.oauthFallback.enabled = $false
$splitPlan = @(Get-RouterModelRoutePlan -RouterConfig $routeConfig)
Assert-Equal 3 @($splitPlan | Where-Object IncludeInCatalog).Count 'Disabled fallback did not expose distinct routes.'
$splitApiIds = @($splitPlan | Where-Object Source -eq 'apikey' | ForEach-Object { [string]$_.PublicModelId })
if (@($splitApiIds | Where-Object { $_ -match '--api-[0-9a-f]{12}$' }).Count -ne 2) {
    throw 'Disabled fallback did not assign stable split IDs to API routes.'
}
if (@($splitPlan | Where-Object Source -eq 'apikey' | Where-Object {
    [string]$_.RequestModelIds[0] -ne [string]$_.PublicModelId
}).Count -ne 0) {
    throw 'A split API account accepts a model ID belonging to another quota route.'
}

$routeConfig.oauthFallback.enabled = $true
$routeConfig.fallbackChannelSelections = [pscustomobject]@{ 'gpt-5.6-sol' = @() }
$disabledFallbackPlan = @(Get-RouterModelRoutePlan -RouterConfig $routeConfig)
if (@($disabledFallbackPlan | Where-Object Source -eq 'apikey' | Where-Object JoinRouter).Count -ne 0) {
    throw 'An explicitly disabled fallback channel still joins the Router group.'
}
$firstKey = Get-RouterFallbackChannelKey `
    -ModelId 'gpt-5.6-sol' `
    -BaseUrl 'HTTPS://API.FIRST.EXAMPLE/V1/'
$secondKey = Get-RouterFallbackChannelKey `
    -ModelId 'openai/gpt-5.6-sol' `
    -BaseUrl 'https://api.second.example/v1'
Assert-Equal 'gpt-5.6-sol|https://api.first.example/v1' $firstKey 'Fallback channel key is not stable.'
$manualSelections = ('{"gpt-5.6-sol":["' + $secondKey + '"]}') | ConvertFrom-Json
Assert-Equal $false (Test-RouterFallbackChannelSelected `
    -Selections $manualSelections `
    -ModelId 'gpt-5.6-sol' `
    -BaseUrl 'https://api.first.example/v1') 'An unselected fallback channel remained active.'
Assert-Equal $true (Test-RouterFallbackChannelSelected `
    -Selections $manualSelections `
    -ModelId 'gpt-5.6-sol' `
    -BaseUrl 'https://api.second.example/v1/') 'The selected fallback channel was not activated.'
Assert-Equal $true (Test-RouterFallbackChannelSelected `
    -Selections $manualSelections `
    -ModelId 'gpt-5.6-luna' `
    -BaseUrl 'https://api.luna.example/v1') 'A missing manual model selection did not preserve automatic matching.'
$emptySelections = '{"gpt-5.6-sol":[]}' | ConvertFrom-Json
Assert-Equal $false (Test-RouterFallbackChannelSelected `
    -Selections $emptySelections `
    -ModelId 'gpt-5.6-sol' `
    -BaseUrl 'https://api.first.example/v1') 'An explicit empty fallback selection was ignored.'

Assert-Equal 100 (Get-RouterEffectiveApiPriority `
    -ConfiguredPriority 10 -MinimumMatchingPriority 10 `
    -ApiBasePriority 100 -OAuthPriority 1 -PreferOAuth $true) 'First inserted OAuth fallback priority changed.'
Assert-Equal 110 (Get-RouterEffectiveApiPriority `
    -ConfiguredPriority 20 -MinimumMatchingPriority 10 `
    -ApiBasePriority 100 -OAuthPriority 1 -PreferOAuth $true) 'Configured fallback ordering was not preserved.'
Assert-Equal 1 (Get-RouterEffectiveApiPriority `
    -ConfiguredPriority 10 -MinimumMatchingPriority 10 `
    -ApiBasePriority 1 -OAuthPriority 100 -PreferOAuth $false) 'API-first base priority changed.'
Assert-Equal 11 (Get-RouterEffectiveApiPriority `
    -ConfiguredPriority 20 -MinimumMatchingPriority 10 `
    -ApiBasePriority 1 -OAuthPriority 100 -PreferOAuth $false) 'API-first configured ordering was not preserved.'

$now = [DateTimeOffset]::Parse('2026-08-03T00:00:00Z')
$deferredState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $true
    rate_limit_reset_at = '2026-08-03T02:00:00Z'
}) -NowUtc $now
Assert-Equal 'defer' $deferredState.Action 'A future OAuth reset was not deferred.'
Assert-Equal $true $deferredState.ShouldIsolate 'A future OAuth reset remained in the request path.'
Assert-Equal 7200 $deferredState.NextCheckSeconds 'The exact OAuth reset delay was not preserved.'

$usageDeferredState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $true
    extra = [pscustomobject]@{
        codex_7d_used_percent = 100
        codex_7d_reset_at = '2026-08-08T00:00:00Z'
        codex_usage_updated_at = '2026-08-03T00:00:00Z'
    }
}) -NowUtc $now
Assert-Equal 'defer' $usageDeferredState.Action 'A known-empty OAuth usage snapshot was not deferred.'
Assert-Equal $true $usageDeferredState.ShouldIsolate 'A known-empty OAuth usage snapshot remained routable.'
Assert-Equal 432000 $usageDeferredState.NextCheckSeconds 'The passive usage reset time was not reused.'

$usageResetAfterState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $true
    extra = @{
        codex_primary_used_percent = 100
        codex_primary_reset_after_seconds = 3600
        codex_usage_updated_at = '2026-08-03T00:00:00Z'
    }
}) -NowUtc $now
Assert-Equal 'defer' $usageResetAfterState.Action 'A relative OAuth usage reset was not deferred.'
Assert-Equal 3600 $usageResetAfterState.NextCheckSeconds 'The relative OAuth reset delay was not preserved.'

$usageUnknownResetState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $true
    extra = @{ codex_7d_used_percent = 100 }
}) -NowUtc $now
Assert-Equal 'probe' $usageUnknownResetState.Action 'Known exhaustion without a reset was not scheduled for probing.'
Assert-Equal 3600 $usageUnknownResetState.NextCheckSeconds 'Known exhaustion without a reset was queried too often.'

$unknownResetState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $false
    temp_unschedulable_reason = 'usage limit exceeded'
}) -NowUtc $now
Assert-Equal 'probe' $unknownResetState.Action 'An exhausted OAuth account without reset time was not scheduled for probing.'
Assert-Equal 3600 $unknownResetState.NextCheckSeconds 'Unknown-reset OAuth recovery is not hourly.'

$healthyState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $true
    status = 'active'
}) -NowUtc $now
Assert-Equal 'healthy' $healthyState.Action 'A healthy OAuth account was incorrectly isolated.'

$openAiSuggestions = @(Get-RouterOAuthModelSuggestions -Platform openai)
foreach ($requiredModel in @(
    'gpt-5.6-sol',
    'gpt-5.6-terra',
    'gpt-5.6-luna',
    'gpt-5.5',
    'gpt-5.4',
    'gpt-5.4-mini',
    'gpt-5.3-codex-spark',
    'codex-auto-review',
    'gpt-5.2'
)) {
    if ($openAiSuggestions.id -notcontains $requiredModel) {
        throw "OpenAI OAuth discovery suggestion is missing '$requiredModel'."
    }
}

$applySource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Apply-Router.ps1') -Raw
if ($applySource -notmatch 'Get-RouterOAuthRoutingPriorities') {
    throw 'Apply-Router.ps1 does not use the tested OAuth routing priority resolver.'
}
if ($applySource -notmatch 'Test-RouterFallbackChannelSelected' -or
    $applySource -notmatch '\$shouldJoinRouter') {
    throw 'Apply-Router.ps1 does not enforce manual fallback selection through real group membership.'
}
if ($applySource -match 'Direct fallback') {
    throw 'Apply-Router.ps1 still duplicates one API channel into proxy and direct scheduling accounts.'
}
if ($applySource -notmatch 'codex_router_oauth_fallback' -or
    $applySource -notmatch '\[bool\]\$isOAuthFallback') {
    throw 'Apply-Router.ps1 does not mark OAuth fallback provenance for cross-provider continuation sanitization.'
}
if ($applySource -notmatch 'DiscoveredOAuthModelsByAccount' -or
    $applySource -notmatch 'oauthModelDiscoveryFailures' -or
    $applySource -notmatch '/api/v1/admin/accounts/\$oauthAccountId/models') {
    throw 'Apply-Router.ps1 does not discover implicit OAuth models with a failure-safe fallback.'
}
$catalogSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') -Raw
if ($catalogSource -notmatch 'DiscoveredOAuthModelsByAccount') {
    throw 'Build-ModelCatalog.ps1 does not reuse implicit OAuth discovery for stable public model IDs.'
}
if ($applySource -notmatch 'proxy_direct_fallback' -or
    $applySource -notmatch 'Test-RouterDirectFallbackEligible') {
    throw 'Apply-Router.ps1 does not mark verified auto-proxy channels for transport-only direct fallback.'
}

$accountSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Get-OAuthAccounts.ps1') -Raw
if ($accountSource -match '/quota') {
    throw 'Get-OAuthAccounts.ps1 duplicates live quota requests instead of reusing the usage snapshot.'
}
if ($accountSource -notmatch 'Get-RouterOAuthModelSuggestions') {
    throw 'Get-OAuthAccounts.ps1 does not append the tested provider model suggestions.'
}

Write-Output 'OAuth routing tests passed.'
