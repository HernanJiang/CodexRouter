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
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'gpt-5.6-sol' -RightModelId 'openai/gpt-5.6') 'OpenRouter OpenAI identity did not match.'
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'claude-opus-5' -RightModelId 'anthropic/claude-opus-5') 'OpenRouter Anthropic identity did not match.'
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'gemini-3.6-flash' -RightModelId 'google/gemini-3-6-flash') 'OpenRouter Gemini separator identity did not match.'
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'grok-4.5' -RightModelId 'x-ai/grok-4.5') 'OpenRouter Grok identity did not match.'
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'deepseek-v4-flash' -RightModelId 'deepseek/deepseek-v4-flash') 'OpenRouter DeepSeek identity did not match.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'grok-4.5' -RightModelId 'openai/grok-4.5') 'A false provider namespace was accepted.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'claude-opus-5' -RightModelId 'claude-opus-5-fast') 'Fast and standard Claude variants were merged.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'gemini-3.1-pro-high' -RightModelId 'gemini-3.1-pro-low') 'Gemini reasoning variants were merged.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'kimi-for-coding' -RightModelId 'kimi-for-coding-highspeed') 'Kimi highspeed and standard variants were merged.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'kimi-k3' -RightModelId 'k3-256k') 'Kimi context variants were merged by display name only.'
Assert-Equal $false (Test-RouterSameModel -LeftModelId 'vendor-a/model-x' -RightModelId 'vendor-b/model-x') 'Unknown provider namespaces were merged by leaf ID.'
Assert-Equal $true (Test-RouterSameModel -LeftModelId 'claude-opus-4-6' -RightModelId 'anthropic/claude-opus-4.6') 'Anthropic separator aliases did not match.'
Assert-Equal $true (Test-RouterCodingPlanChannel -BaseUrl 'https://api.kimi.com/coding/v1' -ModelId 'kimi-for-coding') 'Kimi Coding Plan endpoint was not recognized.'
Assert-Equal $true (Test-RouterCodingPlanChannel -BaseUrl 'https://ark.cn-beijing.volces.com/api/coding/v3' -ModelId 'glm-5.2') 'Ark Coding Plan endpoint was not recognized.'
Assert-Equal $true (Test-RouterCodingPlanChannel -BaseUrl 'https://ark.cn-beijing.volces.com/api/plan/v3' -ModelId 'ark-code-latest') 'Ark Agent Plan endpoint was not recognized.'
Assert-Equal $false (Test-RouterCodingPlanChannel -BaseUrl 'https://openrouter.ai/api/v1' -ModelId 'kimi-for-coding') 'OpenRouter was misclassified as a Coding Plan.'
Assert-Equal $true (Test-RouterCodingPlanChannel -BaseUrl 'https://vendor.example/v1' -ModelId 'model-x' -Extra '{"codex_router_channel_kind":"coding_plan"}') 'Explicit Coding Plan marker was ignored.'
Assert-Equal 0 (Get-RouterChannelTier -Model ([pscustomobject]@{ model='gpt-5.6-sol'; source='oauth'; baseURL='' })) 'OAuth tier is wrong.'
Assert-Equal 1 (Get-RouterChannelTier -Model ([pscustomobject]@{ model='kimi-for-coding'; source='apikey'; baseURL='https://api.kimi.com/coding/v1'; extra='{}' })) 'Coding Plan tier is wrong.'
Assert-Equal 2 (Get-RouterChannelTier -Model ([pscustomobject]@{ model='gpt-5.6-sol'; source='apikey'; baseURL='https://openrouter.ai/api/v1'; extra='{}' })) 'Third-party API tier is wrong.'

$cursorStyleNames = [ordered]@{
    'openai/gpt-5.6-sol-fast' = 'ChatGPT-5.6-Sol-Fast'
    'gpt-5.4-mini' = 'ChatGPT-5.4-Mini'
    'gpt-5.3-codex-high' = 'ChatGPT-5.3-Codex-High'
    'anthropic/claude-opus-5-fast' = 'Claude-Opus-5-Fast'
    'claude-sonnet-4-6-20260501' = 'Claude-Sonnet-4.6'
    'google/gemini-3-1-pro' = 'Gemini-3.1-Pro'
    'gemini-3-pro-image-preview' = 'Gemini-3-Pro-Image-Preview'
    'deepseek/deepseek-v3.2' = 'DeepSeek-V3.2'
    'deepseek/deepseek-v4-pro' = 'DeepSeek-V4-Pro'
    'deepseek/deepseek-v4-flash' = 'DeepSeek-V4-Flash'
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
    Assert-Equal 'gpt-5.6-sol' $apiRoute.PublicModelId 'Fallback API route does not share the OAuth public model ID.'
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
Assert-equal $false $implicitFallback.IsOAuthFallback 'Account-only OAuth enrollment forced an API route into fallback mode.'
Assert-equal $true $implicitFallback.JoinRouter 'Configured API route left the Router group without an OAuth model import.'
Assert-equal $true $implicitFallback.IncludeInCatalog 'Configured API route disappeared from the catalog without an OAuth model import.'
Assert-equal 'openai/gpt-5.6-sol' $implicitFallback.PublicModelId 'Configured API public model id was rewritten by discovery-only OAuth enrollment.'
Assert-equal 'openai/gpt-5.6-sol' ([string]$implicitFallback.RequestModelIds[0]) 'Configured API request model id was rewritten by discovery-only OAuth enrollment.'
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
Assert-Equal 18000 $usageUnknownResetState.NextCheckSeconds 'Known exhaustion without a reset was not limited to one probe per five hours.'

$unknownResetState = Get-RouterOAuthRecoveryState -Account ([pscustomobject]@{
    schedulable = $false
    temp_unschedulable_reason = 'usage limit exceeded'
}) -NowUtc $now
Assert-Equal 'probe' $unknownResetState.Action 'An exhausted OAuth account without reset time was not scheduled for probing.'
Assert-Equal 18000 $unknownResetState.NextCheckSeconds 'Unknown-reset OAuth recovery is not limited to one probe per five hours.'

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

$antigravitySuggestions = @(Get-RouterOAuthModelSuggestions -Platform antigravity)
foreach ($requiredModel in @('gemini-3-flash', 'gemini-3.1-pro-high')) {
    if ($antigravitySuggestions.id -notcontains $requiredModel) {
        throw "Antigravity OAuth discovery suggestion is missing '$requiredModel'."
    }
}
if ($antigravitySuggestions.id -contains 'gemini-3.6-flash') {
    throw 'Antigravity suggestions still advertise gemini-3.6-flash, which Antigravity does not expose.'
}

$grokSuggestions = @(Get-RouterOAuthModelSuggestions -Platform grok)
if ($grokSuggestions.id -notcontains 'grok-4.5') {
    throw 'Grok OAuth discovery suggestion is missing grok-4.5.'
}

$compositePlan = @(Get-RouterCompositeRoutePlan -RoutePlan @(
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'grok-4.5'
        Model = [pscustomobject]@{ model = 'grok-4.5'; source = 'oauth'; oauthPlatform = 'grok'; oauthAccountId = 5 }
    }
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'gpt-5.6-sol'
        Model = [pscustomobject]@{ model = 'gpt-5.6-sol'; source = 'apikey'; baseURL = 'https://api.openai.com/v1' }
    }
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'gemini-3-flash'
        Model = [pscustomobject]@{ model = 'gemini-3-flash'; source = 'oauth'; oauthPlatform = 'antigravity'; oauthAccountId = 4 }
    }
) -AccountPlatformById @{ '5' = 'grok'; '4' = 'antigravity' })
Assert-equal 'grok' (@($compositePlan | Where-Object PublicModelId -eq 'grok-4.5')[0].TargetPlatform) 'Grok OAuth composite target platform is wrong.'
Assert-equal 'openai' (@($compositePlan | Where-Object PublicModelId -eq 'gpt-5.6-sol')[0].TargetPlatform) 'API channel composite target platform is wrong.'
Assert-equal 'antigravity' (@($compositePlan | Where-Object PublicModelId -eq 'gemini-3-flash')[0].TargetPlatform) 'Antigravity OAuth composite target platform is wrong.'

$fallbackCompositePlan = @(Get-RouterCompositeRoutePlan -RoutePlan @(
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'grok-4.5'
        Model = [pscustomobject]@{ model = 'grok-4.5'; source = 'oauth'; oauthPlatform = 'grok'; oauthAccountId = 5 }
    }
    [pscustomobject]@{
        IncludeInCatalog = $false
        JoinRouter = $true
        PublicModelId = 'grok-4.5'
        Model = [pscustomobject]@{ model = 'x-ai/grok-4.5'; source = 'apikey'; baseURL = 'https://openrouter.ai/api/v1' }
    }
) -AccountPlatformById @{ '5' = 'grok' })
Assert-Equal 2 $fallbackCompositePlan.Count 'Cross-platform OAuth/API fallback did not produce two composite routes.'
Assert-Equal 1 (@($fallbackCompositePlan | Where-Object TargetPlatform -eq 'grok')[0].Priority) 'OAuth composite route priority is wrong.'
Assert-Equal 100 (@($fallbackCompositePlan | Where-Object TargetPlatform -eq 'openai')[0].Priority) 'API fallback composite route priority is wrong.'

Assert-equal 'anthropic/claude-opus-5' (Get-RouterOpenRouterUpstreamModelId -ModelId 'claude/claude-opus-5') 'OpenRouter Claude id normalization failed.'
Assert-equal 'anthropic/claude-opus-5' (Get-RouterUpstreamModelId -ModelId 'claude-opus-5' -BaseUrl 'https://openrouter.ai/api/v1') 'OpenRouter bare Claude id normalization failed.'
Assert-equal 'claude-opus-5' (Get-RouterUpstreamModelId -ModelId 'claude-opus-5' -BaseUrl 'https://api.anthropic.com/v1') 'Non-OpenRouter Claude ids should stay unchanged.'
Assert-equal 'google/gemini-3.1-pro-preview' (Get-RouterUpstreamModelId -ModelId 'google/gemini-3.1-pro-high' -BaseUrl 'https://openrouter.ai/api/v1') 'OpenRouter Gemini 3.1 Pro fallback normalization failed.'

$servable = @(Get-RouterServableCatalogRoutes -RoutePlan @(
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'gpt-5.6-sol'
        CanonicalModelId = 'gpt-5.6-sol'
        Source = 'oauth'
        Model = [pscustomobject]@{ model = 'gpt-5.6-sol'; source = 'oauth'; oauthAccountId = 1 }
        RequestModelIds = @('gpt-5.6-sol')
        IsOAuthFallback = $false
        IsMergedOAuthRoute = $true
        Index = 0
    }
    [pscustomobject]@{
        IncludeInCatalog = $false
        JoinRouter = $true
        PublicModelId = 'gpt-5.6-sol'
        CanonicalModelId = 'gpt-5.6-sol'
        Source = 'apikey'
        Model = [pscustomobject]@{ model = 'gpt-5.6-sol'; source = 'apikey'; baseURL = 'https://api.example/v1' }
        RequestModelIds = @('gpt-5.6-sol')
        IsOAuthFallback = $true
        IsMergedOAuthRoute = $false
        Index = 1
    }
    [pscustomobject]@{
        IncludeInCatalog = $true
        JoinRouter = $true
        PublicModelId = 'gpt-5.6-terra'
        CanonicalModelId = 'gpt-5.6-terra'
        Source = 'oauth'
        Model = [pscustomobject]@{ model = 'gpt-5.6-terra'; source = 'oauth'; oauthAccountId = 1 }
        RequestModelIds = @('gpt-5.6-terra')
        IsOAuthFallback = $false
        IsMergedOAuthRoute = $false
        Index = 2
    }
) -IsolatedOAuthAccountIds @{ 1 = 'quota' } -OAuthAccountIds @(1) -OAuthSelectionInitialized $true)
Assert-Equal 1 @($servable).Count 'Isolated OAuth-only models were not removed from the live catalog.'
Assert-equal 'gpt-5.6-sol' $servable[0].PublicModelId 'API fallback model was dropped while OAuth was isolated.'

$applySource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Apply-Router.ps1') -Raw
if ($applySource -notmatch 'Get-RouterOAuthRoutingPriorities') {
    throw 'Apply-Router.ps1 does not use the tested OAuth routing priority resolver.'
}
if ($applySource -notmatch "platform = 'composite'" -or
    $applySource -notmatch 'Sync-RouterCompositeRoutes') {
    throw 'Apply-Router.ps1 does not deploy a composite Codex-Router group with composite model routes.'
}
if ($applySource -notmatch 'Get-RouterServableCatalogRoutes' -or
    $applySource -notmatch 'isolatedOAuthAccountIds') {
    throw 'Apply-Router.ps1 does not filter catalog models for isolated OAuth accounts.'
}
if ($applySource -notmatch 'Get-RouterUpstreamModelId') {
    throw 'Apply-Router.ps1 does not normalize upstream model IDs for OpenRouter channels.'
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
if ($applySource -notmatch '\$shouldUseManagedProxy\s*=\s*\$isRouterMember\s+-and\s+\$isManagedChannel' -or
    $applySource -match '\$shouldUseManagedProxy\s*=.*-not\s+\$isOAuthFallbackChannel') {
    throw 'Apply-Router.ps1 does not route OAuth fallback API channels through the managed proxy.'
}

$accountSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Get-OAuthAccounts.ps1') -Raw
if ($accountSource -match '/quota') {
    throw 'Get-OAuthAccounts.ps1 duplicates live quota requests instead of reusing the usage snapshot.'
}
if ($accountSource -notmatch 'Get-RouterOAuthModelSuggestions') {
    throw 'Get-OAuthAccounts.ps1 does not append the tested provider model suggestions.'
}

# ---------------------------------------------------------------------------
# Quota state machine: subscription first, park on exhaustion, same-model API
# fallback, and recovery. Verified for openai / antigravity / grok platforms.
# ---------------------------------------------------------------------------
function New-QuotaConfig {
    param([string]$OAuthModel, [long]$AccountId, [string]$ApiModel, [string]$ApiBase)
    $models = @(
        @{ model = $OAuthModel; source = 'oauth'; oauthAccountId = $AccountId; priority = 1 }
    )
    if (-not [string]::IsNullOrWhiteSpace($ApiModel)) {
        $models += @{ model = $ApiModel; baseURL = $ApiBase; credentialName = 'ModelApiKey-openrouter'; priority = 10 }
    }
    return (@{
        defaultModel = $OAuthModel
        oauthFallback = @{ enabled = $true; preferOAuth = $true; officialPriority = 1; fallbackPriority = 100 }
        oauthAccountIds = @($AccountId)
        fallbackChannelSelections = @{}
        models = $models
    } | ConvertTo-Json -Depth 8) | ConvertFrom-Json
}

$platformCases = @(
    [pscustomobject]@{ Name = 'ChatGPT'; OAuthModel = 'gpt-5.6-sol'; Account = 1; Platform = 'openai'; ApiModel = 'openai/gpt-5.6-sol' }
    [pscustomobject]@{ Name = 'Gemini'; OAuthModel = 'gemini-3.1-pro-high'; Account = 4; Platform = 'antigravity'; ApiModel = 'google/gemini-3.1-pro-high' }
    [pscustomobject]@{ Name = 'Grok'; OAuthModel = 'grok-4.5'; Account = 3; Platform = 'grok'; ApiModel = 'x-ai/grok-4.5' }
)

foreach ($case in $platformCases) {
    $label = $case.Name
    $config = New-QuotaConfig -OAuthModel $case.OAuthModel -AccountId $case.Account -ApiModel $case.ApiModel -ApiBase 'https://openrouter.ai/api/v1'
    $plan = @(Get-RouterModelRoutePlan -RouterConfig $config -DiscoveredOAuthModelsByAccount @{})
    $platformMap = @{ ([string]$case.Account) = $case.Platform }

    # 1. Quota available -> OAuth is primary and the API row is a joined fallback.
    $healthyCatalog = @(Get-RouterServableCatalogRoutes -RoutePlan $plan -IsolatedOAuthAccountIds @{} -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    Assert-Equal 1 $healthyCatalog.Count "$label healthy catalog should expose exactly one merged model."
    Assert-Equal 'oauth' ([string]$healthyCatalog[0].ServedBy) "$label healthy model must be served by the subscription."
    $healthyDisplay = Get-RouterModelDisplayName -Model $healthyCatalog[0].Model -Route $healthyCatalog[0]
    if ($healthyDisplay -notlike '*(OAuth)') { throw "$label healthy display name must end with (OAuth): $healthyDisplay" }
    $healthyRouting = @(Get-RouterServableRoutingRoutes -RoutePlan $plan -IsolatedOAuthAccountIds @{} -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    $healthyComposite = @(Get-RouterCompositeRoutePlan -RoutePlan $healthyRouting -AccountPlatformById $platformMap -ExcludedOAuthAccountIds @())
    $healthyPlatforms = @($healthyComposite | ForEach-Object { [string]$_.TargetPlatform } | Sort-Object -Unique)
    if ($healthyPlatforms -notcontains $case.Platform) {
        throw "$label healthy composite routes lost the subscription platform '$($case.Platform)'."
    }
    if ($healthyPlatforms -notcontains 'openai') {
        throw "$label healthy composite routes lost the third-party API fallback platform."
    }
    $oauthComposite = @($healthyComposite | Where-Object { [string]$_.TargetPlatform -eq $case.Platform })[0]
    Assert-Equal 1 ([int]$oauthComposite.Priority) "$label subscription composite route must keep priority 1."

    # 2/3. Quota exhausted -> account parked, same-model API keeps serving.
    $parkedCatalog = @(Get-RouterServableCatalogRoutes -RoutePlan $plan -IsolatedOAuthAccountIds @{ ([long]$case.Account) = 'quota' } -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    Assert-Equal 1 $parkedCatalog.Count "$label parked catalog must still expose the model through the API fallback."
    Assert-Equal 'api' ([string]$parkedCatalog[0].ServedBy) "$label parked model must be served by the API fallback."
    $parkedDisplay = Get-RouterModelDisplayName -Model $parkedCatalog[0].Model -Route $parkedCatalog[0]
    if ($parkedDisplay -like '*(OAuth)') {
        throw "$label parked model must not advertise (OAuth) while the API fallback serves it."
    }
    $parkedRouting = @(Get-RouterServableRoutingRoutes -RoutePlan $plan -IsolatedOAuthAccountIds @{ ([long]$case.Account) = 'quota' } -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    $parkedComposite = @(Get-RouterCompositeRoutePlan -RoutePlan $parkedRouting -AccountPlatformById $platformMap -ExcludedOAuthAccountIds @($case.Account))
    $parkedPlatforms = @($parkedComposite | ForEach-Object { [string]$_.TargetPlatform } | Sort-Object -Unique)
    if ($parkedPlatforms -contains $case.Platform -and $case.Platform -ne 'openai') {
        throw "$label parked subscription platform route must be removed so it cannot answer 503."
    }
    if ($parkedPlatforms -notcontains 'openai') {
        throw "$label parked model lost its third-party API route."
    }

    # 4. Recovery -> identical state to the healthy case (idempotent reconciliation).
    $recoveredCatalog = @(Get-RouterServableCatalogRoutes -RoutePlan $plan -IsolatedOAuthAccountIds @{} -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    Assert-Equal 'oauth' ([string]$recoveredCatalog[0].ServedBy) "$label recovery must restore subscription-first routing."

    # 5. Exhausted without any same-model API channel -> model leaves the menu.
    $soloConfig = New-QuotaConfig -OAuthModel $case.OAuthModel -AccountId $case.Account -ApiModel '' -ApiBase ''
    $soloPlan = @(Get-RouterModelRoutePlan -RouterConfig $soloConfig -DiscoveredOAuthModelsByAccount @{})
    $soloParked = @(Get-RouterServableCatalogRoutes -RoutePlan $soloPlan -IsolatedOAuthAccountIds @{ ([long]$case.Account) = 'quota' } -OAuthAccountIds @($case.Account) -OAuthSelectionInitialized $true)
    Assert-Equal 0 $soloParked.Count "$label exhausted subscription without fallback must leave the Codex menu."
}

# Third-party-only profiles must be untouched by the quota state machine.
$apiOnlyConfig = (@{
    defaultModel = 'openai/gpt-5.6-sol'
    oauthFallback = @{ enabled = $true; preferOAuth = $true; officialPriority = 1; fallbackPriority = 100 }
    oauthAccountIds = @()
    fallbackChannelSelections = @{}
    models = @(@{ model = 'openai/gpt-5.6-sol'; baseURL = 'https://openrouter.ai/api/v1'; credentialName = 'ModelApiKey-openrouter'; priority = 10 })
} | ConvertTo-Json -Depth 8) | ConvertFrom-Json
$apiOnlyPlan = @(Get-RouterModelRoutePlan -RouterConfig $apiOnlyConfig -DiscoveredOAuthModelsByAccount @{})
$apiOnlyCatalog = @(Get-RouterServableCatalogRoutes -RoutePlan $apiOnlyPlan -IsolatedOAuthAccountIds @{} -OAuthAccountIds @() -OAuthSelectionInitialized $true)
Assert-Equal 1 $apiOnlyCatalog.Count 'An API-only profile lost its model.'
Assert-Equal 'api' ([string]$apiOnlyCatalog[0].ServedBy) 'An API-only model must be served by the API channel.'

$applyFlagSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Apply-Router.ps1') -Raw
foreach ($requiredFlag in @(
    'STAGE-01-INIT-OK', 'STAGE-02-SERVICES-OK', 'STAGE-03-ADMIN-OK', 'STAGE-04-COMPLIANCE-OK',
    'STAGE-06-CODEX-OK', 'STAGE-07-DONE', 'OAUTH-PRIMARY', 'OAUTH-PARKED-WITH-FALLBACK',
    'OAUTH-PARKED-NO-FALLBACK', 'FALLBACK-ACTIVE', 'CATALOG-MODEL', 'COMPOSITE-SYNC'
)) {
    if ($applyFlagSource -notmatch [Regex]::Escape($requiredFlag)) {
        throw "Apply-Router.ps1 no longer emits the diagnosable flag '$requiredFlag'."
    }
}
if ($applyFlagSource -notmatch 'Get-RouterServableRoutingRoutes') {
    throw 'Apply-Router.ps1 does not keep API fallback platforms in the composite route sync.'
}
$syncSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Sync-RouterRoutingState.ps1') -Raw
if ($syncSource -notmatch 'ROUTING-SYNC-OK' -or $syncSource -notmatch 'Get-RouterServableRoutingRoutes') {
    throw 'Sync-RouterRoutingState.ps1 does not report a diagnosable routing sync.'
}
$recoverySource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Invoke-OAuthRecovery.ps1') -Raw
if ($recoverySource -notmatch 'Sync-RouterRoutingState.ps1') {
    throw 'OAuth recovery does not realign routing after a quota change.'
}

Write-Output 'Quota state machine tests passed.'
Write-Output 'OAuth routing tests passed.'
