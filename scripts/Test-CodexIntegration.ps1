Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'CodexIntegration.psm1') -Force

$inputConfig = @'
service_tier = "priority"
disable_response_storage = true
openai_base_url = "https://example.invalid/v1"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"
model_provider = "custom"
model = "old-model"
model_catalog_json = "old-catalog.json"
approval_policy = "never"
sandbox_mode = "danger-full-access"
personality = "pragmatic"

[windows]
sandbox = "elevated"

[model_providers.openai]
name = "External OpenAI profile"
base_url = "https://openai-proxy.example/v1"

[model_providers.ollama]
name = "Local Ollama"
base_url = "http://127.0.0.1:11434/v1"

[model_providers.lmstudio]
name = "Local LM Studio"
base_url = "http://127.0.0.1:1234/v1"

[model_providers.custom]
name = "Unrelated provider"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
env_key = "UNRELATED_KEY"

[model_providers.sub2api]
name = "Old Router"
base_url = "http://127.0.0.1:18081/v1"
requires_openai_auth = true

[mcp_servers.user-tool]
command = "user-mcp.exe"
startup_timeout_sec = 45
'@

$catalog = 'D:\Portable Folder\config\model-catalog.json'
$localKey = 'local-router-test-token'
$isolatedCodexHome = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-integration-' + [Guid]::NewGuid().ToString('N'))
$result = New-CodexRouterConfig `
    -Content $inputConfig `
    -Model 'kimi-for-coding' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -ReasoningEffort 'high' `
    -FastMode $false `
    -CodexHome $isolatedCodexHome
$resultAgain = New-CodexRouterConfig `
    -Content $result `
    -Model 'kimi-for-coding' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -ReasoningEffort 'high' `
    -FastMode $false `
    -CodexHome $isolatedCodexHome

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

Assert-True ($resultAgain -match '(?m)^model_provider\s*=\s*"codex_router"\s*$') 'The dedicated codex_router provider was not selected.'
Assert-True ($resultAgain -match '(?m)^model\s*=\s*"kimi-for-coding"\s*$') 'Selected model was not written.'
Assert-True ($resultAgain -match '(?ms)^\[models\.new_thread\].*?^model\s*=\s*"kimi-for-coding"\s*$') 'The new-thread default model was not written.'
Assert-True ($resultAgain -match '(?m)^model_reasoning_effort\s*=\s*"high"\s*$') 'The reasoning default was not written.'
Assert-True ($resultAgain -match '(?ms)^\[models\.new_thread\].*?^model_reasoning_effort\s*=\s*"high"\s*$') 'The new-thread reasoning default was not written.'
Assert-True ($resultAgain -match '(?ms)^\[features\].*?^fast_mode\s*=\s*false\s*$') 'Fast was not disabled.'
Assert-True ($resultAgain -match '(?ms)^\[windows\].*?^sandbox\s*=\s*"elevated"\s*$') 'The completed Windows setup marker was not preserved.'
Assert-True ($resultAgain -match '(?m)^approval_policy\s*=\s*"never"\s*$') 'The existing approval policy was not preserved.'
Assert-True ($resultAgain -match '(?m)^sandbox_mode\s*=\s*"danger-full-access"\s*$') 'The existing sandbox mode was not preserved.'
Assert-True ($resultAgain -match '(?m)^personality\s*=\s*"pragmatic"\s*$') 'An unrelated user preference was not preserved.'
Assert-True ($resultAgain -match '(?m)^base_url\s*=\s*"http://127\.0\.0\.1:18080/v1"\s*$') 'Router base URL is incorrect.'
Assert-True ($resultAgain -match '(?m)^experimental_bearer_token\s*=\s*"local-router-test-token"\s*$') 'The local Router bearer was not written.'
Assert-True ($resultAgain -match '(?m)^requires_openai_auth\s*=\s*true\s*$') 'Local Router must keep Codex account sign-in mode while using the local bearer.'
Assert-True ($resultAgain -match '(?ms)^\[desktop\].*?^enabled-reasoning-efforts\s*=\s*\["low", "medium", "high", "xhigh", "ultra", "max"\]\s*$') 'Desktop reasoning controls were not enabled.'
Assert-True ($resultAgain -match '(?ms)^\[model_providers\.openai\].*?^base_url\s*=\s*"https://openai-proxy\.example/v1"\s*$') 'An external OpenAI provider was not preserved.'
Assert-True ($resultAgain -match '(?ms)^\[model_providers\.ollama\].*?^base_url\s*=\s*"http://127\.0\.0\.1:11434/v1"\s*$') 'The user Ollama provider was not preserved.'
Assert-True ($resultAgain -match '(?ms)^\[model_providers\.lmstudio\].*?^base_url\s*=\s*"http://127\.0\.0\.1:1234/v1"\s*$') 'The user LM Studio provider was not preserved.'
Assert-True ($resultAgain -match '(?ms)^\[model_providers\.custom\].*?^name\s*=\s*"Unrelated provider"\s*$') 'An unrelated custom provider was overwritten instead of preserved.'
Assert-True ($resultAgain -match '(?ms)^\[mcp_servers\.user-tool\].*?^command\s*=\s*"user-mcp\.exe"\s*$') 'An unrelated MCP server was not preserved.'
Assert-True ($resultAgain -notmatch '18081|service_tier|disable_response_storage|openai_base_url') 'Legacy provider settings remain.'
Assert-True (([regex]::Matches($resultAgain, '(?m)^base_url\s*=')).Count -eq 5) 'A stale top-level base_url remains outside provider tables.'
Assert-True ($resultAgain -notmatch 'PROXY_MANAGED') 'A stale top-level proxy bearer remains.'
Assert-True (([regex]::Matches($resultAgain, '\[model_providers\.codex_router\]')).Count -eq 1) 'Router provider is duplicated.'
Assert-True ($resultAgain -notmatch '\[model_providers\.sub2api\]') 'The legacy sub2api provider remains.'

$ccProxyInput = @'
model = "old"

[model_providers.custom]
name = "Legacy Loopback Proxy"
base_url = "http://127.0.0.1:15721/v1"
experimental_bearer_token = "PROXY_MANAGED"
'@
$ccProxyResult = New-CodexRouterConfig -Content $ccProxyInput -Model 'gpt-5.6-sol' -CatalogPath $catalog -LocalApiKey $localKey -ReasoningEffort 'medium' -CodexHome $isolatedCodexHome
Assert-True ($ccProxyResult -notmatch '15721|PROXY_MANAGED') 'Legacy temporary loopback proxy was persisted in Router config.'
Assert-True ($ccProxyResult -match '127\.0\.0\.1:18080/v1') 'Router provider was not restored after removing the legacy proxy.'

$fastResult = New-CodexRouterConfig `
    -Content $resultAgain `
    -Model 'gpt-5.6-sol' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -ReasoningEffort 'xhigh' `
    -FastMode $true `
    -CodexHome $isolatedCodexHome
Assert-True ($fastResult -match '(?m)^service_tier\s*=\s*"fast"\s*$') 'Fast service tier was not written.'
Assert-True ($fastResult -match '(?ms)^\[models\.new_thread\].*?^service_tier\s*=\s*"fast"\s*$') 'New-thread Fast tier was not written.'
Assert-True ($fastResult -match '(?ms)^\[features\].*?^fast_mode\s*=\s*true\s*$') 'Fast feature was not enabled.'
$fastOffAgain = New-CodexRouterConfig -Content $fastResult -Model 'gpt-5.6-sol' -CatalogPath $catalog -LocalApiKey $localKey -BaseUrl 'http://127.0.0.1:18080' -ReasoningEffort 'medium' -FastMode $false -CodexHome $isolatedCodexHome
Assert-True ($fastOffAgain -notmatch '(?m)^service_tier\s*=') 'A stale Fast service tier remains after disabling Fast.'
Assert-True (([regex]::Matches($fastOffAgain, '(?m)^model_reasoning_effort\s*=')).Count -eq 2) 'Reasoning defaults were duplicated or lost.'

$apiOnlyResult = New-CodexRouterConfig `
    -Content $resultAgain `
    -Model 'gpt-5.6-sol' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -RequireOpenAiAuth $false
Assert-True ($apiOnlyResult -match '(?m)^requires_openai_auth\s*=\s*false\s*$') 'API-only mode still requires an official OpenAI login.'
$apiOnlyAgain = New-CodexRouterConfig `
    -Content $apiOnlyResult `
    -Model 'gpt-5.6-sol' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -CodexHome $isolatedCodexHome
Assert-True ($apiOnlyAgain -match '(?m)^requires_openai_auth\s*=\s*true\s*$') 'Saving the Router configuration must keep account sign-in mode by default.'

$officialResult = New-CodexRouterConfig `
    -Content $apiOnlyAgain `
    -Model 'gpt-5.6-sol' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -RequireOpenAiAuth $true `
    -CodexHome $isolatedCodexHome
Assert-True ($officialResult -match '(?m)^requires_openai_auth\s*=\s*true\s*$') 'Official Codex sign-in was not preserved on the Router provider.'
Assert-True ($officialResult -match '(?m)^model_catalog_json\s*=\s*".*model-catalog\.json"\s*$') 'Official sign-in lost the Router model catalog.'
Assert-True ($officialResult -match '(?m)^experimental_bearer_token\s*=\s*"local-router-test-token"\s*$') 'Official sign-in lost the local Router bearer.'

$legacyRouterConfig = @'
model_provider = "custom"
model = "gpt-5.6-sol"

[windows]
sandbox = "unelevated"

[model_providers.custom]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
wire_api = "responses"
requires_openai_auth = true
'@
$migratedPermissions = New-CodexRouterConfig `
    -Content $legacyRouterConfig `
    -PermissionSourceContent $inputConfig `
    -Model 'gpt-5.6-sol' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -CodexHome $isolatedCodexHome
Assert-True ($migratedPermissions -match '(?m)^model_provider\s*=\s*"codex_router"\s*$') 'Legacy custom Router provider was not migrated to codex_router.'
Assert-True ($migratedPermissions -match '(?m)^requires_openai_auth\s*=\s*true\s*$') 'Local Router must keep account sign-in mode and still load the Router catalog.'
Assert-True ($migratedPermissions -notmatch '(?ms)^\[model_providers\.custom\].*?^name\s*=\s*"Codex-Router"\s*$') 'Legacy custom Codex-Router block was not removed after migration.'
Assert-True ($migratedPermissions -match '(?ms)^\[windows\].*?^sandbox\s*=\s*"elevated"\s*$') 'An older Router sandbox downgrade was not migrated back to the completed elevated marker.'
Assert-True ($migratedPermissions -match '(?m)^approval_policy\s*=\s*"never"\s*$') 'Approval policy was not recovered from the permission baseline.'
Assert-True ($migratedPermissions -match '(?m)^sandbox_mode\s*=\s*"danger-full-access"\s*$') 'Sandbox mode was not recovered from the permission baseline.'

$chiralCollision = @'
model_provider = "custom"
model = "grok-4.5"

[model_providers.custom]
name = "micu"
base_url = "https://api.430123.xyz/v1"
requires_openai_auth = true
experimental_bearer_token = "sk-chiral-not-for-router"
'@
$chiralResult = New-CodexRouterConfig `
    -Content $chiralCollision `
    -Model 'grok-4.5' `
    -CatalogPath $catalog `
    -LocalApiKey $localKey `
    -BaseUrl 'http://127.0.0.1:18080' `
    -CodexHome $isolatedCodexHome
Assert-True ($chiralResult -match '(?m)^model_provider\s*=\s*"codex_router"\s*$') 'Chiral custom provider kept ownership of the active model_provider.'
Assert-True ($chiralResult -match '(?ms)^\[model_providers\.codex_router\].*?^base_url\s*=\s*"http://127\.0\.0\.1:18080/v1"\s*$') 'Router provider was not written beside the Chiral profile.'
Assert-True ($chiralResult -match '(?ms)^\[model_providers\.custom\].*?^name\s*=\s*"micu"\s*$') 'Chiral/micu custom provider was destroyed instead of preserved.'
$routerBlock = [regex]::Match($chiralResult, '(?ms)^\[model_providers\.codex_router\]\s*.*?(?=^\[|\z)').Value
Assert-True ($routerBlock -match '(?m)^requires_openai_auth\s*=\s*true\s*$') 'Local Router must keep account sign-in mode beside preserved third-party providers.'
Assert-True ($routerBlock -notmatch '430123|sk-chiral-not-for-router') 'Chiral credentials remained on the active Router route.'
Assert-True ($chiralResult -match '430123') 'Preserved Chiral profile lost its upstream URL.'

$routerConfig = '{"defaultModel":"second","models":[{"model":"first"},{"model":"second"}]}' | ConvertFrom-Json
Assert-True ((Get-CodexRouterDefaultModel -RouterConfig $routerConfig) -eq 'second') 'Explicit Router default model was ignored.'
$routerConfig.defaultModel = 'deleted-model'
Assert-True ((Get-CodexRouterDefaultModel -RouterConfig $routerConfig) -eq 'first') 'Invalid Router default model did not fall back to the first model.'
$legacyConfig = '{"models":[{"model":"legacy-first"}]}' | ConvertFrom-Json
Assert-True ((Get-CodexRouterDefaultModel -RouterConfig $legacyConfig) -eq 'legacy-first') 'Legacy Router config did not use its first model.'
Assert-True ((Get-CodexRouterDefaultModel -RouterConfig $null) -eq 'gpt-5.6-sol') 'Config-free Codex integration selected a model absent from the packaged catalog.'
$fallbackCatalog = Join-Path ([IO.Path]::GetTempPath()) ("codex-router-fallback-catalog-" + [Guid]::NewGuid().ToString('N') + '.json')
[IO.File]::WriteAllText($fallbackCatalog, '{"models":[{"slug":"catalog-first","supported_in_api":true,"default_reasoning_level":"high","additional_speed_tiers":[]}]}')
try {
    $catalogFallback = Get-CodexRouterModelDefaults -RouterConfig $null -CatalogPath $fallbackCatalog -Model 'missing-model'
    Assert-True ($catalogFallback.Model -eq 'catalog-first') 'A missing default model did not fall back to the first serviceable catalog entry.'
} finally {
    Remove-Item -LiteralPath $fallbackCatalog -Force -ErrorAction SilentlyContinue
}

Write-Output 'Codex integration configuration tests passed.'
