Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot
$lifecycleLock = Enter-RouterLifecycleLock `
    -RouterRoot $routerRoot `
    -TimeoutMilliseconds 10000 `
    -Operation 'Launch ChatGPT OAuth'
$process = $null
try {
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
Set-Content -LiteralPath (Join-Path $dataRoot 'pids\oauth.pid') -Value $process.Id -Encoding ascii
} finally {
    Exit-RouterLifecycleLock -Lock $lifecycleLock
}

[pscustomobject]@{
    Started = $true
    ProcessId = $process.Id
    Callback = 'http://localhost:1455/auth/callback'
} | Format-List
