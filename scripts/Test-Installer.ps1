param(
    [string]$ArchivePath,
    [string]$RouterExePath,
    [string]$InstallerPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$cargoToml = Get-Content -LiteralPath (Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml') -Raw
$versionMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"\s*$')
if (-not $versionMatch.Success) { throw 'Could not read the Codex-Router version from Cargo.toml.' }
$version = $versionMatch.Groups[1].Value
$installerBuilder = Get-Content -LiteralPath (Join-Path $routerRoot 'scripts\Build-Installer.ps1') -Raw
foreach ($quietCommand in @(
    'AdminQuietInstCmd=%AppLaunched%',
    'UserQuietInstCmd=%AppLaunched%'
)) {
    $matches = [regex]::Matches(
        $installerBuilder,
        "(?m)^$([regex]::Escape($quietCommand))\r?$"
    )
    if ($matches.Count -ne 1) {
        throw "Installer builder must define exactly one silent command: $quietCommand"
    }
}
if (-not $installerBuilder.Contains('start "" /wait "%~dp0Codex-Router-Setup.exe"')) {
    throw 'Installer builder must synchronously wait for the extracted native setup process.'
}
if (-not $installerBuilder.Contains('--installer-wizard')) {
    throw 'Installer builder must launch the interactive installer wizard instead of installing immediately.'
}
$nativeMain = @(
    (Join-Path $routerRoot 'codex-router-gui-rust\src\main.rs'),
    (Join-Path $routerRoot 'codex-router-gui-rust\src\windows_main.rs')
) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | ForEach-Object {
    Get-Content -LiteralPath $_ -Raw
}
$nativeMain = $nativeMain -join "`n"
if (-not $nativeMain.Contains('installer_wizard')) {
    throw 'Native installer must expose the interactive installer wizard entry point.'
}
$nativeUpdater = Get-Content -LiteralPath (Join-Path $routerRoot 'codex-router-gui-rust\src\updater.rs') -Raw
if (-not $nativeUpdater.Contains('create_desktop_shortcut')) {
    throw 'Installer must create a desktop shortcut when the user selects that option.'
}
if ([string]::IsNullOrWhiteSpace($RouterExePath)) {
    $RouterExePath = Join-Path $routerRoot 'codex-router-gui-rust\target\release\codex-router.exe'
}
$RouterExePath = [IO.Path]::GetFullPath($RouterExePath)
if (-not (Test-Path -LiteralPath $RouterExePath -PathType Leaf)) {
    throw "Native installer executable is missing: $RouterExePath"
}

function Invoke-NativeInstaller {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $RouterExePath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    foreach ($argument in $Arguments) {
        [void]$startInfo.ArgumentList.Add($argument)
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    if (-not $process.WaitForExit(120000)) {
        $process.Kill()
        throw 'Native installer process exceeded its two-minute test budget.'
    }
    if ($process.ExitCode -ne 0) {
        throw "Native installer process failed with exit code $($process.ExitCode)."
    }
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-installer-test-' + [Guid]::NewGuid().ToString('N'))
$payload = Join-Path $testRoot "Codex-Router-Portable-$version-windows-x64"
$archive = "$payload.zip"
$installRoot = Join-Path $testRoot 'installed'

try {
    [IO.Directory]::CreateDirectory((Join-Path $payload 'app')) | Out-Null
    [IO.File]::WriteAllText((Join-Path $payload 'Codex-Router.exe'), 'router-test')
    [IO.File]::WriteAllText((Join-Path $payload 'app\cli-proxy-api.exe'), 'cli-proxy-api-test')
    $manifest = @('Codex-Router.exe', 'app/cli-proxy-api.exe') | ForEach-Object {
        $manifestPath = Join-Path $payload ($_ -replace '/', '\')
        [ordered]@{
            path = $_
            bytes = (Get-Item -LiteralPath $manifestPath).Length
            sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    [IO.File]::WriteAllText(
        (Join-Path $payload 'release-manifest.json'),
        ($manifest | ConvertTo-Json -Depth 4),
        [Text.UTF8Encoding]::new($false)
    )
    Compress-Archive -LiteralPath $payload -DestinationPath $archive -CompressionLevel Fastest

    $cliOutput = Join-Path $testRoot 'install-result.json'
    $env:CODEX_ROUTER_CLI_OUTPUT = $cliOutput
    Invoke-NativeInstaller -Arguments @(
        '--install-portable',
        "--install-package=$archive",
        "--install-root=$installRoot",
        "--install-version=$version",
        '--no-shortcut'
    )
    $result = Get-Content -LiteralPath $cliOutput -Raw | ConvertFrom-Json
    if (-not [bool]$result.installed -or [string]$result.version -ne $version) {
        throw 'Installer returned invalid completion metadata.'
    }
    foreach ($relativePath in @('Codex-Router.exe', 'app\cli-proxy-api.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot $relativePath) -PathType Leaf)) {
            throw "Installer did not copy required payload: $relativePath"
        }
    }
    $reportedInstallRoot = [IO.Path]::GetFullPath([string]$result.installRoot).TrimEnd([char[]]@('\', '/'))
    $expectedInstallRoot = [IO.Path]::GetFullPath($installRoot).TrimEnd([char[]]@('\', '/'))
    if (-not [string]::Equals($reportedInstallRoot, $expectedInstallRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Installer reported the wrong install root: expected '$expectedInstallRoot', received '$reportedInstallRoot'."
    }

    'Installer tests passed.'
} finally {
    Remove-Item Env:CODEX_ROUTER_CLI_OUTPUT -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if (-not [string]::IsNullOrWhiteSpace($ArchivePath)) {
    $archivePath = [IO.Path]::GetFullPath($ArchivePath)
    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
        throw "Real installer test archive is missing: $archivePath"
    }
    $realTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-real-installer-test-' + [Guid]::NewGuid().ToString('N'))
    $realInstallRoot = Join-Path $realTestRoot 'installed'
    try {
        [IO.Directory]::CreateDirectory($realTestRoot) | Out-Null
        $cliOutput = Join-Path $realTestRoot 'install-result.json'
        $env:CODEX_ROUTER_CLI_OUTPUT = $cliOutput
        Invoke-NativeInstaller -Arguments @(
            '--install-portable',
            "--install-package=$archivePath",
            "--install-root=$realInstallRoot",
            "--install-version=$version",
            '--no-shortcut'
        )
        $result = Get-Content -LiteralPath $cliOutput -Raw | ConvertFrom-Json
        $exe = Get-Item -LiteralPath (Join-Path $realInstallRoot 'Codex-Router.exe')
        if (-not [bool]$result.installed -or [string]$result.version -ne $version -or
            [string]$exe.VersionInfo.ProductVersion -ne $version -or
            [string]$exe.VersionInfo.CompanyName -ne 'Hernan_JIANG') {
            throw 'Real installer returned invalid version or publisher metadata.'
        }
        foreach ($relativePath in @('Start-Codex-Router.cmd', 'app\cli-proxy-api.exe')) {
            if (-not (Test-Path -LiteralPath (Join-Path $realInstallRoot $relativePath) -PathType Leaf)) {
                throw "Real installer did not copy required payload: $relativePath"
            }
        }
        $runtimePowerShell = @(Get-ChildItem -LiteralPath $realInstallRoot -Recurse -File -ErrorAction Stop |
                Where-Object { $_.Extension -in @('.ps1', '.psm1', '.psd1') })
        if ($runtimePowerShell.Count -gt 0) {
            throw "PowerShell runtime files entered the installed payload: $($runtimePowerShell[0].FullName)"
        }
        'Real release archive installation test passed.'
    } finally {
        Remove-Item Env:CODEX_ROUTER_CLI_OUTPUT -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $realTestRoot) {
            Remove-Item -LiteralPath $realTestRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

if (-not [string]::IsNullOrWhiteSpace($InstallerPath)) {
    $installerPath = [IO.Path]::GetFullPath($InstallerPath)
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
        throw "Installer package is missing: $installerPath"
    }
    $versionInfo = (Get-Item -LiteralPath $installerPath).VersionInfo
    if ([string]$versionInfo.ProductVersion -ne $version -or
        [string]$versionInfo.FileVersion -ne $version -or
        [string]$versionInfo.CompanyName -ne 'Hernan_JIANG' -or
        [string]$versionInfo.ProductName -ne 'CodexRouter') {
        throw 'Installer package exposes incorrect Windows version or publisher metadata.'
    }
    $extractRoot = Join-Path ([IO.Path]::GetTempPath()) (
        'codex-router-installer-extract-test-' + [Guid]::NewGuid().ToString('N'))
    try {
        [IO.Directory]::CreateDirectory($extractRoot) | Out-Null
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $installerPath
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        foreach ($argument in @('/Q', "/T:$extractRoot", '/C')) {
            [void]$startInfo.ArgumentList.Add($argument)
        }
        $process = [Diagnostics.Process]::Start($startInfo)
        if (-not $process.WaitForExit(120000)) {
            $process.Kill()
            throw 'Installer package extraction exceeded its two-minute test budget.'
        }
        if ($process.ExitCode -ne 0) {
            throw "Installer package extraction failed with exit code $($process.ExitCode)."
        }
        foreach ($name in @(
            "Codex-Router-Portable-$version-windows-x64.zip",
            'Codex-Router-Setup.exe',
            'Install-CodexRouter.cmd',
            'VCRUNTIME140.dll',
            'VCRUNTIME140_1.dll',
            'MSVCP140.dll'
        )) {
            if (-not (Test-Path -LiteralPath (Join-Path $extractRoot $name) -PathType Leaf)) {
                throw "Installer package extraction omitted required payload: $name"
            }
        }
    } finally {
        $resolvedExtractRoot = [IO.Path]::GetFullPath($extractRoot)
        $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        if ($resolvedExtractRoot.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
            (Split-Path -Leaf $resolvedExtractRoot).StartsWith(
                'codex-router-installer-extract-test-',
                [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedExtractRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    'Installer package metadata test passed.'
}
