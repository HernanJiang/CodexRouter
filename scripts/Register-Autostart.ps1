Set-StrictMode -Version Latest
$routerRoot = Split-Path -Parent $PSScriptRoot
$startup = [Environment]::GetFolderPath('Startup')
$shortcutPath = Join-Path $startup 'Codex Router.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$shortcut.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$routerRoot\scripts\Start-Router.ps1`""
$shortcut.WorkingDirectory = $routerRoot
$shortcut.IconLocation = "$routerRoot\app\sub2api.exe,0"
$shortcut.Save()
Write-Output "Autostart registered: $shortcutPath"
