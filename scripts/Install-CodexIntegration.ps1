param(
    [string]$CodexHome,
    [switch]$SkipCCSwitchSync
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
$CodexHome = [IO.Path]::GetFullPath($CodexHome)
$codexConfig = Join-Path $CodexHome 'config.toml'
$codexAuth = Join-Path $CodexHome 'auth.json'
$ccSwitchDb = Join-Path $userProfile '.cc-switch\cc-switch.db'
$ccSwitchProviderId = '84b1adc1-e565-4eed-9919-d7bc4149134f'
$python = Join-Path $userProfile '.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe'

function Set-TopLevelTomlValue {
    param(
        [string]$Content,
        [string]$Key,
        [string]$Value
    )

    $pattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=.*$'
    $line = "$Key = $Value"
    if ($Content -match $pattern) {
        return [Text.RegularExpressions.Regex]::Replace($Content, $pattern, $line, 1)
    }

    $firstTable = [Text.RegularExpressions.Regex]::Match($Content, '(?m)^\[')
    if ($firstTable.Success) {
        return $Content.Insert($firstTable.Index, "$line`r`n")
    }
    if ([string]::IsNullOrWhiteSpace($Content)) {
        return "$line`r`n"
    }
    return $Content.TrimEnd() + "`r`n$line`r`n"
}

& "$routerRoot\scripts\Build-ModelCatalog.ps1" -CodexHome $CodexHome

[IO.Directory]::CreateDirectory($CodexHome) | Out-Null

# Cache cleaners may leave a dangling relocation junction behind. Restore its
# target before the packaged app extracts its bundled executables.
$localOpenAIRoot = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'OpenAI'
$localOpenAIItem = Get-Item -LiteralPath $localOpenAIRoot -Force -ErrorAction SilentlyContinue
if ($null -ne $localOpenAIItem -and ($localOpenAIItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    foreach ($target in @($localOpenAIItem.Target)) {
        if (-not [string]::IsNullOrWhiteSpace($target)) {
            $resolvedTarget = if ([IO.Path]::IsPathRooted($target)) {
                $target
            } else {
                Join-Path (Split-Path -Parent $localOpenAIRoot) $target
            }
            [IO.Directory]::CreateDirectory($resolvedTarget) | Out-Null
        }
    }
} else {
    [IO.Directory]::CreateDirectory($localOpenAIRoot) | Out-Null
}
$localCodexRoot = Join-Path $localOpenAIRoot 'Codex'
[IO.Directory]::CreateDirectory((Join-Path $localCodexRoot 'bin')) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $localCodexRoot 'runtimes\cua_node')) | Out-Null

$configExisted = Test-Path -LiteralPath $codexConfig
$text = if ($configExisted) { [IO.File]::ReadAllText($codexConfig) } else { '' }
$text = Set-TopLevelTomlValue -Content $text -Key 'model_provider' -Value '"sub2api"'
$text = Set-TopLevelTomlValue -Content $text -Key 'model' -Value '"deepseek-v4-flash"'
$text = [Text.RegularExpressions.Regex]::Replace(
    $text,
    '(?m)^service_tier\s*=.*(?:\r?\n)?',
    '')
$text = [Text.RegularExpressions.Regex]::Replace(
    $text,
    '(?m)^disable_response_storage\s*=.*(?:\r?\n)?',
    '')
$text = [Text.RegularExpressions.Regex]::Replace(
    $text,
    '(?m)^openai_base_url\s*=.*(?:\r?\n)?',
    '')

$catalogPath = Join-Path $routerRoot 'config\models.json'
$escapedCatalogPath = $catalogPath.Replace('\', '\\').Replace('"', '\"')
$text = Set-TopLevelTomlValue -Content $text -Key 'model_catalog_json' -Value ('"' + $escapedCatalogPath + '"')

$providerPattern = '(?ms)^\[model_providers\.(?:custom|sub2api)(?:\.[^\]]+)?\]\r?\n.*?(?=^\[|\z)'
$text = [Text.RegularExpressions.Regex]::Replace($text, $providerPattern, '')
$reservedProviderPattern = '(?ms)^\[model_providers\.(?:openai|ollama|lmstudio)(?:\.[^\]]+)?\]\r?\n.*?(?=^\[|\z)'
$text = [Text.RegularExpressions.Regex]::Replace($text, $reservedProviderPattern, '')
$providerBlock = @'
[model_providers.sub2api]
request_max_retries = 4
stream_max_retries = 5
stream_idle_timeout_ms = 720000
name = "Codex Unified Router"
wire_api = "responses"
base_url = "http://127.0.0.1:18081/v1"
requires_openai_auth = true
supports_websockets = false

'@
$text = $text.TrimEnd() + "`r`n`r`n$providerBlock"

if ($text -match '(?m)^fast_mode\s*=') {
    $text = [Text.RegularExpressions.Regex]::Replace($text, '(?m)^fast_mode\s*=.*$', 'fast_mode = true')
} elseif ($text -match '(?m)^\[features\]\s*$') {
    $text = [Text.RegularExpressions.Regex]::Replace($text, '(?m)^\[features\]\s*$', "[features]`r`nfast_mode = true", 1)
} else {
    $text = $text.TrimEnd() + "`r`n`r`n[features]`r`nfast_mode = true`r`n"
}

if ($text -match '(?m)^\[model_providers\.(?:openai|ollama|lmstudio)(?:\.|\])') {
    throw 'Reserved built-in provider override remains in generated Codex configuration'
}
if ($text -notmatch '(?m)^model_provider\s*=\s*"sub2api"\s*$' -or
    $text -notmatch '(?m)^model\s*=\s*"deepseek-v4-flash"\s*$' -or
    $text -notmatch '(?m)^supports_websockets\s*=\s*false\s*$') {
    throw 'Generated Codex configuration is missing required router defaults'
}

$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$backupPath = "$codexConfig.codex-router-$timestamp.bak"
if ($configExisted) {
    [IO.File]::Copy($codexConfig, $backupPath, $false)
}
[IO.File]::WriteAllText($codexConfig, $text, [Text.UTF8Encoding]::new($false))

if (-not $SkipCCSwitchSync) {
    if (-not (Test-Path -LiteralPath $ccSwitchDb)) {
        Write-Warning "CC Switch database was not found; skipped synchronization: $ccSwitchDb"
    } elseif (-not (Test-Path -LiteralPath $codexAuth)) {
        Write-Warning "Codex auth.json was not found; skipped CC Switch synchronization until Codex login is restored"
    } else {
        if (-not (Test-Path -LiteralPath $python)) { throw "Bundled Python was not found: $python" }
        & $python `
            "$routerRoot\scripts\Sync-CCSwitchConfig.py" `
            --db $ccSwitchDb `
            --provider-id $ccSwitchProviderId `
            --config $codexConfig `
            --auth $codexAuth `
            --backup-dir (Join-Path $userProfile '.cc-switch\backups')
        if ($LASTEXITCODE -ne 0) { throw "CC Switch synchronization failed with exit code $LASTEXITCODE" }
    }
}

if ($configExisted) {
    Write-Output "Codex configuration installed in $CodexHome; backup: $backupPath"
} else {
    Write-Output "Codex configuration created in $CodexHome"
}
