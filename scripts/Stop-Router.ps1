Set-StrictMode -Version Latest
$ErrorActionPreference = 'Continue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force

$sub2apiPidFile = "$routerRoot\data\pids\sub2api.pid"
if (Test-Path -LiteralPath $sub2apiPidFile) {
    $sub2apiPid = [int]([IO.File]::ReadAllText($sub2apiPidFile).Trim())
    $sub2apiProcess = Get-Process -Id $sub2apiPid -ErrorAction SilentlyContinue
    if ($null -ne $sub2apiProcess -and [string]::Equals($sub2apiProcess.Path, "$routerRoot\app\sub2api.exe", [StringComparison]::OrdinalIgnoreCase)) {
        Stop-Process -Id $sub2apiPid -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $sub2apiPidFile -Force -ErrorAction SilentlyContinue
}

$env:REDISCLI_AUTH = Get-RouterCredential -Name 'RedisPassword'
try {
    & "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-cli.exe" -h 127.0.0.1 -p 16379 shutdown save *> $null
} finally {
    Remove-Item Env:REDISCLI_AUTH -ErrorAction SilentlyContinue
}

$deadline = [DateTime]::UtcNow.AddSeconds(5)
do {
    $redisListener = Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort 16379 -State Listen -ErrorAction SilentlyContinue
    if ($null -eq $redisListener) { break }
    Start-Sleep -Milliseconds 200
} while ([DateTime]::UtcNow -lt $deadline)

if ($null -ne $redisListener) {
    $redisProcess = Get-Process -Id $redisListener.OwningProcess -ErrorAction SilentlyContinue
    $redisPath = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe"
    if ($null -eq $redisProcess -or -not [string]::Equals($redisProcess.Path, $redisPath, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing to stop an unexpected process listening on the Redis port.'
    }
    Stop-Process -Id $redisProcess.Id -Force
}
Remove-Item -LiteralPath "$routerRoot\data\pids\redis.pid" -Force -ErrorAction SilentlyContinue

& "$routerRoot\postgres\pgsql\bin\pg_ctl.exe" stop -D "$routerRoot\data\postgres" -s -m fast -w -t 60 *> $null
Write-Output 'Codex Router is stopped.'
