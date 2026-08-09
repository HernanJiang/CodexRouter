Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$installerScript = Join-Path $PSScriptRoot 'Install-CodexRouter.ps1'
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-installer-test-' + [Guid]::NewGuid().ToString('N'))
$payload = Join-Path $testRoot 'Codex-Router-Portable-1.5.4-windows-x64'
$archive = "$payload.zip"
$installRoot = Join-Path $testRoot 'installed'

try {
    [IO.Directory]::CreateDirectory((Join-Path $payload 'app')) | Out-Null
    [IO.File]::WriteAllText((Join-Path $payload 'Codex-Router.exe'), 'router-test')
    [IO.File]::WriteAllText((Join-Path $payload 'app\sub2api.exe'), 'sub2api-test')
    Compress-Archive -LiteralPath $payload -DestinationPath $archive -CompressionLevel Fastest

    $output = & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $installerScript `
        -PackageZip $archive -InstallRoot $installRoot -NoShortcut
    if ($LASTEXITCODE -ne 0) { throw "Installer process failed with exit code $LASTEXITCODE." }

   $result = $output | ConvertFrom-Json
    if (-not [bool]$result.installed -or [string]$result.version -ne '1.5.4') {
        throw 'Installer returned invalid completion metadata.'
    }
    foreach ($relativePath in @('Codex-Router.exe', 'app\sub2api.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $installRoot $relativePath) -PathType Leaf)) {
            throw "Installer did not copy required payload: $relativePath"
        }
    }
    if ([string]$result.installRoot -ne [IO.Path]::GetFullPath($installRoot)) {
        throw 'Installer reported the wrong install root.'
    }

    'Installer tests passed.'
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
