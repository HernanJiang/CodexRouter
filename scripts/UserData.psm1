Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RouterUserDataRoot {
    param([Parameter(Mandatory)][string]$RouterRoot)

    $resolvedRoot = [IO.Path]::GetFullPath($RouterRoot)
    if ([Environment]::GetEnvironmentVariable('CODEX_ROUTER_PORTABLE_STATE', 'Process') -eq '1') {
        return $resolvedRoot
    }
    $override = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_USER_DATA_ROOT', 'Process')
    if (-not [string]::IsNullOrWhiteSpace($override)) {
        if (-not [IO.Path]::IsPathRooted($override)) {
            throw 'CODEX_ROUTER_USER_DATA_ROOT must be an absolute path.'
        }
        return [IO.Path]::GetFullPath($override)
    }
    if (-not (Test-Path -LiteralPath (Join-Path $resolvedRoot 'release-manifest.json') -PathType Leaf)) {
        return $resolvedRoot
    }
    $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if ([string]::IsNullOrWhiteSpace($localAppData)) { return $resolvedRoot }
    return Join-Path $localAppData 'Codex-Router\UserData'
}

function Get-RouterDataRoot {
    param([Parameter(Mandatory)][string]$RouterRoot)
    return Join-Path (Get-RouterUserDataRoot -RouterRoot $RouterRoot) 'data'
}

function Get-RouterConfigPath {
    param([Parameter(Mandatory)][string]$RouterRoot)
    return Join-Path (Get-RouterUserDataRoot -RouterRoot $RouterRoot) 'codex-router-config.json'
}

function Get-RouterBackupsRoot {
    param([Parameter(Mandatory)][string]$RouterRoot)
    return Join-Path (Get-RouterUserDataRoot -RouterRoot $RouterRoot) 'backups'
}

Export-ModuleMember -Function @(
    'Get-RouterUserDataRoot',
    'Get-RouterDataRoot',
    'Get-RouterConfigPath',
    'Get-RouterBackupsRoot'
)
