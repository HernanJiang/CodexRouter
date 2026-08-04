Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$chiral = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://api.430123.xyz/v1' `
    -Extra @{}
Assert-True (
    [string]$chiral.Extra.openai_responses_mode -eq 'force_responses'
) 'Chiral did not use its documented native Codex Responses endpoint.'
Assert-True (
    @($chiral.OpenAICapabilities).Count -eq 0
) 'Chiral was incorrectly restricted to Chat Completions.'
Assert-True (
    $chiral.Extra.openai_compact_supported -eq $true
) 'The verified Chiral compact capability was not seeded.'

$openRouter = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://openrouter.ai/api/v1' `
    -Extra ([pscustomobject]@{ custom_flag = 'preserved' })
Assert-True (
    [string]$openRouter.Extra.custom_flag -eq 'preserved'
) 'Existing third-party account metadata was not preserved.'
Assert-True (
    [string]$openRouter.ResponsesMode -eq 'auto'
) 'OpenRouter did not receive the capability-probing mode.'
Assert-True (
    $openRouter.Extra.openai_compact_supported -eq $false
) 'The verified OpenRouter compact limitation was not seeded.'

foreach ($kimiBaseUrl in @(
    'https://api.kimi.com/coding/v1',
    'https://api.moonshot.ai/v1',
    'https://api.moonshot.cn/v1'
)) {
    $kimi = Get-RouterOpenAIChannelPolicy -BaseUrl $kimiBaseUrl -Extra @{}
    Assert-True (
        [string]$kimi.ResponsesMode -eq 'force_chat_completions'
    ) "Kimi endpoint was not routed through the required Chat Completions bridge: $kimiBaseUrl"
    Assert-True (
        @($kimi.OpenAICapabilities) -contains 'chat_completions'
    ) "Kimi endpoint did not advertise Chat Completions capability: $kimiBaseUrl"
    Assert-True (
        $kimi.Extra.openai_compact_supported -eq $false
    ) "Kimi endpoint incorrectly advertised Responses compact support: $kimiBaseUrl"
}

$kimiExplicitResponses = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://api.kimi.com/coding/v1' `
    -Extra @{ openai_responses_mode = 'force_responses' }
Assert-True (
    [string]$kimiExplicitResponses.ResponsesMode -eq 'force_responses'
) 'An explicit Kimi protocol override was overwritten.'

$explicitChat = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://chat-only.example.test/v1' `
    -Extra @{ openai_responses_mode = 'force_chat_completions' }
Assert-True (
    [string]$explicitChat.ResponsesMode -eq 'force_chat_completions'
) 'An explicit Chat Completions override was overwritten.'
Assert-True (
    @($explicitChat.OpenAICapabilities) -contains 'chat_completions'
) 'An explicit Chat Completions override was not advertised correctly.'

$probedChat = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://auto.example.test/v1' `
    -Extra @{ openai_responses_supported = $false }
Assert-True (
    [string]$probedChat.ResponsesMode -eq 'auto'
) 'A negative Responses probe unexpectedly disabled auto mode.'
Assert-True (
    @($probedChat.OpenAICapabilities) -contains 'chat_completions'
) 'A negative Responses probe was not restricted to Chat Completions.'

$explicitCompact = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://openrouter.ai/api/v1' `
    -Extra @{ openai_compact_supported = $true }
Assert-True (
    $explicitCompact.Extra.openai_compact_supported -eq $true
) 'Explicit compact capability metadata was overwritten.'

$unknownHost = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://new-provider.example.test/v1' `
    -Extra @{}
Assert-True (
    -not $unknownHost.Extra.Contains('openai_compact_supported')
) 'An unknown provider received an invented compact capability.'

$explicitResponses = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://relay.example.test/v1' `
    -Extra @{ openai_responses_mode = 'force_responses' }
Assert-True (
    [string]$explicitResponses.ResponsesMode -eq 'force_responses'
) 'An explicit native Responses override was overwritten.'
Assert-True (
    @($explicitResponses.OpenAICapabilities).Count -eq 0
) 'A native Responses override was incorrectly restricted to Chat Completions.'

$official = Get-RouterOpenAIChannelPolicy `
    -BaseUrl 'https://api.openai.com/v1' `
    -Extra @{}
Assert-True $official.IsOfficialOpenAI 'The official OpenAI API host was not recognized.'
Assert-True (
    -not $official.Extra.Contains('openai_responses_mode')
) 'The official OpenAI API was unnecessarily forced through the compatibility layer.'

$startRouterSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Start-Router.ps1') -Raw
Assert-True (
    $startRouterSource -notmatch '(?m)^\s*CONFIG_FILE\s*='
) 'Start-Router.ps1 must not replace the complete generated Sub2API config with a partial override.'
Assert-True (
    $startRouterSource -match 'DATA_DIR\s*=\s*\(Join-Path \$dataRoot ''sub2api''\)'
) 'Start-Router.ps1 does not use the persistent Sub2API data directory and its generated config.'
Assert-True (
    $startRouterSource -match "GATEWAY_OPENAI_FIRST_OUTPUT_TIMEOUT_SECONDS\s*=\s*'60'"
) 'The normal first-output failover deadline is missing or unexpected.'
Assert-True (
    $startRouterSource -match "GATEWAY_OPENAI_HIGH_EFFORT_FIRST_OUTPUT_TIMEOUT_SECONDS\s*=\s*'300'"
) 'The high-effort first-output deadline is missing or unexpected.'
Assert-True (
    $startRouterSource -match "GATEWAY_STREAM_DATA_INTERVAL_TIMEOUT\s*=\s*'60'"
) 'The stalled-stream failover deadline is missing or unexpected.'
Assert-True (
    $startRouterSource -match '\$environment\.UPDATE_PROXY_URL\s*=\s*\$proxySettings\.ProxyUrl'
) 'The Sub2API update client does not inherit the discovered outbound proxy.'
Assert-True (
    $startRouterSource -match '\$environment\.UPDATE_PROXY_URL\s*=\s*\$null'
) 'Direct mode does not clear the Sub2API update proxy override.'

Write-Output 'OpenAI channel policy tests passed.'
