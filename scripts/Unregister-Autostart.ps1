Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$pidDirectory = Join-Path (Get-RouterDataRoot -RouterRoot $routerRoot) 'pids'
Remove-Item -LiteralPath (Join-Path $pidDirectory 'health-monitor.enabled') -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath (Join-Path $pidDirectory 'health-monitor.paused') -Force -ErrorAction SilentlyContinue

$installRootPath = Join-Path $env:LOCALAPPDATA 'Codex-Router\install-root.txt'
if (Test-Path -LiteralPath $installRootPath) {
    Remove-Item -LiteralPath $installRootPath -Force
}

$startup = [Environment]::GetFolderPath('Startup')
$shortcutPath = Join-Path $startup 'Codex Router.lnk'
if (Test-Path -LiteralPath $shortcutPath) {
    Remove-Item -LiteralPath $shortcutPath -Force
    Write-Output "Autostart removed: $shortcutPath"
} else {
    Write-Output 'Autostart was not registered.'
}

$taskName = 'Codex Router Health Monitor'
try {
    $scheduler = New-Object -ComObject 'Schedule.Service'
    $scheduler.Connect()
    $folder = $scheduler.GetFolder('\')
    $folder.DeleteTask($taskName, 0)
    Write-Output "Legacy health task removed: $taskName"
} catch {
    if ($_.Exception.HResult -ne -2147024894) { throw }
    Write-Output 'Legacy health task was not registered.'
}
Write-Output 'Background tray startup is disabled.'
