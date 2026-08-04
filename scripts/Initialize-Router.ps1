Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
$userDataRoot = Get-RouterUserDataRoot -RouterRoot $routerRoot
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot

foreach ($directory in @($dataRoot, (Join-Path $dataRoot 'pids'), (Join-Path $dataRoot 'redis'), (Join-Path $dataRoot 'sub2api'), (Join-Path $routerRoot 'logs'))) {
    [IO.Directory]::CreateDirectory($directory) | Out-Null
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
    $generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $generator.GetBytes($buffer)
        return ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant()
    } finally {
        $generator.Dispose()
        [Array]::Clear($buffer, 0, $buffer.Length)
    }
}

$secretSpecs = @{
    'PostgresPassword' = 24
    'RedisPassword' = 24
    'JwtSecret' = 32
    'TotpEncryptionKey' = 32
}

foreach ($entry in $secretSpecs.GetEnumerator()) {
    if ($null -eq (Get-RouterCredential -Name $entry.Key -AllowMissing)) {
        Set-RouterCredential -Name $entry.Key -Secret (New-RandomHex -Bytes $entry.Value)
    }
}

if ($null -eq (Get-RouterCredential -Name 'AdminPassword' -AllowMissing)) {
    Set-RouterCredential -Name 'AdminPassword' -Secret (New-RandomHex -Bytes 24)
}

$pgData = Join-Path $dataRoot 'postgres'
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

if (-not (Test-Path -LiteralPath (Join-Path $dataRoot 'redis'))) {
    New-Item -ItemType Directory -Path (Join-Path $dataRoot 'redis') | Out-Null
}

$aclMarker = Join-Path $dataRoot '.acl-protected-v2'
if (-not (Test-Path -LiteralPath $aclMarker)) {
    if (Test-RouterPathAclSupport -Path $userDataRoot) {
        # The autostart shortcut executes scripts from this directory. Protect
        # the package root against replacement by another local account.
        if (Test-RouterPathAclSupport -Path $routerRoot) {
            Protect-RouterPathAcl -Path $routerRoot
        }
        foreach ($resolved in @($dataRoot, (Join-Path $userDataRoot 'backups'), (Join-Path $routerRoot 'logs'))) {
            if (Test-Path -LiteralPath $resolved) {
                Protect-RouterPathAcl -Path $resolved -Recurse
            }
        }
        $markerBytes = [Text.Encoding]::ASCII.GetBytes('current-user-only')
        try { Write-RouterFileAtomic -Path $aclMarker -Bytes $markerBytes }
        finally { [Array]::Clear($markerBytes, 0, $markerBytes.Length) }
    } else {
        Write-Warning 'ROUTER_ACL_UNSUPPORTED: The user-data drive does not support Windows ACLs. Credentials remain protected by Windows Credential Manager/DPAPI, but local database files cannot be restricted to the current user.'
    }
}

Write-Output 'Codex Router secrets and PostgreSQL data directory are initialized.'
