Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$routerExe = Join-Path $routerRoot 'Codex-Router.exe'
if (-not (Test-Path -LiteralPath $routerExe -PathType Leaf)) {
    throw "Codex-Router.exe is missing from $routerRoot"
}

$pidDirectory = Join-Path (Get-RouterDataRoot -RouterRoot $routerRoot) 'pids'
[IO.Directory]::CreateDirectory($pidDirectory) | Out-Null
Remove-Item -LiteralPath (Join-Path $pidDirectory 'health-monitor.enabled') -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath (Join-Path $pidDirectory 'health-monitor.paused') -Force -ErrorAction SilentlyContinue

$stateDirectory = Join-Path $env:LOCALAPPDATA 'Codex-Router'
[IO.Directory]::CreateDirectory($stateDirectory) | Out-Null
[IO.File]::WriteAllText(
    (Join-Path $stateDirectory 'install-root.txt'),
    [IO.Path]::GetFullPath($routerRoot),
    [Text.UTF8Encoding]::new($false))

# Remove the legacy minute-by-minute PowerShell task during upgrades. The GUI
# now performs a native low-frequency probe while it resides in the tray.
$taskName = 'Codex Router Health Monitor'
try {
    $scheduler = New-Object -ComObject 'Schedule.Service'
    $scheduler.Connect()
    $scheduler.GetFolder('\').DeleteTask($taskName, 0)
} catch {
    if ($_.Exception.HResult -ne -2147024894) { throw }
}

$startup = [Environment]::GetFolderPath('Startup')
$shortcutPath = Join-Path $startup 'Codex Router.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $routerExe
$shortcut.Arguments = '--background'
$shortcut.WorkingDirectory = $routerRoot
$shortcut.IconLocation = "$routerExe,0"
# Keep Windows from briefly restoring a visible window during sign-in. The
# application also starts with an invisible viewport and lives in the tray.
$shortcut.WindowStyle = 7
$shortcut.Save()

Write-Output "Autostart registered: $shortcutPath"
Write-Output 'The Codex-Router GUI will start directly in lightweight tray mode at the next sign-in.'
