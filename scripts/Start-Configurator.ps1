Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$port = 8080

$python = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $python) {
    $python = (Get-Command python3 -ErrorAction SilentlyContinue).Source
}
if (-not $python) {
    throw 'Python is required to start the static configurator. Please install Python and add it to PATH.'
}

Write-Host "Starting Codex Router Configurator at http://127.0.0.1:$port"
& $python -m http.server $port --directory "$routerRoot\configurator"
