param(
    [string]$CodexHome,
    [string]$ConfigPath,
    [string]$OutputPath,
    [AllowNull()]$DiscoveredOAuthModelsByAccount
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = Get-RouterConfigPath -RouterRoot $routerRoot
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $routerRoot 'config\model-catalog.json'
}
if (-not (Test-Path -LiteralPath $ConfigPath)) {
    throw "Router configuration not found: $ConfigPath"
}

$config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$models = @($config.models)
foreach ($configuredModel in $models) {
    if ([string]$configuredModel.model -eq 'gpt-5.6') {
        $configuredModel.model = 'gpt-5.6-sol'
        if ([string]$configuredModel.alias -in @('gpt-5.6', 'GPT-5.6 (Sol)')) {
            $configuredModel.alias = 'ChatGPT-5.6-Sol'
        }
    }
}
if ($models.Count -eq 0) { throw 'At least one model is required to build the Codex catalog.' }
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
$routePlan = @(Get-RouterModelRoutePlan `
    -RouterConfig $config `
    -DiscoveredOAuthModelsByAccount $DiscoveredOAuthModelsByAccount)
$visibleRoutes = @($routePlan | Where-Object { $_.IncludeInCatalog })
if ($visibleRoutes.Count -eq 0) { throw 'At least one selected model is required to build the Codex catalog.' }

function Get-ReasoningSpec([string]$Model) {
    $name = $Model.ToLowerInvariant()
    if ($name.Contains('gpt-5.6-sol')) { return @{ Default='medium'; Levels=@('low','medium','high','xhigh','max','ultra'); Fast=$true } }
    if ($name.Contains('gpt-5.6-terra')) { return @{ Default='medium'; Levels=@('low','medium','high','xhigh','max','ultra'); Fast=$true } }
    if ($name.Contains('gpt-5.6-luna')) { return @{ Default='medium'; Levels=@('low','medium','high','xhigh','max'); Fast=$true } }
    if ($name.Contains('gpt-5.5') -or $name.Contains('gpt-5.4')) { return @{ Default='medium'; Levels=@('minimal','low','medium','high','xhigh'); Fast=$true } }
    if ($name.Contains('claude-opus-5') -or $name.Contains('claude-sonnet-5') -or $name.Contains('claude-fable-5')) { return @{ Default='high'; Levels=@('low','medium','high','xhigh','max'); Fast=$false } }
    if ($name.Contains('gemini-3')) { return @{ Default='high'; Levels=@('minimal','low','medium','high'); Fast=$false } }
    if ($name -eq 'k3' -or $name.StartsWith('k3-') -or $name.Contains('kimi-k3')) { return @{ Default='high'; Levels=@('low','high','max'); Fast=$false } }
    if ($name.Contains('kimi-for-coding') -or $name.Contains('kimi-k2.7')) { return @{ Default='high'; Levels=@('high'); Fast=$false } }
    if ($name.Contains('deepseek-v4')) { return @{ Default='high'; Levels=@('none','low','high','max'); Fast=$false } }
    if ($name.Contains('mimo-v2.5')) { return @{ Default='high'; Levels=@('high'); Fast=$false } }
    if ($name.Contains('deepseek')) { return @{ Default='high'; Levels=@('low','high','max'); Fast=$false } }
    if ($name.Contains('grok-4.5')) { return @{ Default='high'; Levels=@('low','medium','high'); Fast=$false } }
    if ($name.Contains('grok')) { return @{ Default='medium'; Levels=@('low','medium','high'); Fast=$false } }
    return @{ Default='medium'; Levels=@('medium'); Fast=$false }
}

function Get-ModelReasoningSpec($ModelConfig, $LegacyReasoning) {
    $allowed = @('none','minimal','low','medium','high','xhigh','max','ultra')
    $modeProperty = $ModelConfig.PSObject.Properties['reasoningMode']
    $manual = $null -ne $modeProperty -and [string]$modeProperty.Value -eq 'manual'
    $source = $ModelConfig
    if (-not $manual -and $null -ne $LegacyReasoning -and [string]$LegacyReasoning.mode -eq 'manual') {
        $manual = $true
        $source = $LegacyReasoning
    }
    if ($manual) {
        $levelsProperty = if ($source -eq $ModelConfig) { $source.PSObject.Properties['reasoningLevels'] } else { $source.PSObject.Properties['levels'] }
        $defaultProperty = if ($source -eq $ModelConfig) { $source.PSObject.Properties['defaultReasoningLevel'] } else { $source.PSObject.Properties['defaultLevel'] }
        $fastProperty = if ($source -eq $ModelConfig) { $source.PSObject.Properties['fastSupported'] } else { $source.PSObject.Properties['supportsFast'] }
        $levels = @()
        if ($null -ne $levelsProperty) {
            foreach ($value in @($levelsProperty.Value)) {
                $normalized = ([string]$value).Trim().ToLowerInvariant()
                if ($normalized -in $allowed -and $normalized -notin $levels) { $levels += $normalized }
            }
        }
        $default = if ($null -ne $defaultProperty) { ([string]$defaultProperty.Value).Trim().ToLowerInvariant() } else { '' }
        if ($levels.Count -gt 0) {
            if ($default -notin $levels) { $default = $levels[0] }
            return @{
                Default = $default
                Levels = $levels
                Fast = $null -ne $fastProperty -and [bool]$fastProperty.Value
            }
        }
    }
    return Get-ReasoningSpec -Model ([string]$ModelConfig.model)
}

function Get-ContextWindow([string]$Model) {
    $name = $Model.Trim().ToLowerInvariant()
    if ($name.Contains('gpt-5.6-sol') -or $name.Contains('gpt-5.6-terra') -or $name.Contains('gpt-5.6-luna')) { return 272000 }
    if ($name -eq 'k3' -or $name.Contains('kimi-k3')) { return 1048576 }
    if ($name.Contains('claude-opus-5') -or $name.Contains('claude-sonnet-5') -or $name.Contains('claude-fable-5')) { return 1000000 }
    if ($name.Contains('gemini-3') -or $name.Contains('mimo-v2.5') -or $name.Contains('deepseek-v4')) { return 1048576 }
    if ($name.Contains('kimi-for-coding') -or $name.Contains('k3-256k')) { return 262144 }
    if ($name.Contains('grok-4.5')) { return 500000 }
    return 128000
}

function Get-MultimodalDefault([string]$Model) {
    $name = $Model.Trim().ToLowerInvariant()
    $visionMarkers = @('vision','multimodal','qwen-vl','qwen2-vl','qwen2.5-vl','qwen3-vl','glm-4v','glm-4.1v','glm-4.5v','glm-4.6v','cogvlm','janus','pixtral')
    foreach ($marker in $visionMarkers) { if ($name.Contains($marker)) { return $true } }
    if ($name.EndsWith('-vl') -or $name.Contains('/vl-') -or $name.Contains('_vl')) { return $true }
    if ($name.Contains('deepseek') -or $name.Contains('glm')) { return $false }
    if ($name.Contains('qwen') -and ($name.Contains('coder') -or $name.Contains('code'))) { return $false }
    if ($name.Contains('mimo-v2.5-pro')) { return $false }
    if ($name.Contains('gpt-') -or $name.Contains('grok-4') -or $name.Contains('claude-') -or $name.Contains('gemini') -or $name.Contains('kimi') -or $name.Contains('moonshot') -or $name.Contains('mimo-v2.5') -or $name -eq 'k3' -or $name.StartsWith('k3-')) { return $true }
    return $false
}

$templatePath = @(
    (Join-Path $routerRoot 'config\models.json'),
    (Join-Path $routerRoot 'config\model-catalog.example.json')
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if ([string]::IsNullOrWhiteSpace([string]$templatePath)) {
    throw 'A complete Codex model catalog template is required.'
}
$templateDocument = Get-Content -LiteralPath $templatePath -Raw | ConvertFrom-Json
$templateModels = if ($null -ne $templateDocument.PSObject.Properties['models']) {
    @($templateDocument.models)
} else {
    @($templateDocument)
}
$modelTemplate = @($templateModels | Where-Object {
    -not [string]::IsNullOrWhiteSpace([string]$_.base_instructions) -and $null -ne $_.model_messages
}) | Select-Object -First 1
if ($null -eq $modelTemplate) {
    throw "Codex model catalog template is incomplete: $templatePath"
}

$catalogModels = @(for ($index = 0; $index -lt $visibleRoutes.Count; $index++) {
    $route = $visibleRoutes[$index]
    $model = $route.Model
    # Reasoning is owned by each model. The retired global override caused one
    # profile's manual value to silently replace every model's documented preset.
    $reasoning = Get-ModelReasoningSpec -ModelConfig $model -LegacyReasoning $null
    $multimodalProperty = $model.PSObject.Properties['multimodal']
    $supportsImages = if ($null -ne $multimodalProperty -and [string]$multimodalProperty.Value -eq 'true') {
        $true
    } elseif ($null -ne $multimodalProperty -and [string]$multimodalProperty.Value -eq 'false') {
        $false
    } else {
        Get-MultimodalDefault -Model ([string]$model.model)
    }
    $contextProperty = $model.PSObject.Properties['contextWindow']
    $contextWindow = if ($null -ne $contextProperty -and [long]$contextProperty.Value -gt 0) {
        [long]$contextProperty.Value
    } else {
        Get-ContextWindow -Model ([string]$model.model)
    }
    $compactProperty = $model.PSObject.Properties['autoCompactPercent']
    $compactPercent = if ($null -ne $compactProperty -and [int]$compactProperty.Value -ge 60 -and [int]$compactProperty.Value -le 90) {
        [int]$compactProperty.Value
    } else {
        80
    }
    $displayName = Get-RouterModelDisplayName -Model $model -Route $route
    $inputModalities = @('text')
    if ($supportsImages) { $inputModalities = @('text', 'image') }
    $speedTiers = @()
    if ($reasoning.Fast) { $speedTiers = @('fast') }
    $serviceTiers = @()
    if ($reasoning.Fast) {
        $serviceTiers = @([ordered]@{
            id = 'priority'
            name = 'Fast'
            description = '1.5x speed, increased usage'
        })
    }
    $catalogModel = $modelTemplate | ConvertTo-Json -Depth 100 | ConvertFrom-Json
    $catalogModel.slug = [string]$route.PublicModelId
    $catalogModel.display_name = $displayName
    $catalogModel.description = "Codex-Router model #$($index + 1)"
    $catalogModel.default_reasoning_level = [string]$reasoning.Default
    $catalogModel.supported_reasoning_levels = @($reasoning.Levels | ForEach-Object {
        [ordered]@{ effort = $_; description = "$_ reasoning level" }
    })
    $catalogModel.input_modalities = $inputModalities
    $catalogModel.supports_image_detail_original = $supportsImages
    $catalogModel.context_window = $contextWindow
    $catalogModel.max_context_window = $contextWindow
    $catalogModel.effective_context_window_percent = $compactPercent
    $catalogModel.shell_type = 'shell_command'
    $catalogModel.visibility = 'list'
    $catalogModel.supported_in_api = $true
    $catalogModel.priority = [int]$model.priority
    $catalogModel.additional_speed_tiers = $speedTiers
    $catalogModel.service_tiers = $serviceTiers
    $catalogModel.availability_nux = $null
    $catalogModel.upgrade = $null
    $catalogModel
})

$catalog = [ordered]@{
    fetched_at = [DateTime]::UtcNow.ToString('o')
    etag = 'codex-router-local-v2'
    client_version = [string]$config.version
    models = @($catalogModels)
}
$parent = Split-Path -Parent ([IO.Path]::GetFullPath($OutputPath))
[IO.Directory]::CreateDirectory($parent) | Out-Null
[IO.File]::WriteAllText(
    [IO.Path]::GetFullPath($OutputPath),
    ($catalog | ConvertTo-Json -Depth 100),
    [Text.UTF8Encoding]::new($false)
)
Write-Output "Codex model catalog generated: $OutputPath ($($catalogModels.Count) models)"
