Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$port = Get-NetTCPConnection -LocalPort 1455 -State Listen -ErrorAction SilentlyContinue
if ($port) { throw 'OAuth callback port 1455 is already in use.' }

$process = Start-Process `
    -FilePath "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
    -ArgumentList @(
        '-NoProfile',
        '-ExecutionPolicy',
        'Bypass',
        '-File',
        "$routerRoot\scripts\Start-ChatGPTOAuth.ps1",
        '-TimeoutSeconds',
        '600'
    ) `
    -WorkingDirectory $routerRoot `
    -WindowStyle Hidden `
    -RedirectStandardOutput "$routerRoot\logs\oauth-stdout.log" `
    -RedirectStandardError "$routerRoot\logs\oauth-stderr.log" `
    -PassThru

Set-Content -LiteralPath "$routerRoot\data\pids\oauth.pid" -Value $process.Id -Encoding ascii
[pscustomobject]@{
    Started = $true
    ProcessId = $process.Id
    Callback = 'http://localhost:1455/auth/callback'
} | Format-List
