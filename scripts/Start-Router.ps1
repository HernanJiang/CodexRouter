Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force

function Wait-TcpPort([int]$Port, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $client = [Net.Sockets.TcpClient]::new()
        try {
            $task = $client.ConnectAsync('127.0.0.1', $Port)
            if ($task.Wait(500) -and $client.Connected) { return $true }
        } catch { } finally { $client.Dispose() }
        Start-Sleep -Milliseconds 300
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Set-RedisRequirePass([string]$Password) {
    $client = [Net.Sockets.TcpClient]::new()
    try {
        $connectTask = $client.ConnectAsync('127.0.0.1', 16379)
        if (-not $connectTask.Wait(3000) -or -not $client.Connected) {
            throw 'Timed out while applying Redis authentication.'
        }
        $stream = $client.GetStream()
        $stream.ReadTimeout = 3000
        $stream.WriteTimeout = 3000
        $arguments = @('CONFIG', 'SET', 'requirepass', $Password)
        $builder = [Text.StringBuilder]::new()
        [void]$builder.Append("*$($arguments.Count)`r`n")
        foreach ($argument in $arguments) {
            $byteCount = [Text.Encoding]::UTF8.GetByteCount($argument)
            [void]$builder.Append("`$$byteCount`r`n$argument`r`n")
        }
        $request = [Text.Encoding]::UTF8.GetBytes($builder.ToString())
        try {
            $stream.Write($request, 0, $request.Length)
            $stream.Flush()
            $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $false, 1024, $true)
            try {
                $response = $reader.ReadLine()
                if ($response -ne '+OK') { throw 'Redis rejected the authentication configuration.' }
            } finally {
                $reader.Dispose()
            }
        } finally {
            [Array]::Clear($request, 0, $request.Length)
            $builder.Clear() | Out-Null
            $stream.Dispose()
        }
    } finally {
        $client.Dispose()
    }
}

function Get-RedisPing([string]$Password) {
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $redisCli
    $startInfo.Arguments = '-h 127.0.0.1 -p 16379 ping'
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    if (-not [string]::IsNullOrWhiteSpace($Password)) {
        $startInfo.EnvironmentVariables['REDISCLI_AUTH'] = $Password
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { return '' }
        if (-not $process.WaitForExit(3000)) {
            $process.Kill()
            $process.WaitForExit()
            return ''
        }
        return $process.StandardOutput.ReadToEnd().Trim()
    } catch {
        return ''
    } finally {
        $process.Dispose()
    }
}

$pgPassword = Get-RouterCredential -Name 'PostgresPassword'
$redisPassword = Get-RouterCredential -Name 'RedisPassword'
$adminPassword = Get-RouterCredential -Name 'AdminPassword'
$jwtSecret = Get-RouterCredential -Name 'JwtSecret'
$totpKey = Get-RouterCredential -Name 'TotpEncryptionKey'

$pgCtl = "$routerRoot\postgres\pgsql\bin\pg_ctl.exe"
$pgData = "$routerRoot\data\postgres"
$pgConfig = "$routerRoot\config\postgresql.conf"
& $pgCtl status -D $pgData *> $null
if ($LASTEXITCODE -ne 0) {
    & $pgCtl start -D $pgData -s -w -t 60 -l "$routerRoot\logs\postgres.log" -o "-c config_file=$pgConfig"
    if ($LASTEXITCODE -ne 0) { throw "PostgreSQL failed to start with exit code $LASTEXITCODE" }
}

$env:PGPASSWORD = $pgPassword
$env:PGCONNECT_TIMEOUT = '8'
try {
    $exists = & "$routerRoot\postgres\pgsql\bin\psql.exe" -h 127.0.0.1 -p 15432 -U sub2api -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='sub2api'"
    if (($exists | Out-String).Trim() -ne '1') {
        & "$routerRoot\postgres\pgsql\bin\createdb.exe" -h 127.0.0.1 -p 15432 -U sub2api sub2api
        if ($LASTEXITCODE -ne 0) { throw "Failed to create the sub2api database." }
    }
} finally {
    Remove-Item Env:PGPASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:PGCONNECT_TIMEOUT -ErrorAction SilentlyContinue
}

$redisCli = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-cli.exe"
$authenticatedPing = Get-RedisPing -Password $redisPassword
if ($authenticatedPing -ne 'PONG') {
    $anonymousPing = Get-RedisPing -Password ''
    if ($anonymousPing -ne 'PONG') {
        if (Wait-TcpPort -Port 16379 -TimeoutSeconds 1) {
            throw 'Redis is running with an unknown password. Stop the verified local Redis process before restarting the router.'
        }
        $redisProcess = Start-Process `
            -FilePath "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe" `
            -ArgumentList @('../../config/redis.conf') `
            -WorkingDirectory "$routerRoot\data\redis" `
            -WindowStyle Hidden `
            -RedirectStandardOutput "$routerRoot\logs\redis-stdout.log" `
            -RedirectStandardError "$routerRoot\logs\redis-stderr.log" `
            -PassThru
        [IO.File]::WriteAllText("$routerRoot\data\pids\redis.pid", [string]$redisProcess.Id)
        if (-not (Wait-TcpPort -Port 16379 -TimeoutSeconds 30)) { throw 'Redis failed to listen on port 16379.' }
    }
    Set-RedisRequirePass -Password $redisPassword
    $authenticatedPing = Get-RedisPing -Password $redisPassword
    if ($authenticatedPing -ne 'PONG') { throw 'Redis authentication check failed.' }
}

$pidFile = "$routerRoot\data\pids\sub2api.pid"
$sub2apiRunning = $false
if (Test-Path -LiteralPath $pidFile) {
    $savedPid = [int]([IO.File]::ReadAllText($pidFile).Trim())
    $savedProcess = Get-CimInstance Win32_Process -Filter "ProcessId=$savedPid" -ErrorAction SilentlyContinue
    $expectedSub2Api = [IO.Path]::GetFullPath("$routerRoot\app\sub2api.exe")
    $sub2apiRunning = $null -ne $savedProcess -and
        -not [string]::IsNullOrWhiteSpace([string]$savedProcess.ExecutablePath) -and
        [IO.Path]::GetFullPath([string]$savedProcess.ExecutablePath).Equals(
            $expectedSub2Api,
            [StringComparison]::OrdinalIgnoreCase)
    if (-not $sub2apiRunning) {
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    }
}

if (-not $sub2apiRunning) {
    $environment = @{
        AUTO_SETUP = 'true'
        SERVER_HOST = '127.0.0.1'
        SERVER_PORT = '18080'
        SERVER_MODE = 'release'
        RUN_MODE = 'simple'
        TZ = 'UTC'
        DATA_DIR = "$routerRoot\data\sub2api"
        DATABASE_HOST = '127.0.0.1'
        DATABASE_PORT = '15432'
        DATABASE_USER = 'sub2api'
        DATABASE_PASSWORD = $pgPassword
        DATABASE_DBNAME = 'sub2api'
        DATABASE_SSLMODE = 'disable'
        DATABASE_MAX_OPEN_CONNS = '40'
        DATABASE_MAX_IDLE_CONNS = '10'
        REDIS_HOST = '127.0.0.1'
        REDIS_PORT = '16379'
        REDIS_PASSWORD = $redisPassword
        REDIS_DB = '0'
        REDIS_POOL_SIZE = '128'
        REDIS_MIN_IDLE_CONNS = '4'
        ADMIN_EMAIL = 'admin@sub2api.local'
        ADMIN_PASSWORD = $adminPassword
        JWT_SECRET = $jwtSecret
        TOTP_ENCRYPTION_KEY = $totpKey
        JWT_EXPIRE_HOUR = '24'
        LOG_LEVEL = 'info'
        LOG_FORMAT = 'console'
        LOG_OUTPUT_TO_STDOUT = 'true'
        LOG_OUTPUT_TO_FILE = 'true'
        LOG_OUTPUT_FILE_PATH = "$routerRoot\logs\sub2api.log"
        GATEWAY_FORCE_CODEX_CLI = 'true'
        GATEWAY_OPENAI_RESPONSE_HEADER_TIMEOUT = '0'
        RATE_LIMIT_OVERLOAD_COOLDOWN_MINUTES = '60'
        SECURITY_URL_ALLOWLIST_ENABLED = 'false'
        SECURITY_URL_ALLOWLIST_ALLOW_INSECURE_HTTP = 'false'
        SECURITY_URL_ALLOWLIST_ALLOW_PRIVATE_HOSTS = 'true'
    }
    $configPath = Join-Path $routerRoot 'codex-router-config.json'
    if (Test-Path -LiteralPath $configPath) {
        $routerConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
        if ($routerConfig.proxy -and $routerConfig.proxy.enabled) {
            $proxyCredentialProperty = $routerConfig.proxy.PSObject.Properties['passwordCredential']
            $proxyCredential = if ($null -ne $proxyCredentialProperty -and $proxyCredentialProperty.Value) { [string]$proxyCredentialProperty.Value } else { 'ProxyPassword' }
            $proxyPassword = Get-RouterCredential -Name $proxyCredential -AllowMissing
            $proxyAuth = ''
            if ($routerConfig.proxy.username) {
                $proxyAuth = [Uri]::EscapeDataString([string]$routerConfig.proxy.username) + ':' + [Uri]::EscapeDataString([string]$proxyPassword) + '@'
            }
            $proxyTypeProperty = $routerConfig.proxy.PSObject.Properties['proxyType']
            $legacyTypeProperty = $routerConfig.proxy.PSObject.Properties['type']
            $proxyType = if ($null -ne $proxyTypeProperty -and $proxyTypeProperty.Value) { [string]$proxyTypeProperty.Value } elseif ($null -ne $legacyTypeProperty -and $legacyTypeProperty.Value) { [string]$legacyTypeProperty.Value } else { 'http' }
            $proxyUrl = $proxyType + '://' + $proxyAuth + ([string]$routerConfig.proxy.host) + ':' + ([string]$routerConfig.proxy.port)
            $environment.HTTP_PROXY = $proxyUrl
            $environment.HTTPS_PROXY = $proxyUrl
            $environment.ALL_PROXY = $proxyUrl
        }
    }
    foreach ($item in $environment.GetEnumerator()) { [Environment]::SetEnvironmentVariable($item.Key, $item.Value, 'Process') }
    try {
        if (-not (Test-Path -LiteralPath "$routerRoot\data\sub2api")) { New-Item -ItemType Directory -Path "$routerRoot\data\sub2api" | Out-Null }
        $sub2apiProcess = Start-Process `
            -FilePath "$routerRoot\app\sub2api.exe" `
            -WorkingDirectory "$routerRoot\app" `
            -WindowStyle Hidden `
            -RedirectStandardOutput "$routerRoot\logs\sub2api-stdout.log" `
            -RedirectStandardError "$routerRoot\logs\sub2api-stderr.log" `
            -PassThru
        [IO.File]::WriteAllText($pidFile, [string]$sub2apiProcess.Id)
    } finally {
        foreach ($key in $environment.Keys) { [Environment]::SetEnvironmentVariable($key, $null, 'Process') }
    }
}

$sub2apiReady = $false
$deadline = [DateTime]::UtcNow.AddSeconds(120)
do {
    try {
        $health = Invoke-RestMethod -Uri 'http://127.0.0.1:18080/health' -TimeoutSec 3
        if ($null -ne $health) {
            $sub2apiReady = $true
            break
        }
    } catch { }
    Start-Sleep -Seconds 1
} while ([DateTime]::UtcNow -lt $deadline)

if (-not $sub2apiReady) {
    throw 'Sub2API health check did not become ready within 120 seconds.'
}

Write-Output 'Codex Router is running at http://127.0.0.1:18080'
