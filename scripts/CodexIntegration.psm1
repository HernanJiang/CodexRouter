Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function ConvertTo-CodexTomlString {
    param([Parameter(Mandatory = $true)][string]$Value)
    return $Value.Replace('\', '\\').Replace('"', '\"')
}

function Get-CodexRouterDefaultModel {
    param(
        [AllowNull()]$RouterConfig,
        [string]$Fallback = 'gpt-5.6-sol'
    )

    if ($null -eq $RouterConfig) { return $Fallback }
    $modelsProperty = $RouterConfig.PSObject.Properties['models']
    if ($null -eq $modelsProperty) { return $Fallback }
    $models = @($modelsProperty.Value)
    if ($models.Count -eq 0) { return $Fallback }

    $defaultProperty = $RouterConfig.PSObject.Properties['defaultModel']
    if ($null -ne $defaultProperty -and -not [string]::IsNullOrWhiteSpace([string]$defaultProperty.Value)) {
        $requested = [string]$defaultProperty.Value
        if ($models | Where-Object { [string]$_.model -eq $requested } | Select-Object -First 1) {
            return $requested
        }
    }
    return [string]$models[0].model
}

function Set-CodexTopLevelValue {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][string]$Value
    )

    $firstTable = [Text.RegularExpressions.Regex]::Match($Content, '(?m)^\[')
    $headLength = if ($firstTable.Success) { $firstTable.Index } else { $Content.Length }
    $head = $Content.Substring(0, $headLength)
    $tail = $Content.Substring($headLength)
    $pattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=.*(?:\r?\n)?'
    $line = "$Key = $Value`r`n"
    $matches = [Text.RegularExpressions.Regex]::Matches($head, $pattern)
    if ($matches.Count -gt 0) {
        $firstIndex = $matches[0].Index
        $head = [Text.RegularExpressions.Regex]::Replace($head, $pattern, '')
        $head = $head.Insert([Math]::Min($firstIndex, $head.Length), $line)
    } else {
        $head = $head.TrimEnd() + $(if ([string]::IsNullOrWhiteSpace($head)) { '' } else { "`r`n" }) + $line
    }
    return $head + $tail
}

function Remove-CodexTopLevelValues {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string[]]$Keys
    )

    $firstTable = [Text.RegularExpressions.Regex]::Match($Content, '(?m)^\[')
    $headLength = if ($firstTable.Success) { $firstTable.Index } else { $Content.Length }
    $head = $Content.Substring(0, $headLength)
    $tail = $Content.Substring($headLength)
    foreach ($key in $Keys) {
        $pattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($key) + '\s*=.*(?:\r?\n)?'
        $head = [Text.RegularExpressions.Regex]::Replace($head, $pattern, '')
    }
    return $head + $tail
}

function Set-CodexTableValue {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Table,
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][string]$Value
    )

    $tablePattern = '(?ms)^\[' + [Text.RegularExpressions.Regex]::Escape($Table) + '\]\s*\r?\n.*?(?=^\[|\z)'
    $tableMatch = [Text.RegularExpressions.Regex]::Match($Content, $tablePattern)
    $line = "$Key = $Value"
    if ($tableMatch.Success) {
        $section = $tableMatch.Value
        $keyPattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=.*$'
        if ($section -match $keyPattern) {
            $updated = [Text.RegularExpressions.Regex]::Replace($section, $keyPattern, $line)
        } else {
            $headerEnd = $section.IndexOf("`n") + 1
            $updated = $section.Insert($headerEnd, "$line`r`n")
        }
        return $Content.Remove($tableMatch.Index, $tableMatch.Length).Insert($tableMatch.Index, $updated)
    }
    return $Content.TrimEnd() + "`r`n`r`n[$Table]`r`n$line`r`n"
}

function Remove-CodexTableValue {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Table,
        [Parameter(Mandatory = $true)][string]$Key
    )
    $tablePattern = '(?ms)^\[' + [Text.RegularExpressions.Regex]::Escape($Table) + '\]\s*\r?\n.*?(?=^\[|\z)'
    $tableMatch = [Text.RegularExpressions.Regex]::Match($Content, $tablePattern)
    if (-not $tableMatch.Success) { return $Content }
    $keyPattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=.*(?:\r?\n)?'
    $updated = [Text.RegularExpressions.Regex]::Replace($tableMatch.Value, $keyPattern, '')
    return $Content.Remove($tableMatch.Index, $tableMatch.Length).Insert($tableMatch.Index, $updated)
}

function Get-CodexTopLevelRawValue {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Key
    )

    $firstTable = [Text.RegularExpressions.Regex]::Match($Content, '(?m)^\[')
    $headLength = if ($firstTable.Success) { $firstTable.Index } else { $Content.Length }
    $head = $Content.Substring(0, $headLength)
    $match = [Text.RegularExpressions.Regex]::Match(
        $head,
        '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=\s*(?<value>.+?)\s*$')
    if (-not $match.Success) { return $null }
    return $match.Groups['value'].Value
}

function Get-CodexTableRawValue {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Table,
        [Parameter(Mandatory = $true)][string]$Key
    )

    $tablePattern = '(?ms)^\[' + [Text.RegularExpressions.Regex]::Escape($Table) + '\]\s*\r?\n(?<body>.*?)(?=^\[|\z)'
    $tableMatch = [Text.RegularExpressions.Regex]::Match($Content, $tablePattern)
    if (-not $tableMatch.Success) { return $null }
    $match = [Text.RegularExpressions.Regex]::Match(
        $tableMatch.Groups['body'].Value,
        '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=\s*(?<value>.+?)\s*$')
    if (-not $match.Success) { return $null }
    return $match.Groups['value'].Value
}

function Copy-CodexPermissionSettings {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$SourceContent
    )

    $text = $Content
    foreach ($key in @('approval_policy', 'sandbox_mode')) {
        $value = Get-CodexTopLevelRawValue -Content $SourceContent -Key $key
        if (-not [string]::IsNullOrWhiteSpace($value)) {
            $text = Set-CodexTopLevelValue -Content $text -Key $key -Value $value
        }
    }
    # Codex Desktop owns this one-time Windows installation state.
    # Prefer a completed elevated marker from either the live config or the
    # permission baseline. Never delete a completion marker during Apply.
    $liveWindowsSandbox = Get-CodexTableRawValue -Content $text -Table 'windows' -Key 'sandbox'
    $sourceWindowsSandbox = Get-CodexTableRawValue -Content $SourceContent -Table 'windows' -Key 'sandbox'
    $liveValue = if ($null -eq $liveWindowsSandbox) { '' } else { $liveWindowsSandbox.Trim().Trim('"') }
    $sourceValue = if ($null -eq $sourceWindowsSandbox) { '' } else { $sourceWindowsSandbox.Trim().Trim('"') }
    if ($liveValue -eq 'elevated') {
        # already complete
    }
    elseif ($sourceValue -eq 'elevated') {
        $text = Set-CodexTableValue -Content $text -Table 'windows' -Key 'sandbox' -Value '"elevated"'
    }
    elseif ([string]::IsNullOrWhiteSpace($liveValue) -and -not [string]::IsNullOrWhiteSpace($sourceWindowsSandbox)) {
        $text = Set-CodexTableValue -Content $text -Table 'windows' -Key 'sandbox' -Value $sourceWindowsSandbox
    }
    return $text
}

function Get-CodexPermissionSourceContent {
    param(
        [Parameter(Mandatory = $true)][string]$CodexConfigPath,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content
    )

    # First-time installs already carry the user's permission settings in Content.
    # Only consult Router backups when upgrading a configuration managed by an
    # older Router release that forced [windows].sandbox to "unelevated".
    $routerBody = Get-CodexRouterProviderBody -Content $Content
    $routerProvider = -not [string]::IsNullOrWhiteSpace($routerBody) -and
        $routerBody -match '(?m)^base_url\s*=\s*"http://(?:127\.0\.0\.1|localhost):'
    if (-not $routerProvider) { return $Content }

    $directory = Split-Path -Parent $CodexConfigPath
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) { return $Content }
    foreach ($backup in @(Get-ChildItem -LiteralPath $directory -Filter 'config.toml.codex-router-*.bak' -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTimeUtc, Name -Descending)) {
        try { $candidate = [IO.File]::ReadAllText($backup.FullName) } catch { continue }
        $approval = Get-CodexTopLevelRawValue -Content $candidate -Key 'approval_policy'
        $sandboxMode = Get-CodexTopLevelRawValue -Content $candidate -Key 'sandbox_mode'
        $windowsSandbox = Get-CodexTableRawValue -Content $candidate -Table 'windows' -Key 'sandbox'
        if (-not [string]::IsNullOrWhiteSpace($approval) -or
            -not [string]::IsNullOrWhiteSpace($sandboxMode) -or
            -not [string]::IsNullOrWhiteSpace($windowsSandbox)) {
            return $candidate
        }
    }
    return $Content
}

function Get-CodexRouterModelDefaults {
    param(
        [AllowNull()]$RouterConfig,
        [Parameter(Mandatory = $true)][string]$CatalogPath,
        [AllowEmptyString()][string]$Model = ''
    )
    $configuredModel = Get-CodexRouterDefaultModel -RouterConfig $RouterConfig
    if ([string]::IsNullOrWhiteSpace($Model)) { $Model = $configuredModel }
    $effort = 'medium'
    $supportsFast = $false
    if (Test-Path -LiteralPath $CatalogPath) {
        $catalog = Get-Content -LiteralPath $CatalogPath -Raw | ConvertFrom-Json
        $entry = @($catalog.models) | Where-Object { [string]$_.slug -eq $Model } | Select-Object -First 1
        if ($null -eq $entry) {
            $entry = @($catalog.models) | Where-Object {
                -not [string]::IsNullOrWhiteSpace([string]$_.slug) -and
                ($null -eq $_.PSObject.Properties['supported_in_api'] -or [bool]$_.supported_in_api)
            } | Select-Object -First 1
            if ($null -ne $entry) { $Model = [string]$entry.slug }
        }
        if ($null -ne $entry) {
            if (-not [string]::IsNullOrWhiteSpace([string]$entry.default_reasoning_level)) {
                $effort = ([string]$entry.default_reasoning_level).Trim().ToLowerInvariant()
            }
            $supportsFast = @($entry.additional_speed_tiers) -contains 'fast'
        }
    }
    $fastMode = $false
    if ($null -ne $RouterConfig -and $null -ne $RouterConfig.PSObject.Properties['models']) {
        $modelConfig = @($RouterConfig.models) | Where-Object { [string]$_.model -eq $configuredModel } | Select-Object -First 1
        if ($null -ne $modelConfig -and $null -ne $modelConfig.PSObject.Properties['fastMode']) {
            $fastMode = $supportsFast -and [bool]$modelConfig.fastMode
        }
    }
    return [pscustomobject]@{ Model = $Model; ReasoningEffort = $effort; FastMode = $fastMode }
}

function Remove-CodexTable {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Table
    )
    $pattern = '(?ms)^\[' + [Text.RegularExpressions.Regex]::Escape($Table) + '\]\s*\r?\n.*?(?=^\[|\z)'
    return [Text.RegularExpressions.Regex]::Replace($Content, $pattern, '')
}

function Get-CodexRouterProviderBody {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content)

    foreach ($providerId in @('codex_router', 'custom', 'sub2api')) {
        $escaped = [Text.RegularExpressions.Regex]::Escape($providerId)
        $blockMatch = [Text.RegularExpressions.Regex]::Match(
            $Content,
            "(?ms)^\[model_providers\.$escaped\]\s*(?<body>.*?)(?=^\[|\z)")
        if (-not $blockMatch.Success) { continue }
        $body = $blockMatch.Groups['body'].Value
        if ($providerId -eq 'codex_router' -or
            $body -match '(?m)^name\s*=\s*"Codex-Router"\s*$') {
            return $body
        }
    }
    return ''
}

function Remove-LegacyCodexRouterProvider {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content)
    $text = $Content
    foreach ($providerId in @('custom', 'sub2api')) {
        $pattern = '(?ms)^\[' + [Text.RegularExpressions.Regex]::Escape("model_providers.$providerId") + '\]\s*\r?\n.*?(?=^\[|\z)'
        $match = [Text.RegularExpressions.Regex]::Match($text, $pattern)
        if ($match.Success -and ($match.Value -match '(?m)^name\s*=\s*"Codex(?:-Router|(?: Unified)? Router)"\s*$' -or
                $match.Value -match '127\.0\.0\.1:18081')) {
            $text = $text.Remove($match.Index, $match.Length)
        }
    }
    return $text
}

function Remove-LegacyCodexLoopbackProxyProvider {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content)
    $pattern = '(?ms)^\[model_providers\.custom\]\s*\r?\n.*?(?=^\[|\z)'
    $match = [Text.RegularExpressions.Regex]::Match($Content, $pattern)
    if ($match.Success -and
        $match.Value -match '127\.0\.0\.1:15721/v1' -and
        $match.Value -match 'experimental_bearer_token\s*=\s*"PROXY_MANAGED"') {
        return $Content.Remove($match.Index, $match.Length)
    }
    return $Content
}

function Test-CodexHomeHasChatGptAuth {
    param([AllowEmptyString()][string]$CodexHome = '')
    $home = if ([string]::IsNullOrWhiteSpace($CodexHome)) {
        if ($env:CODEX_HOME) { [string]$env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
    } else { $CodexHome }
    $authPath = Join-Path $home 'auth.json'
    if (-not (Test-Path -LiteralPath $authPath)) { return $false }
    try {
        $auth = Get-Content -LiteralPath $authPath -Raw | ConvertFrom-Json
        $mode = [string]$auth.auth_mode
        if ($mode -ne 'chatgpt') { return $false }
        $tokens = $auth.tokens
        return $null -ne $tokens
    } catch {
        return $false
    }
}

function Get-CodexRouterRequiresOpenAiAuth {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [AllowEmptyString()][string]$CodexHome = ''
    )

    # Always keep Codex in account/sign-in UI mode. Local Router traffic still
    # uses experimental_bearer_token; Apply must never force API-only mode or
    # hide the Router model catalog behind a third-party login state.
    $null = $Content
    $null = $CodexHome
    return $true
}

function New-CodexRouterConfig {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [Parameter(Mandatory = $true)][string]$Model,
        [Parameter(Mandatory = $true)][string]$CatalogPath,
        [Parameter(Mandatory = $true)][string]$LocalApiKey,
        [string]$BaseUrl = 'http://127.0.0.1:18080',
        [string]$ReasoningEffort = 'medium',
        [bool]$FastMode = $false,
        [Nullable[bool]]$RequireOpenAiAuth = $null,
        [AllowNull()][string]$PermissionSourceContent = $null,
        [AllowEmptyString()][string]$CodexHome = ''
    )

    $uri = $null
    if (-not [Uri]::TryCreate($BaseUrl.TrimEnd('/'), [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -ne 'http' -or
        $uri.Host -notin @('127.0.0.1', 'localhost')) {
        throw 'Codex-Router provider URL must be a local HTTP URL (127.0.0.1 or localhost).'
    }
    $base = $uri.GetLeftPart([UriPartial]::Authority).TrimEnd('/')
    $ReasoningEffort = $ReasoningEffort.Trim().ToLowerInvariant()
    if ($ReasoningEffort -notin @('none','minimal','low','medium','high','xhigh','max','ultra')) {
        throw "Unsupported Codex reasoning effort: '$ReasoningEffort'."
    }
    if ([string]::IsNullOrWhiteSpace($LocalApiKey)) {
        throw 'The local Router credential is required for Codex integration.'
    }
    if ($null -eq $RequireOpenAiAuth) {
        $RequireOpenAiAuth = Get-CodexRouterRequiresOpenAiAuth -Content $Content -CodexHome $CodexHome
    }

    $text = Remove-CodexTopLevelValues -Content $Content -Keys @(
        'base_url',
        'wire_api',
        'experimental_bearer_token',
        'openai_base_url',
        'service_tier',
        'disable_response_storage'
    )
    $text = Set-CodexTopLevelValue -Content $text -Key 'model_provider' -Value '"codex_router"'
    $text = Set-CodexTopLevelValue -Content $text -Key 'model' -Value ('"' + (ConvertTo-CodexTomlString $Model) + '"')
    $text = Set-CodexTopLevelValue -Content $text -Key 'model_catalog_json' -Value ('"' + (ConvertTo-CodexTomlString $CatalogPath) + '"')
    $text = Set-CodexTopLevelValue -Content $text -Key 'model_reasoning_effort' -Value ('"' + $ReasoningEffort + '"')
    # Antigravity Claude rejects the provider-side Apps tool schemas. Keep the
    # local coding tools, MCP, shell, files, and multi-agent features available.
    $text = Set-CodexTableValue -Content $text -Table 'features' -Key 'apps' -Value 'false'
    $text = Set-CodexTableValue -Content $text -Table 'models.new_thread' -Key 'model' -Value ('"' + (ConvertTo-CodexTomlString $Model) + '"')
    $text = Set-CodexTableValue -Content $text -Table 'models.new_thread' -Key 'model_reasoning_effort' -Value ('"' + $ReasoningEffort + '"')
    if ($FastMode) {
        $text = Set-CodexTopLevelValue -Content $text -Key 'service_tier' -Value '"fast"'
        $text = Set-CodexTableValue -Content $text -Table 'models.new_thread' -Key 'service_tier' -Value '"fast"'
        $text = Set-CodexTableValue -Content $text -Table 'features' -Key 'fast_mode' -Value 'true'
    } else {
        $text = Remove-CodexTableValue -Content $text -Table 'models.new_thread' -Key 'service_tier'
        $text = Set-CodexTableValue -Content $text -Table 'features' -Key 'fast_mode' -Value 'false'
    }
    $permissionSource = if ($null -eq $PermissionSourceContent) { $Content } else { $PermissionSourceContent }
    $text = Copy-CodexPermissionSettings -Content $text -SourceContent $permissionSource
    $text = Set-CodexTableValue -Content $text -Table 'desktop' -Key 'enabled-reasoning-efforts' -Value '["low", "medium", "high", "xhigh", "ultra", "max"]'
    $text = Remove-LegacyCodexLoopbackProxyProvider -Content $text
    $text = Remove-LegacyCodexRouterProvider -Content $text
    # Own a dedicated provider id so third-party tools that rewrite
    # model_providers.custom (Chiral/micu, other switchers) cannot steal the
    # active Codex route away from the local Router.
    foreach ($routerOwned in @('model_providers.codex_router', 'model_providers.sub2api')) {
        $text = Remove-CodexTable -Content $text -Table $routerOwned
    }
    # Deduplicate accidental [model_providers.custom] blocks left by external
    # tools that clobber the active provider id onto custom.
    $customMatches = [Text.RegularExpressions.Regex]::Matches(
        $text,
        '(?ms)^\[model_providers\.custom\]\s*\r?\n.*?(?=^\[|\z)')
    if ($customMatches.Count -gt 1) {
        $keep = $customMatches[$customMatches.Count - 1].Value
        $text = [Text.RegularExpressions.Regex]::Replace(
            $text,
            '(?ms)^\[model_providers\.custom\]\s*\r?\n.*?(?=^\[|\z)',
            '')
        $text = $text.TrimEnd() + "`r`n`r`n" + $keep.TrimEnd() + "`r`n"
    }

    $localBearer = ConvertTo-CodexTomlString $LocalApiKey
    $requiresOpenAiAuthLiteral = if ([bool]$RequireOpenAiAuth) { 'true' } else { 'false' }
    $providerBlock = @"
[model_providers.codex_router]
name = "Codex-Router"
base_url = "$base/v1"
wire_api = "responses"
requires_openai_auth = $requiresOpenAiAuthLiteral
experimental_bearer_token = "$localBearer"
request_max_retries = 2
stream_max_retries = 2
stream_idle_timeout_ms = 300000
supports_websockets = false
"@
    return $text.TrimEnd() + "`r`n`r`n" + $providerBlock.Trim() + "`r`n"
}

function Set-CodexUserEnvironmentVariable {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Value
    )

    [Environment]::SetEnvironmentVariable($Name, $Value, 'User')
    [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
    if (-not ('CodexRouter.EnvironmentBroadcast' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace CodexRouter {
    public static class EnvironmentBroadcast {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr SendMessageTimeout(
            IntPtr hWnd, uint msg, UIntPtr wParam, string lParam,
            uint flags, uint timeout, out UIntPtr result);

        public static void Notify() {
            UIntPtr result;
            SendMessageTimeout(new IntPtr(0xffff), 0x001A, UIntPtr.Zero,
                "Environment", 0x0002, 5000, out result);
        }
    }
}
'@
    }
    [CodexRouter.EnvironmentBroadcast]::Notify()
}

function Limit-CodexRouterBackups {
    param(
        [Parameter(Mandatory = $true)][string]$Directory,
        [Parameter(Mandatory = $true)][string]$Filter,
        [ValidateRange(1, 20)][int]$Keep = 3
    )

    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) { return }
    $backups = @(Get-ChildItem -LiteralPath $Directory -Filter $Filter -File |
        Sort-Object LastWriteTimeUtc, Name -Descending)
    foreach ($backup in @($backups | Select-Object -Skip $Keep)) {
        Remove-Item -LiteralPath $backup.FullName -Force
    }
}

Export-ModuleMember -Function ConvertTo-CodexTomlString, Get-CodexRouterDefaultModel, Get-CodexRouterModelDefaults, Get-CodexRouterRequiresOpenAiAuth, Test-CodexHomeHasChatGptAuth, Get-CodexPermissionSourceContent, New-CodexRouterConfig, Set-CodexUserEnvironmentVariable, Limit-CodexRouterBackups
