param(
    [Parameter(Mandatory)][string]$Stage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$stageRoot = [IO.Path]::GetFullPath($Stage).TrimEnd([char[]]@('\', '/'))
if (-not (Test-Path -LiteralPath $stageRoot -PathType Container)) {
    throw "Portable stage does not exist: $stageRoot"
}
$postgresBin = Join-Path $stageRoot 'postgres\pgsql\bin'
foreach ($name in @('initdb.exe', 'postgres.exe', 'pg_ctl.exe', 'pg_isready.exe', 'createdb.exe', 'psql.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $postgresBin $name) -PathType Leaf)) {
        throw "Portable PostgreSQL payload is incomplete; missing: $name"
    }
}
foreach ($relative in @('postgres\pgsql\share\locale', 'postgres\pgsql\bin\stackbuilder.exe')) {
    if (Test-Path -LiteralPath (Join-Path $stageRoot $relative)) {
        throw "Optional PostgreSQL payload was not trimmed: $relative"
    }
}
$remainingWxFiles = @(Get-ChildItem -LiteralPath $postgresBin -File -Force -Filter 'wx*.dll')
if ($remainingWxFiles.Count -gt 0) {
    throw "StackBuilder-only wxWidgets runtime was not trimmed: $($remainingWxFiles.Name -join ', ')"
}

$validationParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
$validationRoot = Join-Path $validationParent ('codex-router-postgres-validation-' + [Guid]::NewGuid().ToString('N'))
$dataRoot = Join-Path $validationRoot 'data'
$logPath = Join-Path $validationRoot 'postgres.log'
$passwordPath = Join-Path $validationRoot 'password.txt'
[IO.Directory]::CreateDirectory($validationRoot) | Out-Null

$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
try { $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port }
finally { $listener.Stop() }

$password = 'validation-' + [Guid]::NewGuid().ToString('N')
$started = $false
$success = $false
$result = $null
try {
    [IO.File]::WriteAllText($passwordPath, $password, [Text.Encoding]::ASCII)
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $initOutput = @(& (Join-Path $postgresBin 'initdb.exe') `
        --pgdata=$dataRoot `
        --username=sub2api `
        --encoding=UTF8 `
        --locale=C `
        --auth-host=scram-sha-256 `
        --auth-local=scram-sha-256 `
        --pwfile=$passwordPath 2>&1)
    $initExitCode = $LASTEXITCODE
    $timer.Stop()
    $initMilliseconds = $timer.ElapsedMilliseconds
    Remove-Item -LiteralPath $passwordPath -Force
    if ($initExitCode -ne 0) {
        throw "initdb failed with exit code ${initExitCode}: $($initOutput -join ' | ')"
    }

    $serverOptions = "-h 127.0.0.1 -p $port -c max_connections=10 -c shared_buffers=16MB -c logging_collector=off"
    $timer.Restart()
    & (Join-Path $postgresBin 'pg_ctl.exe') `
        start -D $dataRoot -s -w -t 60 -l $logPath -o $serverOptions
    if ($LASTEXITCODE -ne 0) { throw 'pg_ctl start failed.' }
    $started = $true
    $timer.Stop()
    $startMilliseconds = $timer.ElapsedMilliseconds

    & (Join-Path $postgresBin 'pg_isready.exe') `
        -h 127.0.0.1 -p $port -d postgres -U sub2api -t 5 *> $null
    if ($LASTEXITCODE -ne 0) { throw 'pg_isready failed.' }

    $env:PGPASSWORD = $password
    $env:PGCONNECT_TIMEOUT = '8'
    $timer.Restart()
    & (Join-Path $postgresBin 'createdb.exe') `
        -h 127.0.0.1 -p $port -U sub2api trim_validation
    if ($LASTEXITCODE -ne 0) { throw 'createdb failed.' }
    $queryOutput = @(& (Join-Path $postgresBin 'psql.exe') `
        -h 127.0.0.1 -p $port -U sub2api -d trim_validation `
        -v ON_ERROR_STOP=1 -Atc @'
CREATE EXTENSION IF NOT EXISTS pg_trgm;
SELECT extversion FROM pg_extension WHERE extname='pg_trgm';
SELECT similarity('codex-router', 'codex router') > 0.5;
SELECT current_database();
'@ 2>&1)
    $queryExitCode = $LASTEXITCODE
    $timer.Stop()
    $queryMilliseconds = $timer.ElapsedMilliseconds
    if ($queryExitCode -ne 0) {
        throw "psql failed with exit code ${queryExitCode}: $($queryOutput -join ' | ')"
    }
    if ('t' -notin $queryOutput -or 'trim_validation' -notin $queryOutput) {
        throw "PostgreSQL verification returned unexpected output: $($queryOutput -join ' | ')"
    }

    $timer.Restart()
    & (Join-Path $postgresBin 'pg_ctl.exe') stop -D $dataRoot -s -w -t 60 -m smart
    if ($LASTEXITCODE -ne 0) { throw 'pg_ctl smart stop failed.' }
    $started = $false
    $timer.Stop()
    $stopMilliseconds = $timer.ElapsedMilliseconds

    $postmasterPidRemoved = -not (Test-Path -LiteralPath (Join-Path $dataRoot 'postmaster.pid'))
    $probe = [Net.Sockets.TcpClient]::new()
    try {
        $connectTask = $probe.ConnectAsync('127.0.0.1', $port)
        $portClosed = -not ($connectTask.Wait(500) -and $probe.Connected)
    } catch {
        $portClosed = $true
    } finally {
        $probe.Dispose()
    }
    if (-not $postmasterPidRemoved -or -not $portClosed) {
        throw 'PostgreSQL shutdown verification failed.'
    }

    $result = [ordered]@{
        success = $true
        stage = $stageRoot
        port = $port
        initMilliseconds = $initMilliseconds
        startMilliseconds = $startMilliseconds
        queryMilliseconds = $queryMilliseconds
        smartStopMilliseconds = $stopMilliseconds
        queryOutput = @($queryOutput)
        postmasterPidRemoved = $postmasterPidRemoved
        portClosed = $portClosed
        optionalPayloadAbsent = $true
        validationArtifactsRemoved = $true
    }
    $success = $true
} finally {
    Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:PGCONNECT_TIMEOUT -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $passwordPath) {
        Remove-Item -LiteralPath $passwordPath -Force
    }
    if ($started) {
        & (Join-Path $postgresBin 'pg_ctl.exe') stop -D $dataRoot -s -w -t 30 -m fast 2>$null
    }
    if (Test-Path -LiteralPath $validationRoot) {
        $resolvedValidationRoot = [IO.Path]::GetFullPath($validationRoot)
        $expectedPrefix = $validationParent + [IO.Path]::DirectorySeparatorChar
        if (-not $resolvedValidationRoot.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase) -or
            -not [IO.Path]::GetFileName($resolvedValidationRoot).StartsWith(
                'codex-router-postgres-validation-',
                [StringComparison]::Ordinal)) {
            throw "Refusing to clean an unexpected validation path: $resolvedValidationRoot"
        }
        Remove-Item -LiteralPath $resolvedValidationRoot -Recurse -Force
    }
}

if (-not $success -or $null -eq $result) { throw 'PostgreSQL portable payload validation failed.' }
$result | ConvertTo-Json -Depth 4
