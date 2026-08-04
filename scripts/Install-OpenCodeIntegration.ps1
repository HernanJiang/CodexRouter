param(
    [string]$RouterConfigPath,
    [string]$OpenCodeConfigDir,
    [string]$BaseUrl = 'http://127.0.0.1:18080'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CodexIntegration.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$configLock = Enter-RouterConfigLock -RouterRoot $routerRoot -TimeoutMilliseconds 10000
$previousLockMarker = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', 'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', '1', 'Process')
try {
if ([string]::IsNullOrWhiteSpace($RouterConfigPath)) {
    $RouterConfigPath = Get-RouterConfigPath -RouterRoot $routerRoot
}
if (-not (Test-Path -LiteralPath $RouterConfigPath)) {
    throw "Router configuration not found: $RouterConfigPath"
}
if ([string]::IsNullOrWhiteSpace($OpenCodeConfigDir)) {
    $OpenCodeConfigDir = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.config\opencode'
}

$uri = $null
if (-not [Uri]::TryCreate($BaseUrl.TrimEnd('/'), [UriKind]::Absolute, [ref]$uri) -or
    $uri.Scheme -ne 'http' -or
    $uri.Host -notin @('127.0.0.1', 'localhost')) {
    throw 'OpenCode Router provider URL must be a local HTTP URL (127.0.0.1 or localhost).'
}
$routerBaseUrl = $uri.GetLeftPart([UriPartial]::Authority).TrimEnd('/') + '/v1'

$routerConfig = Get-Content -LiteralPath $RouterConfigPath -Raw | ConvertFrom-Json
$models = @($routerConfig.models)
if ($models.Count -eq 0) { throw 'At least one Router model is required for OpenCode integration.' }

$openCodeModels = [ordered]@{}
foreach ($model in $models) {
    $modelID = [string]$model.model
    if ([string]::IsNullOrWhiteSpace($modelID)) { continue }
    $aliasProperty = $model.PSObject.Properties['alias']
    $displayName = if ($null -ne $aliasProperty -and -not [string]::IsNullOrWhiteSpace([string]$aliasProperty.Value)) {
        [string]$aliasProperty.Value
    } else {
        $modelID
    }
    $openCodeModels[$modelID] = [ordered]@{ name = $displayName }
}
if ($openCodeModels.Count -eq 0) { throw 'Router models did not contain any usable model IDs.' }

function Set-OpenCodeProperty {
    param(
        [Parameter(Mandatory)][object]$Object,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][AllowNull()][object]$Value
    )
    $property = $Object.PSObject.Properties[$Name]
    if ($null -ne $property) {
        $property.Value = $Value
    } else {
        $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
    }
}

[IO.Directory]::CreateDirectory($OpenCodeConfigDir) | Out-Null
$openCodeConfigPath = Join-Path $OpenCodeConfigDir 'opencode.json'
$existingRaw = if (Test-Path -LiteralPath $openCodeConfigPath) {
    [IO.File]::ReadAllText($openCodeConfigPath)
} else {
    ''
}
$openCodeConfig = if ([string]::IsNullOrWhiteSpace($existingRaw)) {
    [pscustomobject][ordered]@{
        '$schema' = 'https://opencode.ai/config.json'
        provider = [pscustomobject][ordered]@{}
    }
} else {
    try {
        $existingRaw | ConvertFrom-Json
    } catch {
        throw "Existing OpenCode JSON is invalid; it was not overwritten: $openCodeConfigPath"
    }
}

$providerProperty = $openCodeConfig.PSObject.Properties['provider']
if ($null -eq $providerProperty -or $null -eq $providerProperty.Value) {
    $providers = [pscustomobject][ordered]@{}
    Set-OpenCodeProperty -Object $openCodeConfig -Name 'provider' -Value $providers
} else {
    $providers = $providerProperty.Value
}

$managedProvider = [pscustomobject][ordered]@{
    name = 'Codex-Router'
    npm = '@ai-sdk/openai-compatible'
    options = [pscustomobject][ordered]@{
        baseURL = $routerBaseUrl
        apiKey = '{env:CODEX_ROUTER_API_KEY}'
    }
    models = [pscustomobject]$openCodeModels
}
Set-OpenCodeProperty -Object $providers -Name 'codex-router' -Value $managedProvider

$nextText = $openCodeConfig | ConvertTo-Json -Depth 100
if ($existingRaw.TrimEnd() -eq $nextText.TrimEnd()) {
    Write-Output "OpenCode integration already current: $openCodeConfigPath"
    return
}

if (-not [string]::IsNullOrWhiteSpace($existingRaw)) {
    $backupPath = "$openCodeConfigPath.codex-router-$(Get-Date -Format 'yyyyMMdd-HHmmss-fff').bak"
    Write-RouterTextFileAtomic -Path $backupPath -Text $existingRaw
    Limit-CodexRouterBackups `
        -Directory (Split-Path -Parent $openCodeConfigPath) `
        -Filter 'opencode.json.codex-router-*.bak' `
        -Keep 3
}
Write-RouterTextFileAtomic `
    -Path $openCodeConfigPath `
    -Text ($nextText.TrimEnd() + [Environment]::NewLine)
Write-Output "OpenCode provider installed: codex-router / $routerBaseUrl / $($openCodeModels.Count) model(s)"
} finally {
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_CONFIG_LOCK_HELD',
        $previousLockMarker,
        'Process')
    Exit-RouterConfigLock -Lock $configLock
}
