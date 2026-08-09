param(
    [switch]$RepairUnhealthy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\ProxyDiscovery.psm1" -Force
Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot
$sub2apiBaseUri = Get-RouterBaseUri
$sub2apiUri = [Uri]$sub2apiBaseUri
$sub2apiPort = $sub2apiUri.Port

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

function ConvertTo-WindowsCommandLineArgument([string]$Argument) {
    if ($Argument -notmatch '[\s"]') { return $Argument }
    # Escape trailing backslashes and any backslashes immediately before a
    # double quote according to CommandLineToArgvW parsing rules.
    return '"' + ([Regex]::Replace($Argument, '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-NativeQuiet {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$ArgumentList,
        [ValidateRange(1, 120)][int]$TimeoutSeconds = 75
    )

    $quotedArguments = @($ArgumentList | ForEach-Object {
        ConvertTo-WindowsCommandLineArgument -Argument ([string]$_)
    })
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = $quotedArguments -join ' '
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { return -1 }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            [void]$process.WaitForExit(2000)
            return -2
        }
        $process.Refresh()
        return [int]$process.ExitCode
    } finally {
        $process.Dispose()
    }
}

function Invoke-PostgresScalar {
    param(
        [Parameter(Mandatory)][string]$Query,
        [Parameter(Mandatory)][string]$Password,
        [string]$Database = 'postgres',
        [ValidateRange(1, 30)][int]$TimeoutSeconds = 10
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "$routerRoot\postgres\pgsql\bin\psql.exe"
    $quotedArguments = @(
        '-X', '-w', '-h', '127.0.0.1', '-p', '15432',
        '-U', 'sub2api', '-d', $Database, '-tAc', $Query
    ) | ForEach-Object {
        ConvertTo-WindowsCommandLineArgument -Argument ([string]$_)
    }
    $startInfo.Arguments = $quotedArguments -join ' '
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.EnvironmentVariables['PGPASSWORD'] = $Password
    $startInfo.EnvironmentVariables['PGCONNECT_TIMEOUT'] = [string][Math]::Min($TimeoutSeconds, 8)
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            return [PSCustomObject]@{ Succeeded = $false; TimedOut = $false; Output = '' }
        }
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            [void]$process.WaitForExit(2000)
            return [PSCustomObject]@{ Succeeded = $false; TimedOut = $true; Output = '' }
        }
        $process.Refresh()
        $output = $process.StandardOutput.ReadToEnd().Trim()
        return [PSCustomObject]@{
            Succeeded = $process.ExitCode -eq 0
            TimedOut = $false
            Output = $output
        }
    } finally {
        $process.Dispose()
    }
}

function Get-VerifiedLoopbackListener {
    param(
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][string]$ServiceName,
        [switch]$AllowMissing
    )
    $listeners = @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
    if ($listeners.Count -eq 0) {
        if ($AllowMissing) { return $null }
        throw "$ServiceName is not listening on 127.0.0.1:$Port."
    }
    $unexpected = @($listeners | Where-Object { $_.LocalAddress -ne '127.0.0.1' })
    if ($unexpected.Count -gt 0) {
        throw "$ServiceName has a non-loopback listener on port $Port; refusing to continue."
    }
    $processIds = @($listeners | Select-Object -ExpandProperty OwningProcess -Unique)
    if ($processIds.Count -ne 1) {
        throw "$ServiceName port $Port is owned by an ambiguous process set."
    }
    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($processIds[0])" -ErrorAction SilentlyContinue
    $expected = [IO.Path]::GetFullPath($ExpectedPath)
    if ($null -eq $process -or
        [string]::IsNullOrWhiteSpace([string]$process.ExecutablePath)) {
        throw "ROUTER_PORT_CONFLICT: $ServiceName port $Port is owned by an unidentified process."
    }
    $actual = [IO.Path]::GetFullPath([string]$process.ExecutablePath)
    if (-not $actual.Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
        if ([IO.Path]::GetFileName($actual).Equals(
            [IO.Path]::GetFileName($expected),
            [StringComparison]::OrdinalIgnoreCase)) {
            if ($RepairUnhealthy) {
                # A previous portable folder left services running. Take ownership
                # so upgrades do not leave the GUI talking to a foreign Sub2API.
                Write-Warning "ROUTER_INSTALL_ROOT_REPAIR: stopping foreign $ServiceName from $actual"
                try {
                    Stop-Process -Id ([int]$process.ProcessId) -Force -ErrorAction Stop
                    Wait-Process -Id ([int]$process.ProcessId) -Timeout 8 -ErrorAction SilentlyContinue
                } catch {
                    throw "ROUTER_INSTALL_ROOT_CONFLICT: Could not stop foreign $ServiceName PID $($process.ProcessId): $($_.Exception.Message)"
                }
                Start-Sleep -Milliseconds 400
                # Treat the port as free so the caller can start this package's binary.
                return $null
            }
            throw "ROUTER_INSTALL_ROOT_CONFLICT: $ServiceName port $Port is owned by another Codex-Router installation."
        }
        throw "ROUTER_PORT_CONFLICT: $ServiceName port $Port is owned by another program."
    }
    return $process
}

function Test-PostgresReady {
    param([int]$TimeoutSeconds = 3)
    $exitCode = Invoke-NativeQuiet `
        -FilePath "$routerRoot\postgres\pgsql\bin\pg_isready.exe" `
        -ArgumentList @('-h', '127.0.0.1', '-p', '15432', '-d', 'postgres', '-U', 'sub2api', '-t', [string]$TimeoutSeconds)
    return $exitCode -eq 0
}

function Test-PostgresReadyStable {
    param([Parameter(Mandatory)][string]$Password)
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        if (Test-PostgresReady -TimeoutSeconds 2) {
            $sqlProbe = Invoke-PostgresScalar `
                -Query 'SELECT 1' `
                -Password $Password `
                -TimeoutSeconds 8
            if ($sqlProbe.Succeeded -and $sqlProbe.Output -eq '1') { return $true }
        }
        if ($attempt -lt 2) { Start-Sleep -Milliseconds 500 }
    }
    return $false
}

function Test-Sub2ApiHealth {
    param([Parameter(Mandatory)][string]$Uri, [int]$TimeoutMilliseconds = 3000)
    $request = $null
    $response = $null
    try {
        $request = [Net.HttpWebRequest]::Create($Uri)
        $request.Method = 'GET'
        $request.Proxy = $null
        $request.Timeout = $TimeoutMilliseconds
        $request.ReadWriteTimeout = $TimeoutMilliseconds
        $request.KeepAlive = $false
        $response = [Net.HttpWebResponse]$request.GetResponse()
        return [int]$response.StatusCode -eq 200
    } catch {
        return $false
    } finally {
        if ($null -ne $response) { $response.Dispose() }
    }
}

function Test-Sub2ApiHealthStable {
    param([Parameter(Mandatory)][string]$Uri)
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        if (Test-Sub2ApiHealth -Uri $Uri -TimeoutMilliseconds 1500) { return $true }
        if ($attempt -lt 2) { Start-Sleep -Milliseconds 300 }
    }
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

function Wait-RedisAuthenticatedPing {
    param(
        [Parameter(Mandatory)][string]$Password,
        [ValidateRange(1, 60)][int]$TimeoutSeconds = 30
    )
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if ((Get-RedisPing -Password $Password) -eq 'PONG') { return $true }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Get-NetworkSettingsFingerprint {
    param(
        [Parameter(Mandatory)][string]$Value,
        [Parameter(Mandatory)][string]$Secret
    )
    $keyBytes = [Text.Encoding]::UTF8.GetBytes($Secret)
    $valueBytes = [Text.Encoding]::UTF8.GetBytes($Value)
    $algorithm = [Security.Cryptography.HMACSHA256]::new($keyBytes)
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($valueBytes))).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
        [Array]::Clear($keyBytes, 0, $keyBytes.Length)
        [Array]::Clear($valueBytes, 0, $valueBytes.Length)
    }
}

function Stop-Sub2ApiForNetworkChange {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][int]$Port
    )
    Assert-RouterServiceInterruptionAllowed `
        -ProcessId $ProcessId `
        -Port $Port `
        -Operation 'Proxy settings change'
    Stop-Process -Id $ProcessId -Force -ErrorAction Stop
    Wait-Process -Id $ProcessId -Timeout 5 -ErrorAction SilentlyContinue
    if (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue) {
        throw 'Sub2API did not exit during the bounded network reconfiguration.'
    }
}

function Stop-Sub2ApiForRecovery {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][int]$Port
    )
    [void](Get-VerifiedLoopbackListener `
        -Port $Port `
        -ExpectedPath $expectedSub2Api `
        -ServiceName 'Sub2API')
    Stop-Process -Id $ProcessId -Force -ErrorAction Stop
    Wait-Process -Id $ProcessId -Timeout 5 -ErrorAction SilentlyContinue
    if (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue) {
        throw 'Sub2API did not exit during verified unhealthy-service recovery.'
    }
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $networkFingerprintFile -Force -ErrorAction SilentlyContinue
}

function Stop-VerifiedPostgresTree {
    param(
        [Parameter(Mandatory)][int]$MainProcessId,
        [Parameter(Mandatory)][string]$ExpectedPath
    )
    $main = Get-CimInstance Win32_Process -Filter "ProcessId=$MainProcessId" -ErrorAction SilentlyContinue
    $expected = [IO.Path]::GetFullPath($ExpectedPath)
    if ($null -eq $main -or
        [string]::IsNullOrWhiteSpace([string]$main.ExecutablePath) -or
        -not [IO.Path]::GetFullPath([string]$main.ExecutablePath).Equals(
            $expected,
            [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing to terminate an unverified PostgreSQL process.'
    }

    $allProcesses = @(Get-CimInstance Win32_Process -ErrorAction Stop)
    $verifiedIds = [Collections.Generic.HashSet[int]]::new()
    [void]$verifiedIds.Add($MainProcessId)
    do {
        $added = $false
        foreach ($process in $allProcesses) {
            if ($process.Name -ne 'postgres.exe' -or
                -not $verifiedIds.Contains([int]$process.ParentProcessId) -or
                $verifiedIds.Contains([int]$process.ProcessId)) {
                continue
            }
            if (-not [string]::IsNullOrWhiteSpace([string]$process.ExecutablePath) -and
                -not [IO.Path]::GetFullPath([string]$process.ExecutablePath).Equals(
                    $expected,
                    [StringComparison]::OrdinalIgnoreCase)) {
                throw 'A PostgreSQL descendant has an unexpected executable path.'
            }
            [void]$verifiedIds.Add([int]$process.ProcessId)
            $added = $true
        }
    } while ($added)

    $descendants = @($verifiedIds | Where-Object { $_ -ne $MainProcessId })
    if ($descendants.Count -gt 0) {
        Stop-Process -Id $descendants -Force -ErrorAction SilentlyContinue
    }
    Stop-Process -Id $MainProcessId -Force -ErrorAction Stop
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if ($null -eq (Get-Process -Id $MainProcessId -ErrorAction SilentlyContinue)) { return }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Verified PostgreSQL process did not exit after forced recovery.'
}

$lifecycleLock = Enter-RouterLifecycleLock `
    -RouterRoot $routerRoot `
    -TimeoutMilliseconds 10000 `
    -Operation 'Start Router'
$previousLifecycleLockMarker = [Environment]::GetEnvironmentVariable(
    'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
    'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_LIFECYCLE_LOCK_HELD', [string]$PID, 'Process')
try {
$pgPassword = Get-RouterCredential -Name 'PostgresPassword'
$redisPassword = Get-RouterCredential -Name 'RedisPassword'
$adminPassword = Get-RouterCredential -Name 'AdminPassword'
$jwtSecret = Get-RouterCredential -Name 'JwtSecret'
$totpKey = Get-RouterCredential -Name 'TotpEncryptionKey'

$configPath = Get-RouterConfigPath -RouterRoot $routerRoot
$routerConfig = $null
$proxyConfig = $null
$proxyPassword = $null
if (Test-Path -LiteralPath $configPath) {
    $routerConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    $proxyProperty = $routerConfig.PSObject.Properties['proxy']
    if ($null -ne $proxyProperty) { $proxyConfig = $proxyProperty.Value }
    $proxyEnabledProperty = if ($null -ne $proxyConfig) {
        $proxyConfig.PSObject.Properties['enabled']
    } else {
        $null
    }
    if ($null -ne $proxyEnabledProperty -and [bool]$proxyEnabledProperty.Value) {
        $proxyCredentialProperty = $proxyConfig.PSObject.Properties['passwordCredential']
        $proxyCredential = if ($null -ne $proxyCredentialProperty -and $proxyCredentialProperty.Value) {
            [string]$proxyCredentialProperty.Value
        } else {
            'ProxyPassword'
        }
        $proxyPassword = Get-RouterCredential -Name $proxyCredential -AllowMissing
    }
}
$proxySettings = Resolve-RouterProxySettings `
    -ProxyConfig $proxyConfig `
    -ProxyPassword $proxyPassword
$networkFingerprint = Get-NetworkSettingsFingerprint `
    -Value (([string]$proxySettings.ProxyUrl) + "`n" + $proxySettings.NoProxy) `
    -Secret $jwtSecret
$networkFingerprintFile = Join-Path $dataRoot 'pids\sub2api-network.hmac'

# Resolve every potentially destructive Sub2API decision before touching its
# dependencies. A transient listener/health miss is treated as deferred state,
# never as permission to terminate a possibly busy gateway.
$pidFile = Join-Path $dataRoot 'pids\sub2api.pid'
$expectedSub2Api = [IO.Path]::GetFullPath("$routerRoot\app\sub2api.exe")
$savedPid = 0
$sub2apiRunning = $false
if (Test-Path -LiteralPath $pidFile) {
    $savedPidText = [IO.File]::ReadAllText($pidFile).Trim()
    if ([int]::TryParse($savedPidText, [ref]$savedPid)) {
        $savedProcess = Get-CimInstance Win32_Process -Filter "ProcessId=$savedPid" -ErrorAction SilentlyContinue
        $sub2apiRunning = $null -ne $savedProcess -and
            -not [string]::IsNullOrWhiteSpace([string]$savedProcess.ExecutablePath) -and
            [IO.Path]::GetFullPath([string]$savedProcess.ExecutablePath).Equals(
                $expectedSub2Api,
                [StringComparison]::OrdinalIgnoreCase)
    }
    if (-not $sub2apiRunning) {
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    }
}

if ($sub2apiRunning) {
    $verifiedSub2Api = Get-VerifiedLoopbackListener `
        -Port $sub2apiPort `
        -ExpectedPath $expectedSub2Api `
        -ServiceName 'Sub2API' `
        -AllowMissing
    if ($null -eq $verifiedSub2Api) {
        if (-not $RepairUnhealthy) {
            throw "ROUTER_LIFECYCLE_DEFERRED: Sub2API PID $savedPid is still running but its listener is temporarily unavailable. No Router service was changed; retry Start later."
        }
        Stop-Process -Id $savedPid -Force -ErrorAction Stop
        Wait-Process -Id $savedPid -Timeout 5 -ErrorAction SilentlyContinue
        if (Get-Process -Id $savedPid -ErrorAction SilentlyContinue) {
            throw 'Sub2API did not exit during missing-listener recovery.'
        }
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $networkFingerprintFile -Force -ErrorAction SilentlyContinue
        $sub2apiRunning = $false
    }
    if ($null -ne $verifiedSub2Api -and [int]$verifiedSub2Api.ProcessId -ne $savedPid) {
        throw 'The Sub2API PID file and verified listener refer to different processes.'
    }

    $storedNetworkFingerprint = if ($sub2apiRunning -and (Test-Path -LiteralPath $networkFingerprintFile)) {
        [IO.File]::ReadAllText($networkFingerprintFile).Trim()
    } else {
        ''
    }
    if ($sub2apiRunning -and -not [string]::Equals(
        $storedNetworkFingerprint,
        $networkFingerprint,
        [StringComparison]::OrdinalIgnoreCase)) {
        Stop-Sub2ApiForNetworkChange -ProcessId $savedPid -Port $sub2apiPort
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $networkFingerprintFile -Force -ErrorAction SilentlyContinue
        $sub2apiRunning = $false
    } elseif ($sub2apiRunning -and -not (Test-Sub2ApiHealthStable -Uri "$sub2apiBaseUri/health")) {
        if (-not $RepairUnhealthy) {
            throw "ROUTER_LIFECYCLE_DEFERRED: Sub2API PID $savedPid did not pass the bounded health observation. It was not terminated; retry Start later."
        }
        Stop-Sub2ApiForRecovery -ProcessId $savedPid -Port $sub2apiPort
        $sub2apiRunning = $false
    }
}

if (-not $sub2apiRunning) {
    $existingListener = Get-VerifiedLoopbackListener `
        -Port $sub2apiPort `
        -ExpectedPath $expectedSub2Api `
        -ServiceName 'Sub2API' `
        -AllowMissing
    if ($null -ne $existingListener) {
        $existingPid = [int]$existingListener.ProcessId
        $storedNetworkFingerprint = if (Test-Path -LiteralPath $networkFingerprintFile) {
            [IO.File]::ReadAllText($networkFingerprintFile).Trim()
        } else {
            ''
        }
        $networkSettingsMatch = [string]::Equals(
            $storedNetworkFingerprint,
            $networkFingerprint,
            [StringComparison]::OrdinalIgnoreCase)
        if (-not $networkSettingsMatch) {
            Stop-Sub2ApiForNetworkChange -ProcessId $existingPid -Port $sub2apiPort
            Remove-Item -LiteralPath $networkFingerprintFile -Force -ErrorAction SilentlyContinue
        } elseif (Test-Sub2ApiHealthStable -Uri "$sub2apiBaseUri/health") {
            $savedPid = $existingPid
            Write-RouterFileAtomic `
                -Path $pidFile `
                -Bytes ([Text.Encoding]::ASCII.GetBytes([string]$savedPid))
            $sub2apiRunning = $true
        } else {
            if (-not $RepairUnhealthy) {
                throw "ROUTER_LIFECYCLE_DEFERRED: Sub2API PID $existingPid owns the listener but did not pass the bounded health observation. It was not terminated; retry Start later."
            }
            Stop-Sub2ApiForRecovery -ProcessId $existingPid -Port $sub2apiPort
        }
    }
}

$pgCtl = "$routerRoot\postgres\pgsql\bin\pg_ctl.exe"
$pgData = Join-Path $dataRoot 'postgres'
$pgConfig = "$routerRoot\config\postgresql.conf"
$postgresExe = "$routerRoot\postgres\pgsql\bin\postgres.exe"
$pgStatusExitCode = Invoke-NativeQuiet -FilePath $pgCtl -ArgumentList @('status', '-D', $pgData)
$postgresRunning = $pgStatusExitCode -eq 0
if ($postgresRunning -and -not (Test-PostgresReadyStable -Password $pgPassword)) {
    if (-not $RepairUnhealthy) {
        throw 'PostgreSQL is running but did not pass the bounded readiness probe. Retry with -RepairUnhealthy after active requests have been ruled out.'
    }
    if ($sub2apiRunning) {
        Stop-Sub2ApiForRecovery -ProcessId $savedPid -Port $sub2apiPort
        $sub2apiRunning = $false
    }
    $postgresProcess = Get-VerifiedLoopbackListener `
        -Port 15432 `
        -ExpectedPath $postgresExe `
        -ServiceName 'PostgreSQL'
    $pgStopExitCode = Invoke-NativeQuiet `
        -FilePath $pgCtl `
        -ArgumentList @('stop', '-D', $pgData, '-s', '-m', 'fast', '-w', '-t', '15')
    if ($pgStopExitCode -ne 0) {
        Stop-VerifiedPostgresTree `
            -MainProcessId ([int]$postgresProcess.ProcessId) `
            -ExpectedPath $postgresExe
    }
    $postgresRunning = $false
}
if (-not $postgresRunning) {
    $unexpectedPostgres = Get-VerifiedLoopbackListener `
        -Port 15432 `
        -ExpectedPath $postgresExe `
        -ServiceName 'PostgreSQL' `
        -AllowMissing
    if ($null -ne $unexpectedPostgres) {
        if (-not $RepairUnhealthy) {
            throw 'PostgreSQL is listening from this package but is not using the expected data directory.'
        }
        Write-Warning 'ROUTER_POSTGRES_DATADIR_REPAIR: stopping package PostgreSQL that is not using the expected data directory.'
        $pgStopExitCode = Invoke-NativeQuiet `
            -FilePath $pgCtl `
            -ArgumentList @('stop', '-D', $pgData, '-s', '-m', 'fast', '-w', '-t', '15')
        if ($pgStopExitCode -ne 0) {
            Stop-VerifiedPostgresTree `
                -MainProcessId ([int]$unexpectedPostgres.ProcessId) `
                -ExpectedPath $postgresExe
        }
        Start-Sleep -Milliseconds 500
        $stillListening = Get-VerifiedLoopbackListener `
            -Port 15432 `
            -ExpectedPath $postgresExe `
            -ServiceName 'PostgreSQL' `
            -AllowMissing
        if ($null -ne $stillListening) {
            throw 'PostgreSQL remained listening after data-directory repair.'
        }
    }
    # Stale postmaster.pid after a hard kill makes pg_ctl status report "no server"
    # while also blocking a clean start.
    $postmasterPidFile = Join-Path $pgData 'postmaster.pid'
    if (Test-Path -LiteralPath $postmasterPidFile) {
        $stalePid = 0
        try {
            $firstLine = @(Get-Content -LiteralPath $postmasterPidFile -TotalCount 1)[0]
            [void][int]::TryParse(([string]$firstLine).Trim(), [ref]$stalePid)
        } catch {
            $stalePid = 0
        }
        if ($stalePid -le 0 -or -not (Get-Process -Id $stalePid -ErrorAction SilentlyContinue)) {
            Remove-Item -LiteralPath $postmasterPidFile -Force -ErrorAction SilentlyContinue
        }
    }
    [IO.Directory]::CreateDirectory((Join-Path $routerRoot 'logs')) | Out-Null
    $pgBin = Join-Path $routerRoot 'postgres\pgsql\bin'
    $previousPath = [Environment]::GetEnvironmentVariable('PATH', 'Process')
    try {
        if ($previousPath -notlike "*${pgBin}*") {
            [Environment]::SetEnvironmentVariable('PATH', ($pgBin + ';' + $previousPath), 'Process')
        }
        $pgStartExitCode = Invoke-NativeQuiet `
            -FilePath $pgCtl `
            -ArgumentList @('start', '-D', $pgData, '-s', '-w', '-t', '60', '-l', "$routerRoot\logs\postgres.log", '-o', "-c config_file=$pgConfig")
    } finally {
        [Environment]::SetEnvironmentVariable('PATH', $previousPath, 'Process')
    }
    if ($pgStartExitCode -ne 0) { throw "PostgreSQL failed to start with exit code $pgStartExitCode" }
}
if (-not (Test-PostgresReadyStable -Password $pgPassword)) {
    throw 'PostgreSQL did not pass pg_isready.'
}
[void](Get-VerifiedLoopbackListener `
    -Port 15432 `
    -ExpectedPath $postgresExe `
    -ServiceName 'PostgreSQL')

$databaseProbe = Invoke-PostgresScalar `
    -Query "SELECT 1 FROM pg_database WHERE datname='sub2api'" `
    -Password $pgPassword `
    -TimeoutSeconds 10
if (-not $databaseProbe.Succeeded) {
    throw 'PostgreSQL accepted a socket connection but did not complete the database probe.'
}
if ($databaseProbe.Output -ne '1') {
    $previousPassword = [Environment]::GetEnvironmentVariable('PGPASSWORD', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $pgPassword, 'Process')
        & "$routerRoot\postgres\pgsql\bin\createdb.exe" -h 127.0.0.1 -p 15432 -U sub2api sub2api
        if ($LASTEXITCODE -ne 0) { throw 'Failed to create the sub2api database.' }
    } finally {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $previousPassword, 'Process')
    }
}

$redisCli = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-cli.exe"
$redisServer = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe"
$redisConfig = "$routerRoot\config\redis.conf"
$redisRuntimeConfig = Join-Path $dataRoot 'redis\redis.conf'
$authenticatedPing = Get-RedisPing -Password $redisPassword
if ($authenticatedPing -ne 'PONG') {
    $anonymousPing = Get-RedisPing -Password ''
    if ($anonymousPing -ne 'PONG') {
        if (Wait-TcpPort -Port 16379 -TimeoutSeconds 1) {
            # Redis opens its listener before persisted AOF/RDB data is fully
            # loaded. Wait for the password already staged in redis.conf before
            # treating the process as a foreign instance with an unknown key.
            if (-not (Wait-RedisAuthenticatedPing -Password $redisPassword -TimeoutSeconds 30)) {
                throw 'Redis is running with an unknown password. Stop the verified local Redis process before restarting the router.'
            }
        } else {
            # Bake requirepass into the runtime config so Redis never accepts
            # unauthenticated clients during startup.
            $redisConfigText = [IO.File]::ReadAllText($redisConfig)
            if ($redisConfigText -notmatch '(?m)^\s*requirepass\s+') {
                $redisConfigText = $redisConfigText.TrimEnd() + "`nrequirepass $redisPassword`n"
            } else {
                $redisConfigText = [regex]::Replace(
                    $redisConfigText,
                    '(?m)^\s*requirepass\s+\S+\s*$',
                    "requirepass $redisPassword")
            }
            Write-RouterFileAtomic `
                -Path $redisRuntimeConfig `
                -Bytes ([Text.Encoding]::UTF8.GetBytes($redisConfigText))
            $redisProcess = Start-Process `
                -FilePath $redisServer `
                -ArgumentList @('redis.conf') `
                -WorkingDirectory (Join-Path $dataRoot 'redis') `
                -WindowStyle Hidden `
                -RedirectStandardOutput "$routerRoot\logs\redis-stdout.log" `
                -RedirectStandardError "$routerRoot\logs\redis-stderr.log" `
                -PassThru
            $redisStarted = $false
            $redisDeadline = [DateTime]::UtcNow.AddSeconds(30)
            do {
                $redisProcess.Refresh()
                if ($redisProcess.HasExited) {
                    throw "Redis exited before listening on port 16379 (exit code $($redisProcess.ExitCode))."
                }
                if (Wait-TcpPort -Port 16379 -TimeoutSeconds 1) {
                    $redisStarted = $true
                    break
                }
            } while ([DateTime]::UtcNow -lt $redisDeadline)
            if (-not $redisStarted) { throw 'Redis failed to listen on port 16379.' }
            Write-RouterFileAtomic `
                -Path (Join-Path $dataRoot 'pids\redis.pid') `
                -Bytes ([Text.Encoding]::ASCII.GetBytes([string]$redisProcess.Id))
            if (-not (Wait-RedisAuthenticatedPing -Password $redisPassword -TimeoutSeconds 30)) {
                throw 'Redis did not become ready with the configured authentication after startup.'
            }
        }
    } else {
        # An already-running unauthenticated Redis still needs the password.
        Set-RedisRequirePass -Password $redisPassword
        if (-not (Wait-RedisAuthenticatedPing -Password $redisPassword -TimeoutSeconds 5)) {
            throw 'Redis authentication check failed.'
        }
    }
}

function Test-Sub2ApiDatabaseReady {
    param([Parameter(Mandatory)][string]$Password)
    $probe = Invoke-PostgresScalar `
        -Database 'sub2api' `
        -Query "SELECT COUNT(*) FROM users WHERE role = 'admin' AND status = 'active'" `
        -Password $Password `
        -TimeoutSeconds 8
    if (-not $probe.Succeeded) { return $false }
    $count = 0L
    return [long]::TryParse($probe.Output, [ref]$count) -and $count -gt 0
}
[void](Get-VerifiedLoopbackListener `
    -Port 16379 `
    -ExpectedPath $redisServer `
    -ServiceName 'Redis')

$sub2apiProcess = $null
if (-not $sub2apiRunning) {
    $environment = @{
        AUTO_SETUP = 'true'
        SERVER_HOST = '127.0.0.1'
        SERVER_PORT = [string]$sub2apiPort
        SERVER_MODE = 'release'
        RUN_MODE = 'simple'
        TZ = 'UTC'
        DATA_DIR = (Join-Path $dataRoot 'sub2api')
        DATABASE_HOST = '127.0.0.1'
        DATABASE_PORT = '15432'
        DATABASE_USER = 'sub2api'
        DATABASE_PASSWORD = $pgPassword
        DATABASE_DBNAME = 'sub2api'
        DATABASE_SSLMODE = 'disable'
        PGCONNECT_TIMEOUT = '8'
        DATABASE_MAX_OPEN_CONNS = '16'
        DATABASE_MAX_IDLE_CONNS = '4'
        REDIS_HOST = '127.0.0.1'
        REDIS_PORT = '16379'
        REDIS_PASSWORD = $redisPassword
        REDIS_DB = '0'
        REDIS_POOL_SIZE = '32'
        REDIS_MIN_IDLE_CONNS = '2'
        ADMIN_EMAIL = 'admin@admin.com'
        ADMIN_PASSWORD = $adminPassword
        JWT_SECRET = $jwtSecret
        TOTP_ENCRYPTION_KEY = $totpKey
        JWT_EXPIRE_HOUR = '24'
        LOG_LEVEL = 'warn'
        LOG_FORMAT = 'console'
        LOG_OUTPUT_TO_STDOUT = 'false'
        LOG_OUTPUT_TO_FILE = 'true'
        LOG_OUTPUT_FILE_PATH = "$routerRoot\logs\sub2api.log"
        LOG_ROTATION_MAX_SIZE_MB = '20'
        LOG_ROTATION_MAX_BACKUPS = '3'
        LOG_ROTATION_MAX_AGE_DAYS = '3'
        LOG_SAMPLING_ENABLED = 'true'
        LOG_SAMPLING_INITIAL = '20'
        LOG_SAMPLING_THEREAFTER = '100'
        GOMEMLIMIT = '192MiB'
        GOGC = '75'
        GATEWAY_RESPONSE_HEADER_TIMEOUT = '30'
        GATEWAY_OPENAI_FIRST_OUTPUT_TIMEOUT_SECONDS = '60'
        GATEWAY_OPENAI_HIGH_EFFORT_FIRST_OUTPUT_TIMEOUT_SECONDS = '300'
        GATEWAY_MAX_ACCOUNT_SWITCHES = '4'
        GATEWAY_CONNECTION_POOL_ISOLATION = 'proxy'
        GATEWAY_MAX_IDLE_CONNS = '64'
        GATEWAY_MAX_IDLE_CONNS_PER_HOST = '16'
        GATEWAY_MAX_CONNS_PER_HOST = '32'
        GATEWAY_MAX_UPSTREAM_CLIENTS = '64'
        GATEWAY_CLIENT_IDLE_TTL_SECONDS = '300'
        GATEWAY_STREAM_DATA_INTERVAL_TIMEOUT = '60'
        GATEWAY_STREAM_KEEPALIVE_INTERVAL = '10'
        GATEWAY_FORCE_CODEX_CLI = 'true'
        GATEWAY_OPENAI_RESPONSE_HEADER_TIMEOUT = '0'
        RATE_LIMIT_OVERLOAD_COOLDOWN_MINUTES = '60'
        SECURITY_URL_ALLOWLIST_ENABLED = 'false'
        SECURITY_URL_ALLOWLIST_ALLOW_INSECURE_HTTP = 'false'
        SECURITY_URL_ALLOWLIST_ALLOW_PRIVATE_HOSTS = 'true'
    }
    if ($null -ne $proxySettings.ProxyUrl) {
        foreach ($name in @('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy')) {
            $environment[$name] = $proxySettings.ProxyUrl
        }
        # Sub2API's pricing updater uses its dedicated update proxy setting
        # instead of Go's process-wide proxy environment.
        $environment.UPDATE_PROXY_URL = $proxySettings.ProxyUrl
    } else {
        foreach ($name in @('HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy')) {
            $environment[$name] = $null
        }
        $environment.UPDATE_PROXY_URL = $null
    }
    $environment.NO_PROXY = $proxySettings.NoProxy
    $environment.no_proxy = $proxySettings.NoProxy
    $originalEnvironment = @{}
    foreach ($item in $environment.GetEnumerator()) {
        $originalEnvironment[$item.Key] = [Environment]::GetEnvironmentVariable($item.Key, 'Process')
        [Environment]::SetEnvironmentVariable($item.Key, $item.Value, 'Process')
    }
    try {
        if (-not (Test-Path -LiteralPath (Join-Path $dataRoot 'sub2api'))) { New-Item -ItemType Directory -Path (Join-Path $dataRoot 'sub2api') | Out-Null }
        foreach ($streamName in @('stdout', 'stderr')) {
            $streamLog = "$routerRoot\logs\sub2api-$streamName.log"
            $previousStreamLog = "$routerRoot\logs\sub2api-$streamName.previous.log"
            if ((Test-Path -LiteralPath $streamLog) -and (Get-Item -LiteralPath $streamLog).Length -gt 0) {
                Move-Item -LiteralPath $streamLog -Destination $previousStreamLog -Force
            }
        }
        $sub2apiProcess = Start-Process `
            -FilePath "$routerRoot\app\sub2api.exe" `
            -WorkingDirectory "$routerRoot\app" `
            -WindowStyle Hidden `
            -RedirectStandardOutput "$routerRoot\logs\sub2api-stdout.log" `
            -RedirectStandardError "$routerRoot\logs\sub2api-stderr.log" `
            -PassThru
        Write-RouterFileAtomic `
            -Path $pidFile `
            -Bytes ([Text.Encoding]::ASCII.GetBytes([string]$sub2apiProcess.Id))
    } finally {
        foreach ($key in $environment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $originalEnvironment[$key], 'Process')
        }
        $originalEnvironment.Clear()
        $environment.Clear()
    }
}

$sub2apiReady = $false
$deadline = [DateTime]::UtcNow.AddSeconds(120)
do {
    if ($null -ne $sub2apiProcess -and $sub2apiProcess.HasExited) {
        $sub2apiProcess.Refresh()
        throw "Sub2API exited during startup with exit code $($sub2apiProcess.ExitCode)."
    }
    if ((Test-Sub2ApiHealth -Uri "$sub2apiBaseUri/health") -and
        (Test-PostgresReadyStable -Password $pgPassword) -and
        (Test-Sub2ApiDatabaseReady -Password $pgPassword) -and
        (Get-RedisPing -Password $redisPassword) -eq 'PONG') {
        $sub2apiReady = $true
        break
    }
    Start-Sleep -Seconds 1
} while ([DateTime]::UtcNow -lt $deadline)

if (-not $sub2apiReady) {
    throw 'Sub2API or one of its authenticated dependencies did not become ready within 120 seconds. The application database must contain a readable active administrator; use the explicit repair flow if the data directory is incomplete.'
}

[void](Get-VerifiedLoopbackListener `
    -Port $sub2apiPort `
    -ExpectedPath $expectedSub2Api `
    -ServiceName 'Sub2API')

Write-RouterFileAtomic `
    -Path $networkFingerprintFile `
    -Bytes ([Text.Encoding]::ASCII.GetBytes($networkFingerprint))

$proxyPassword = $null
$proxySettings = $null
$routerConfig = $null
Write-Output "Codex Router is running at $sub2apiBaseUri"
} finally {
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
        $previousLifecycleLockMarker,
        'Process')
    Exit-RouterLifecycleLock -Lock $lifecycleLock
}
