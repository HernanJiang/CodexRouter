param(
    [string]$CodexHome
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$userProfile = [Environment]::GetFolderPath('UserProfile')
if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    if (-not [string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
        $CodexHome = $env:CODEX_HOME
    } else {
        $CodexHome = Join-Path $userProfile '.codex'
    }
}
$sourcePath = Join-Path ([IO.Path]::GetFullPath($CodexHome)) 'models_cache.json'
$outputPath = "$routerRoot\config\models.json"

if (-not (Test-Path -LiteralPath $sourcePath)) {
    if (Test-Path -LiteralPath $outputPath) {
        $existingCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
        $requiredSlugs = @('gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna', 'kimi-for-coding', 'kimi-for-coding-highspeed', 'grok-4.5', 'deepseek-v4-flash')
        $existingSlugs = @($existingCatalog.models | ForEach-Object { [string]$_.slug })
        if (@($requiredSlugs | Where-Object { $_ -notin $existingSlugs }).Count -eq 0) {
            Write-Output "Codex model cache was not found; kept the validated Router catalog: $outputPath"
            return
        }
    }
    throw "Codex model cache was not found and the existing Router catalog is incomplete: $sourcePath"
}

$source = Get-Content -LiteralPath $sourcePath -Raw | ConvertFrom-Json
$templates = @{}
foreach ($model in @($source.models)) { $templates[[string]$model.slug] = $model }

function Copy-JsonObject {
    param([Parameter(Mandatory)]$Value)
    return ($Value | ConvertTo-Json -Depth 100 | ConvertFrom-Json)
}

function New-ReasoningLevels {
    param([Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Efforts)

    $descriptions = @{
        none = 'No additional reasoning'
        minimal = 'Minimal reasoning for the fastest response'
        low = 'Fast responses with lighter reasoning'
        medium = 'Balanced speed and reasoning depth'
        high = 'Greater reasoning depth for complex problems'
        xhigh = 'Extra high reasoning depth for hard problems'
        max = 'Maximum reasoning depth for the hardest problems'
        ultra = 'Maximum reasoning with automatic task delegation'
    }
    return @($Efforts | ForEach-Object {
        [pscustomobject]@{ effort = $_; description = $descriptions[$_] }
    })
}

$specs = @(
    @{ Slug='gpt-5.6-sol'; Template='gpt-5.6-sol'; Name='GPT-5.6-Sol'; Description='Frontier agentic coding model.'; Default='low'; Reasoning=@('low','medium','high','xhigh','max','ultra'); Fast=$true; Context=272000; Modalities=@('text','image') },
    @{ Slug='gpt-5.6-terra'; Template='gpt-5.6-terra'; Name='GPT-5.6-Terra'; Description='Balanced agentic coding model for everyday work.'; Default='medium'; Reasoning=@('low','medium','high','xhigh','max','ultra'); Fast=$true; Context=272000; Modalities=@('text','image') },
    @{ Slug='gpt-5.6-luna'; Template='gpt-5.6-luna'; Name='GPT-5.6-Luna'; Description='Fast and affordable agentic coding model.'; Default='medium'; Reasoning=@('low','medium','high','xhigh','max'); Fast=$true; Context=272000; Modalities=@('text','image') },
    @{ Slug='kimi-for-coding'; Template='gpt-5.4-mini'; Name='Kimi for Coding'; Description='Kimi Coding Plan default model.'; Default='medium'; Reasoning=@(); Fast=$false; Context=262144; Modalities=@('text') },
    @{ Slug='kimi-for-coding-highspeed'; Template='gpt-5.4-mini'; Name='Kimi for Coding HighSpeed'; Description='Separate high-speed Kimi Coding Plan model.'; Default='medium'; Reasoning=@(); Fast=$false; Context=262144; Modalities=@('text') },
    @{ Slug='grok-4.5'; Template='gpt-5.4'; Name='Grok 4.5'; Description='Grok 4.5 through OpenRouter.'; Default='medium'; Reasoning=@('minimal','low','medium','high','xhigh'); Fast=$false; Context=500000; Modalities=@('text','image') },
    @{ Slug='deepseek-v4-flash'; Template='gpt-5.4'; Name='DeepSeek V4 Flash'; Description='DeepSeek V4 Flash through OpenRouter.'; Default='medium'; Reasoning=@('minimal','low','medium','high','xhigh'); Fast=$false; Context=1048576; Modalities=@('text') }
)

$models = [Collections.Generic.List[object]]::new()
for ($index = 0; $index -lt $specs.Count; $index++) {
    $spec = $specs[$index]
    if (-not $templates.ContainsKey($spec.Template)) {
        throw "Model template is missing from the Codex cache: $($spec.Template)"
    }

    $model = Copy-JsonObject -Value $templates[$spec.Template]
    $model.slug = $spec.Slug
    $model.display_name = $spec.Name
    $model.description = $spec.Description
    $model.default_reasoning_level = $spec.Default
    $model.supported_reasoning_levels = @(New-ReasoningLevels -Efforts $spec.Reasoning)
    $model.visibility = 'list'
    $model.supported_in_api = $true
    $model.priority = $index + 1
    if ($spec.Slug -ne $spec.Template) {
        $model.upgrade = $null
    }
    if ($spec.Fast) {
        $model.additional_speed_tiers = [string[]]@('fast')
        $model.service_tiers = [object[]]@(
            [pscustomobject]@{ id='priority'; name='Fast'; description='1.5x speed, increased usage' }
        )
    } else {
        $model.additional_speed_tiers = [string[]]@()
        $model.service_tiers = [object[]]@()
    }
    $model.context_window = $spec.Context
    $model.max_context_window = $spec.Context
    $model.input_modalities = @($spec.Modalities)
    if ($spec.Slug -notlike 'gpt-*') {
        $model.use_responses_lite = $false
        $model.supports_image_detail_original = $false
    }
    $models.Add($model)
}

$duplicates = $models | Group-Object slug | Where-Object Count -gt 1
if ($duplicates) { throw "Duplicate model slugs: $($duplicates.Name -join ', ')" }
if ($models.Count -ne 7) { throw "Expected 7 models, generated $($models.Count)." }

$catalog = [ordered]@{
    fetched_at = [DateTime]::UtcNow.ToString('o')
    etag = 'codex-router-local-v1'
    client_version = [string]$source.client_version
    models = $models
}
$json = $catalog | ConvertTo-Json -Depth 100
[IO.File]::WriteAllText($outputPath, $json, [Text.UTF8Encoding]::new($false))
Write-Output "Codex model catalog generated: $outputPath ($($models.Count) models)"
