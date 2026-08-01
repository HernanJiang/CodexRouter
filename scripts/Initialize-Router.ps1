Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force

foreach ($directory in @('data', 'data\pids', 'data\redis', 'data\sub2api', 'logs')) {
    [IO.Directory]::CreateDirectory((Join-Path $routerRoot $directory)) | Out-Null
}
foreach ($requiredFile in @(
    'app\sub2api.exe',
    'postgres\pgsql\bin\initdb.exe',
    'redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe'
)) {
    if (-not (Test-Path -LiteralPath (Join-Path $routerRoot $requiredFile))) {
        throw "Portable runtime is incomplete; missing: $requiredFile"
    }
}

function New-RandomHex([int]$Bytes) {
    $buffer = [byte[]]::new($Bytes)
    [Security.Cryptography.RandomNumberGenerator]::Fill($buffer)
    try { return ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant() }
    finally { [Array]::Clear($buffer, 0, $buffer.Length) }
}

$secretSpecs = @{
    'PostgresPassword' = 24
    'RedisPassword' = 24
    'AdminPassword' = 18
    'JwtSecret' = 32
    'TotpEncryptionKey' = 32
}

foreach ($entry in $secretSpecs.GetEnumerator()) {
    if ($null -eq (Get-RouterCredential -Name $entry.Key -AllowMissing)) {
        Set-RouterCredential -Name $entry.Key -Secret (New-RandomHex -Bytes $entry.Value)
    }
}

$pgData = "$routerRoot\data\postgres"
$pgVersion = "$pgData\PG_VERSION"
if (-not (Test-Path -LiteralPath $pgVersion)) {
    if (-not (Test-Path -LiteralPath $pgData)) { New-Item -ItemType Directory -Path $pgData | Out-Null }
    $passwordFile = Join-Path $env:TEMP ("codex-router-pg-" + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        [IO.File]::WriteAllText($passwordFile, (Get-RouterCredential -Name 'PostgresPassword'), [Text.Encoding]::ASCII)
        & "$routerRoot\postgres\pgsql\bin\initdb.exe" `
            --pgdata=$pgData `
            --username=sub2api `
            --encoding=UTF8 `
            --locale=C `
            --auth-host=scram-sha-256 `
            --auth-local=scram-sha-256 `
            --pwfile=$passwordFile
        if ($LASTEXITCODE -ne 0) { throw "initdb failed with exit code $LASTEXITCODE" }
        [IO.File]::Copy((Join-Path $routerRoot 'config\pg_hba.conf'), (Join-Path $pgData 'pg_hba.conf'), $true)
    } finally {
        if (Test-Path -LiteralPath $passwordFile) { Remove-Item -LiteralPath $passwordFile -Force }
    }
}

if (-not (Test-Path -LiteralPath "$routerRoot\data\redis")) {
    New-Item -ItemType Directory -Path "$routerRoot\data\redis" | Out-Null
}

Write-Output 'Codex Router secrets and PostgreSQL data directory are initialized.'
