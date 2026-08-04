Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = Join-Path ([IO.Path]::GetTempPath()) ("codex-router-catalog-test-" + [Guid]::NewGuid().ToString('N'))
try {
    [IO.Directory]::CreateDirectory($root) | Out-Null
    $configPath = Join-Path $root 'router.json'
    $outputPath = Join-Path $root 'nested\model-catalog.json'
    $config = @{
        version = 'test'
        reasoning = @{ mode = 'manual'; levels = @('high'); defaultLevel = 'high' }
        models = @(
            @{ model = 'k3-256k'; alias = 'Kimi K3 256K'; priority = 1; multimodal = 'auto'; credentialName = 'ModelApiKey-test' },
            @{ model = 'text-only'; alias = ''; priority = 2; multimodal = 'false'; credentialName = 'ModelApiKey-text' },
            @{ model = 'gpt-5.6-sol'; priority = 3 },
            @{ model = 'gpt-5.6-terra'; priority = 31 },
            @{ model = 'gpt-5.6-luna'; priority = 32 },
            @{ model = 'deepseek/deepseek-v4-pro'; priority = 4; multimodal = 'auto' },
            @{ model = 'grok-4.5'; priority = 5 },
            @{ model = 'kimi-for-coding'; priority = 6 },
            @{ model = 'claude-opus-5'; priority = 7 },
            @{ model = 'gemini-3.6-flash'; priority = 8 },
            @{ model = 'mimo-v2.5-pro'; priority = 9 },
            @{ model = 'custom-manual'; priority = 10; reasoningMode = 'manual'; reasoningLevels = @('low','xhigh'); defaultReasoningLevel = 'xhigh'; fastSupported = $true }
        )
    }
    [IO.File]::WriteAllText(
        $configPath,
        ($config | ConvertTo-Json -Depth 10),
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
        -CodexHome (Join-Path $root 'empty-codex-home') `
        -ConfigPath $configPath `
        -OutputPath $outputPath | Out-Null
    $catalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if (@($catalog.models).Count -ne 12) { throw 'The generated catalog did not deduplicate public model IDs.' }
    if (@($catalog.models | Where-Object { [string]$_.slug -ieq 'gpt-5.6-sol' }).Count -ne 1) { throw 'The GPT-5.6 Sol menu item is missing.' }
    if (@($catalog.models[0].input_modalities) -notcontains 'image') { throw 'Auto multimodal was not enabled.' }
    if (@($catalog.models[1].input_modalities) -contains 'image') { throw 'Explicit text-only override was ignored.' }
    if (@((@($catalog.models) | Where-Object slug -eq 'deepseek/deepseek-v4-pro').input_modalities) -contains 'image') { throw 'DeepSeek was incorrectly marked as multimodal.' }
    if ((@($catalog.models) | Where-Object slug -eq 'deepseek/deepseek-v4-pro').display_name -ne 'DeepSeek-V4-Flash') { throw 'DeepSeek recommended display name was not applied.' }
    if (@((@($catalog.models) | Where-Object slug -eq 'mimo-v2.5-pro').input_modalities) -contains 'image') { throw 'MiMo V2.5 Pro was incorrectly marked as multimodal.' }
    if (@((@($catalog.models) | Where-Object slug -eq 'gemini-3.6-flash').input_modalities) -notcontains 'image') { throw 'Gemini 3.6 Flash image support was not detected.' }
    if (@((@($catalog.models) | Where-Object slug -eq 'claude-opus-5').input_modalities) -notcontains 'image') { throw 'Claude Opus 5 image support was not detected.' }
    if (@((@($catalog.models) | Where-Object slug -eq 'grok-4.5').input_modalities) -notcontains 'image') { throw 'Grok 4.5 image support was not detected.' }
    foreach ($model in @($catalog.models)) {
        $levels = @($model.supported_reasoning_levels | ForEach-Object { [string]$_.effort })
        if ([string]::IsNullOrWhiteSpace([string]$model.default_reasoning_level)) { throw "Empty reasoning default for $($model.slug)." }
        if ($levels.Count -eq 0 -or [string]$model.default_reasoning_level -notin $levels) { throw "Invalid reasoning levels for $($model.slug)." }
    }
    $expected = @{
        'k3-256k' = @{ Default = 'high'; Levels = @('low','high','max') }
        'gpt-5.6-sol' = @{ Default = 'medium'; Levels = @('low','medium','high','xhigh','max','ultra') }
        'gpt-5.6-terra' = @{ Default = 'medium'; Levels = @('low','medium','high','xhigh','max','ultra') }
        'gpt-5.6-luna' = @{ Default = 'medium'; Levels = @('low','medium','high','xhigh','max') }
        'deepseek/deepseek-v4-pro' = @{ Default = 'high'; Levels = @('none','low','high','max') }
        'grok-4.5' = @{ Default = 'high'; Levels = @('low','medium','high') }
        'kimi-for-coding' = @{ Default = 'high'; Levels = @('high') }
        'claude-opus-5' = @{ Default = 'high'; Levels = @('low','medium','high','xhigh','max') }
        'gemini-3.6-flash' = @{ Default = 'high'; Levels = @('minimal','low','medium','high') }
        'mimo-v2.5-pro' = @{ Default = 'high'; Levels = @('high') }
        'custom-manual' = @{ Default = 'xhigh'; Levels = @('low','xhigh') }
    }
    foreach ($name in $expected.Keys) {
        $entry = @($catalog.models) | Where-Object { $_.slug -eq $name } | Select-Object -First 1
        $actualLevels = @($entry.supported_reasoning_levels | ForEach-Object { $_.effort })
        if ($entry.default_reasoning_level -ne $expected[$name].Default -or (($actualLevels -join ',') -ne ($expected[$name].Levels -join ','))) {
            throw "Unexpected reasoning preset for $name."
        }
    }
    foreach ($entry in @($catalog.models)) {
        if ($entry.input_modalities -isnot [System.Array]) { throw "input_modalities is not a JSON array for $($entry.slug)." }
        if ($entry.supported_reasoning_levels -isnot [System.Array]) { throw "supported_reasoning_levels is not a JSON array for $($entry.slug)." }
        if ($entry.additional_speed_tiers -isnot [System.Array]) { throw "additional_speed_tiers is not a JSON array for $($entry.slug)." }
        if ($entry.service_tiers -isnot [System.Array]) { throw "service_tiers is not a JSON array for $($entry.slug)." }
        if ([string]::IsNullOrWhiteSpace([string]$entry.base_instructions)) { throw "base_instructions is missing for $($entry.slug)." }
        if ($null -eq $entry.model_messages) { throw "model_messages is missing for $($entry.slug)." }
        if ([int]$entry.effective_context_window_percent -ne 80) { throw "Conservative context percentage is missing for $($entry.slug)." }
    }
    $gptSolSpeedTiers = (@($catalog.models) | Where-Object slug -eq 'gpt-5.6-sol').additional_speed_tiers
    if (@($gptSolSpeedTiers) -notcontains 'fast') { throw 'GPT-5.6 Sol did not advertise Fast.' }
    if ((@($catalog.models) | Where-Object slug -eq 'gpt-5.6-sol').default_reasoning_level -ne 'medium') { throw 'Legacy global reasoning incorrectly replaced the per-model official default.' }
    if ((@($catalog.models) | Where-Object slug -eq 'claude-opus-5').context_window -ne 1000000) { throw 'Claude Opus 5 context window is stale.' }
    foreach ($slug in @('deepseek/deepseek-v4-pro','gemini-3.6-flash','mimo-v2.5-pro')) {
        if ((@($catalog.models) | Where-Object slug -eq $slug).context_window -ne 1048576) { throw "Context window is stale for $slug." }
    }
    if ((@($catalog.models) | Where-Object slug -eq 'k3-256k').context_window -ne 262144) { throw 'Kimi K3 256K context window is stale.' }
    $raw = Get-Content -LiteralPath $outputPath -Raw
    if ($raw -match '(?i)api[_-]?key|secret|credentialName') { throw 'The generated catalog contains credential data.' }

    $singleConfig = @{
        version = 'single-model-test'
        models = @(
            @{ model = 'gpt-5.6-sol'; alias = 'GPT-5.6 Sol'; priority = 1 }
        )
    }
    [IO.File]::WriteAllText(
        $configPath,
        ($singleConfig | ConvertTo-Json -Depth 10),
        [Text.UTF8Encoding]::new($false)
    )
    $singleRunOutput = @(& (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
        -CodexHome (Join-Path $root 'empty-codex-home') `
        -ConfigPath $configPath `
        -OutputPath $outputPath)
    $singleCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if ($singleCatalog.models -isnot [System.Array]) { throw 'A one-model catalog was not serialized as a JSON array.' }
    if (@($singleCatalog.models).Count -ne 1) { throw 'A one-model catalog did not contain exactly one model.' }
    if ($singleCatalog.models[0].slug -ne 'gpt-5.6-sol') { throw 'The one-model catalog has the wrong slug.' }
    if ($singleCatalog.models[0].display_name -ne 'ChatGPT-5.6-Sol') { throw 'The one-model catalog has the wrong display name.' }
    if (($singleRunOutput -join "`n") -notmatch '\(1 models\)') { throw 'The one-model completion log did not report the model count.' }

    $mergedConfig = @{
        version = 'oauth-merge-test'
        defaultModel = 'gpt-5.6-sol'
        oauthFallback = @{ enabled = $true; preferOAuth = $true; officialPriority = 1; fallbackPriority = 100 }
        oauthAccountIds = @(42)
        models = @(
            @{ model = 'gpt-5.6-sol'; alias = 'GPT-5.6 Sol'; aliasCustomized = $false; priority = 1; source = 'oauth'; oauthAccountId = 42 },
            @{ model = 'gpt-5.6-sol'; alias = 'Chiral quota'; baseURL = 'https://api.430123.xyz/v1'; credentialName = 'ModelApiKey-chiral'; priority = 10 },
            @{ model = 'openai/gpt-5.6-sol'; alias = 'OpenRouter quota'; baseURL = 'https://openrouter.ai/api/v1'; credentialName = 'ModelApiKey-openrouter'; priority = 20 }
        )
    }
    [IO.File]::WriteAllText(
        $configPath,
        ($mergedConfig | ConvertTo-Json -Depth 10),
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') -ConfigPath $configPath -OutputPath $outputPath | Out-Null
    $mergedCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if (@($mergedCatalog.models).Count -ne 1) { throw 'Enabled OAuth fallback did not merge same-name channels.' }
    if ($mergedCatalog.models[0].slug -ne 'gpt-5.6-sol' -or $mergedCatalog.models[0].display_name -ne 'ChatGPT-5.6-Sol') {
        throw 'Enabled OAuth fallback did not preserve the OAuth route and display name.'
    }

    $implicitMergedConfig = @{
        version = 'implicit-oauth-merge-test'
        defaultModel = 'openai/gpt-5.6-sol'
        oauthFallback = @{ enabled = $true; preferOAuth = $true; officialPriority = 1; fallbackPriority = 100 }
        oauthAccountIds = @(42)
        models = @(
            @{ model = 'openai/gpt-5.6-sol'; alias = 'Chiral quota'; baseURL = 'https://api.430123.xyz/v1'; credentialName = 'ModelApiKey-chiral'; priority = 10 }
        )
    }
    [IO.File]::WriteAllText(
        $configPath,
        ($implicitMergedConfig | ConvertTo-Json -Depth 10),
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') `
        -ConfigPath $configPath `
        -OutputPath $outputPath `
        -DiscoveredOAuthModelsByAccount @{'42' = @('gpt-5.6-sol')} | Out-Null
    $implicitMergedCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if (@($implicitMergedCatalog.models).Count -ne 1 -or
        $implicitMergedCatalog.models[0].slug -ne 'gpt-5.6-sol') {
        throw 'Implicit OAuth discovery did not produce one stable merged catalog route.'
    }

    $splitConfig = $mergedConfig | ConvertTo-Json -Depth 10 | ConvertFrom-Json
    $splitConfig.oauthFallback.enabled = $false
    [IO.File]::WriteAllText(
        $configPath,
        ($splitConfig | ConvertTo-Json -Depth 10),
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') -ConfigPath $configPath -OutputPath $outputPath | Out-Null
    $splitCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if (@($splitCatalog.models).Count -ne 3) { throw 'Disabled OAuth fallback did not expose separate quota routes.' }
    if ((@($splitCatalog.models) | Where-Object display_name -eq 'ChatGPT-5.6-Sol(OAuth)').slug -ne 'gpt-5.6-sol') {
        throw 'The OAuth route did not retain its native model ID.'
    }
    foreach ($alias in @('Chiral quota', 'OpenRouter quota')) {
        $splitEntry = @($splitCatalog.models) | Where-Object display_name -eq $alias | Select-Object -First 1
        if ($null -eq $splitEntry -or [string]$splitEntry.slug -notmatch '--api-[0-9a-f]{12}$') {
            throw "The split API route for '$alias' does not have a stable local model ID."
        }
    }
    $firstSplitSlugs = @($splitCatalog.models | ForEach-Object { [string]$_.slug })
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') -ConfigPath $configPath -OutputPath $outputPath | Out-Null
    $secondSplitCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if (($firstSplitSlugs -join ',') -ne (@($secondSplitCatalog.models | ForEach-Object { [string]$_.slug }) -join ',')) {
        throw 'Split API route IDs changed between identical catalog builds.'
    }

    $splitConfig.models[0].alias = 'My paid account'
    $splitConfig.models[0].aliasCustomized = $true
    [IO.File]::WriteAllText($configPath, ($splitConfig | ConvertTo-Json -Depth 10), [Text.UTF8Encoding]::new($false))
    & (Join-Path $PSScriptRoot 'Build-ModelCatalog.ps1') -ConfigPath $configPath -OutputPath $outputPath | Out-Null
    $customCatalog = Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json
    if ((@($customCatalog.models) | Where-Object slug -eq 'gpt-5.6-sol').display_name -ne 'My paid account') {
        throw 'A user-customized OAuth display name was overwritten by the recommendation.'
    }

    Write-Output 'Fresh-machine model catalog test passed.'
} finally {
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
