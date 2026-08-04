param(
    [Parameter(Mandatory = $true)][string]$RouterRoot,
    [string]$CodexHome,
    [string]$CcSwitchDb,
    [string]$ProviderId,
    [string]$BackupDir,
    [string]$PythonExecutable = 'python'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRootPath = [IO.Path]::GetFullPath($RouterRoot)
$routerConfigPath = Join-Path $routerRootPath 'codex-router-config.json'
if (-not (Test-Path -LiteralPath $routerConfigPath -PathType Leaf)) {
    throw "Router configuration not found: $routerConfigPath"
}
$routerConfig = Get-Content -LiteralPath $routerConfigPath -Raw | ConvertFrom-Json

if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    $CodexHome = if (-not [string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
        $env:CODEX_HOME
    } else {
        Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex'
    }
}
$CodexHome = [IO.Path]::GetFullPath($CodexHome)
$codexConfig = Join-Path $CodexHome 'config.toml'
$codexAuth = Join-Path $CodexHome 'auth.json'
foreach ($required in @($codexConfig, $codexAuth)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required Codex state is missing: $required"
    }
}

if ([string]::IsNullOrWhiteSpace($CcSwitchDb)) {
    $CcSwitchDb = [string]$routerConfig.deploy.ccSwitchDb
}
if ([string]::IsNullOrWhiteSpace($ProviderId)) {
    $ProviderId = [string]$routerConfig.deploy.ccSwitchProfileId
}
if ([string]::IsNullOrWhiteSpace($CcSwitchDb) -or
    [string]::IsNullOrWhiteSpace($ProviderId)) {
    throw 'CC Switch database and Router provider ID are required.'
}
$CcSwitchDb = [IO.Path]::GetFullPath($CcSwitchDb)
$ccSettings = Join-Path (Split-Path -Parent $CcSwitchDb) 'settings.json'
if (-not (Test-Path -LiteralPath $CcSwitchDb -PathType Leaf) -or
    -not (Test-Path -LiteralPath $ccSettings -PathType Leaf)) {
    throw 'CC Switch database or settings.json was not found.'
}
if ([string]::IsNullOrWhiteSpace($BackupDir)) {
    $BackupDir = Join-Path $routerRootPath 'backups\cc-switch'
}
$BackupDir = [IO.Path]::GetFullPath($BackupDir)

$currentBefore = (Get-Content -LiteralPath $ccSettings -Raw | ConvertFrom-Json).currentProviderCodex
if ([string]$currentBefore -eq $ProviderId) {
    throw 'The requested CC Switch Router provider is active; offline update was aborted.'
}
$configHashBefore = (Get-FileHash -LiteralPath $codexConfig -Algorithm SHA256).Hash
$authHashBefore = (Get-FileHash -LiteralPath $codexAuth -Algorithm SHA256).Hash
$settingsHashBefore = (Get-FileHash -LiteralPath $ccSettings -Algorithm SHA256).Hash

Import-Module (Join-Path $routerRootPath 'scripts\CredentialStore.psm1') -Force
Import-Module (Join-Path $routerRootPath 'scripts\CodexIntegration.psm1') -Force
$catalogPath = Join-Path $routerRootPath 'config\model-catalog.json'
& (Join-Path $routerRootPath 'scripts\Build-ModelCatalog.ps1') `
    -ConfigPath $routerConfigPath `
    -OutputPath $catalogPath | Out-Null

$tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
$tempRoot = Join-Path $tempBase ('codex-router-offline-sync-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($tempRoot) | Out-Null
$tempConfig = Join-Path $tempRoot 'router-config.toml'
$localKey = $null
try {
    $localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
    if ([string]::IsNullOrWhiteSpace($localKey)) {
        throw 'The local Router credential is missing.'
    }
    $baseUrl = if ($routerConfig.deploy.sub2apiHost) {
        [string]$routerConfig.deploy.sub2apiHost
    } else {
        'http://127.0.0.1:18080'
    }
    $model = Get-CodexRouterDefaultModel -RouterConfig $routerConfig
    $defaults = Get-CodexRouterModelDefaults `
        -RouterConfig $routerConfig `
        -CatalogPath $catalogPath
    $generated = New-CodexRouterConfig `
        -Content ([IO.File]::ReadAllText($codexConfig)) `
        -Model $model `
        -CatalogPath ([IO.Path]::GetFullPath($catalogPath)) `
        -LocalApiKey $localKey `
        -BaseUrl $baseUrl `
        -ReasoningEffort $defaults.ReasoningEffort `
        -FastMode $defaults.FastMode
    Write-RouterTextFileAtomic -Path $tempConfig -Text $generated

    $python = (Get-Command $PythonExecutable -ErrorAction Stop).Source
    & $python (Join-Path $PSScriptRoot 'Sync-CCSwitchConfig.py') `
        --db $CcSwitchDb `
        --provider-id $ProviderId `
        --config $tempConfig `
        --auth $codexAuth `
        --backup-dir $BackupDir `
        --settings $ccSettings `
        --base-url $baseUrl `
        --require-inactive
    if ($LASTEXITCODE -ne 0) {
        throw "Offline CC Switch sync failed with exit code $LASTEXITCODE"
    }
} finally {
    $localKey = $null
    if (Test-Path -LiteralPath $tempRoot) {
        $resolvedTemp = [IO.Path]::GetFullPath($tempRoot)
        if (-not $resolvedTemp.StartsWith(
                $tempBase + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Temporary cleanup target escaped the system temp directory.'
        }
        Remove-Item -LiteralPath $resolvedTemp -Recurse -Force
    }
}

$currentAfter = (Get-Content -LiteralPath $ccSettings -Raw | ConvertFrom-Json).currentProviderCodex
$result = [ordered]@{
    providerId = $ProviderId
    currentProvider = [string]$currentAfter
    currentProviderUnchanged = ([string]$currentBefore -eq [string]$currentAfter)
    codexConfigUnchanged = ($configHashBefore -eq (Get-FileHash -LiteralPath $codexConfig -Algorithm SHA256).Hash)
    codexAuthUnchanged = ($authHashBefore -eq (Get-FileHash -LiteralPath $codexAuth -Algorithm SHA256).Hash)
    ccSettingsBytesChanged = ($settingsHashBefore -ne (Get-FileHash -LiteralPath $ccSettings -Algorithm SHA256).Hash)
}
if (-not $result.currentProviderUnchanged -or
    -not $result.codexConfigUnchanged -or
    -not $result.codexAuthUnchanged) {
    throw 'Offline sync modified the active Codex state unexpectedly.'
}
$result | ConvertTo-Json -Compress
