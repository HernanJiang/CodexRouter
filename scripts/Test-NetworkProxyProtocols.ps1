Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$python = Get-Command python -ErrorAction SilentlyContinue
$previousBytecodeSetting = [Environment]::GetEnvironmentVariable('PYTHONDONTWRITEBYTECODE', 'Process')
[Environment]::SetEnvironmentVariable('PYTHONDONTWRITEBYTECODE', '1', 'Process')
try {
    if ($null -eq $python) {
        $launcher = Get-Command py -ErrorAction SilentlyContinue
        if ($null -eq $launcher) {
            throw 'Python 3.10 or newer is required to run the network proxy protocol tests.'
        }
        & $launcher.Source -3 (Join-Path $PSScriptRoot 'test_network_proxies.py')
    } else {
        & $python.Source (Join-Path $PSScriptRoot 'test_network_proxies.py')
    }
    $testExitCode = $LASTEXITCODE
} finally {
    [Environment]::SetEnvironmentVariable(
        'PYTHONDONTWRITEBYTECODE',
        $previousBytecodeSetting,
        'Process')
}

if ($testExitCode -ne 0) {
    throw "Network proxy protocol tests failed with exit code $testExitCode."
}

