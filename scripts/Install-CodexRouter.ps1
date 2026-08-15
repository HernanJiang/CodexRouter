[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$SourceRoot,
    [string]$InstallRoot,
    [string]$PackageZip,
    [switch]$NoShortcut
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$version = '1.7.0'

if ([string]::IsNullOrWhiteSpace($SourceRoot)) {
    $SourceRoot = $PSScriptRoot
}
if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    $InstallRoot = Join-Path $env:LOCALAPPDATA "Programs\Codex-Router\$version"
}
$SourceRoot = [IO.Path]::GetFullPath($SourceRoot)
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)

if ([string]::IsNullOrWhiteSpace($PackageZip)) {
    $PackageZip = @(Get-ChildItem -LiteralPath $SourceRoot -Filter "Codex-Router-Portable-$version-windows-x64.zip" -File -ErrorAction SilentlyContinue |
            Select-Object -First 1 -ExpandProperty FullName)
}
if (-not [string]::IsNullOrWhiteSpace([string]$PackageZip) -and -not [IO.Path]::IsPathRooted($PackageZip)) {
    $PackageZip = Join-Path $SourceRoot $PackageZip
}

$temporaryExtract = $null
$payloadRoot = $SourceRoot
try {
    if (-not [string]::IsNullOrWhiteSpace([string]$PackageZip)) {
        $PackageZip = [IO.Path]::GetFullPath($PackageZip)
        if (-not (Test-Path -LiteralPath $PackageZip -PathType Leaf)) {
            throw "Installer payload is missing: $PackageZip"
        }
        $temporaryExtract = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-install-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($temporaryExtract) | Out-Null
        Expand-Archive -LiteralPath $PackageZip -DestinationPath $temporaryExtract -Force
        $payloadRoot = @(Get-ChildItem -LiteralPath $temporaryExtract -Directory | Select-Object -First 1 -ExpandProperty FullName)
        if ([string]::IsNullOrWhiteSpace([string]$payloadRoot)) { $payloadRoot = $temporaryExtract }
    }

    $routerExe = Join-Path $payloadRoot 'Codex-Router.exe'
    if (-not (Test-Path -LiteralPath $routerExe -PathType Leaf)) {
        throw "Codex-Router.exe is missing from the installer payload: $payloadRoot"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $payloadRoot 'app\sub2api.exe') -PathType Leaf)) {
        throw 'The installer payload is incomplete: app\sub2api.exe is missing.'
    }

    if ($PSCmdlet.ShouldProcess($InstallRoot, 'Install Codex-Router')) {
        [IO.Directory]::CreateDirectory($InstallRoot) | Out-Null
        Get-ChildItem -LiteralPath $payloadRoot -Force | ForEach-Object {
            Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $InstallRoot $_.Name) -Recurse -Force
        }

        if (-not $NoShortcut) {
            $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Codex-Router'
            [IO.Directory]::CreateDirectory($startMenu) | Out-Null
            $shortcutPath = Join-Path $startMenu 'Codex-Router.lnk'
            $shell = New-Object -ComObject WScript.Shell
            try {
                $shortcut = $shell.CreateShortcut($shortcutPath)
                $shortcut.TargetPath = Join-Path $InstallRoot 'Codex-Router.exe'
                $shortcut.WorkingDirectory = $InstallRoot
                $shortcut.Description = 'Codex-Router'
                $shortcut.IconLocation = "$(Join-Path $InstallRoot 'Codex-Router.exe'),0"
                $shortcut.Save()
            } finally {
                [Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell) | Out-Null
            }
        }
    }

    [ordered]@{
        installed = $true
        installRoot = $InstallRoot
        version = $version
        shortcut = (-not $NoShortcut)
        userData = (Join-Path $env:LOCALAPPDATA 'Codex-Router\UserData')
    } | ConvertTo-Json -Compress
} finally {
    if ($null -ne $temporaryExtract -and (Test-Path -LiteralPath $temporaryExtract)) {
        Remove-Item -LiteralPath $temporaryExtract -Recurse -Force -ErrorAction SilentlyContinue
    }
}
