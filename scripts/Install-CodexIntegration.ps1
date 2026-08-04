param([string]$CodexHome)

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
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CodexIntegration.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$configLock = Enter-RouterConfigLock -RouterRoot $routerRoot -TimeoutMilliseconds 10000
$previousLockMarker = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', 'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_CONFIG_LOCK_HELD', '1', 'Process')
$localKey = $null
try {
$routerConfigPath = Get-RouterConfigPath -RouterRoot $routerRoot
$routerConfig = if (Test-Path -LiteralPath $routerConfigPath) {
    Get-Content -LiteralPath $routerConfigPath -Raw | ConvertFrom-Json
} else {
    $null
}

$catalogPath = Join-Path $routerRoot 'config\model-catalog.json'
if ($null -ne $routerConfig) {
    & "$routerRoot\scripts\Build-ModelCatalog.ps1" `
        -CodexHome $CodexHome `
        -ConfigPath $routerConfigPath `
        -OutputPath $catalogPath
}

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
$permissionSource = Get-CodexPermissionSourceContent -CodexConfigPath $codexConfig -Content $text
$model = Get-CodexRouterDefaultModel -RouterConfig $routerConfig
$modelDefaults = Get-CodexRouterModelDefaults -RouterConfig $routerConfig -CatalogPath $catalogPath
$localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
if ([string]::IsNullOrWhiteSpace($localKey)) {
    throw 'The local Router credential is missing. Run Codex-Router.exe before installing the Codex integration.'
}
$sub2apiHost = if ($null -ne $routerConfig -and $routerConfig.deploy.sub2apiHost) {
    [string]$routerConfig.deploy.sub2apiHost
} else {
    'http://127.0.0.1:18080'
}
$text = New-CodexRouterConfig `
    -Content $text `
    -Model $model `
    -CatalogPath $catalogPath `
    -LocalApiKey $localKey `
    -BaseUrl $sub2apiHost `
    -ReasoningEffort $modelDefaults.ReasoningEffort `
    -FastMode $modelDefaults.FastMode `
    -PermissionSourceContent $permissionSource

if ($text -notmatch '(?m)^model_provider\s*=\s*"custom"\s*$' -or
    $text -notmatch ('(?m)^model\s*=\s*"' + [Text.RegularExpressions.Regex]::Escape($model) + '"\s*$') -or
    $text -notmatch '(?m)^requires_openai_auth\s*=\s*(?:true|false)\s*$' -or
    $text -notmatch '(?m)^experimental_bearer_token\s*=\s*".+"\s*$' -or
    $text -notmatch '(?m)^supports_websockets\s*=\s*false\s*$') {
    throw 'Generated Codex configuration is missing required router defaults'
}

$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$backupPath = "$codexConfig.codex-router-$timestamp.bak"
if ($configExisted) {
    Write-RouterFileAtomic -Path $backupPath -Bytes ([IO.File]::ReadAllBytes($codexConfig))
    Limit-CodexRouterBackups `
        -Directory (Split-Path -Parent $codexConfig) `
        -Filter 'config.toml.codex-router-*.bak' `
        -Keep 3
}
Write-RouterTextFileAtomic -Path $codexConfig -Text $text

try {
    Set-CodexUserEnvironmentVariable -Name 'CODEX_ROUTER_API_KEY' -Value $localKey
} finally {
    $localKey = $null
}

if ($configExisted) {
    Write-Output "Codex configuration installed in $CodexHome; backup: $backupPath"
} else {
    Write-Output "Codex configuration created in $CodexHome"
}
} finally {
    $localKey = $null
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_CONFIG_LOCK_HELD',
        $previousLockMarker,
        'Process')
    Exit-RouterConfigLock -Lock $configLock
}
