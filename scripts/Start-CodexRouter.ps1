Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$app = Join-Path $routerRoot 'Codex-Router.exe'
if (-not (Test-Path -LiteralPath $app)) {
    throw "Codex-Router is missing: $app"
}
Start-Process -FilePath $app -WorkingDirectory $routerRoot
Write-Output "Started Codex-Router: $app"
