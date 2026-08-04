param(
    # Retained for command-line compatibility. Normal lifecycle calls still
    # defer while active requests are present.
    [ValidateRange(0, 300)][int]$DrainTimeoutSeconds = 30,
    [ValidateRange(1, 120)][int]$DependencyTimeoutSeconds = 15,
    # A deliberate full GUI exit uses this switch. Ownership and loopback
    # verification still apply, but active requests do not leave the portable
    # backend running after the window has closed.
    [switch]$Force,
    # A newer portable GUI can explicitly adopt the active services from an
    # older verified Codex-Router release during a deliberate full exit.
    [switch]$AdoptActivePortableOwner,
    [ValidateRange(1, 65535)][int]$RedisPort = 16379,
    [ValidateRange(1, 65535)][int]$PostgresPort = 15432
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot
$sub2apiBaseUri = Get-RouterBaseUri
$sub2apiPort = ([Uri]$sub2apiBaseUri).Port

function Get-VerifiedProcess {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][string]$ServiceName
    )
    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($null -eq $process) { return $null }
    $expected = [IO.Path]::GetFullPath($ExpectedPath)
    $actualPath = try { [string]$process.Path } catch { '' }
    if ([string]::IsNullOrWhiteSpace($actualPath) -or
        -not [IO.Path]::GetFullPath($actualPath).Equals(
            $expected,
            [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to stop an unexpected process recorded as $ServiceName."
    }
    return [pscustomobject]@{
        ProcessId = [int]$process.Id
        ExecutablePath = $actualPath
    }
}

function Get-VerifiedListenerProcess {
    param(
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][string]$ServiceName
    )
    $process = Get-LoopbackListenerProcess -Port $Port -ServiceName $ServiceName
    if ($null -eq $process) { return $null }
    return Get-VerifiedProcess `
        -ProcessId ([int]$process.ProcessId) `
        -ExpectedPath $ExpectedPath `
        -ServiceName $ServiceName
}

function Get-VerifiedPidFileProcess {
    param(
        [Parameter(Mandatory)][string]$PidFile,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][string]$ServiceName
    )
    if (-not (Test-Path -LiteralPath $PidFile -PathType Leaf)) { return $null }
    $pidLines = [IO.File]::ReadAllLines($PidFile)
    $savedPid = 0
    if ($pidLines.Count -eq 0 -or
        -not [int]::TryParse($pidLines[0].Trim(), [ref]$savedPid) -or
        $savedPid -le 0) {
        return $null
    }
    return Get-VerifiedProcess `
        -ProcessId $savedPid `
        -ExpectedPath $ExpectedPath `
        -ServiceName $ServiceName
}

function Test-CurrentPortableStackOwned {
    $ownedServices = @(
        [pscustomobject]@{
            PidFile = Join-Path $dataRoot 'pids\sub2api.pid'
            ExpectedPath = Join-Path $routerRoot 'app\sub2api.exe'
            ServiceName = 'Sub2API'
        },
        [pscustomobject]@{
            PidFile = Join-Path $dataRoot 'pids\redis.pid'
            ExpectedPath = Join-Path $routerRoot 'redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe'
            ServiceName = 'Redis'
        },
        [pscustomobject]@{
            PidFile = Join-Path $dataRoot 'postgres\postmaster.pid'
            ExpectedPath = Join-Path $routerRoot 'postgres\pgsql\bin\postgres.exe'
            ServiceName = 'PostgreSQL'
        }
    )
    foreach ($service in $ownedServices) {
        $ownedProcess = try {
            Get-VerifiedPidFileProcess `
                -PidFile $service.PidFile `
                -ExpectedPath $service.ExpectedPath `
                -ServiceName $service.ServiceName
        } catch {
            $null
        }
        if ($null -eq $ownedProcess) {
            return $false
        }
    }
    return $true
}

function Get-ProcessesByExecutablePath {
    param([Parameter(Mandatory)][string]$ExpectedPath)
    $expected = [IO.Path]::GetFullPath($ExpectedPath)
    $processName = [IO.Path]::GetFileNameWithoutExtension($expected)
    $matches = foreach ($process in @(Get-Process -Name $processName -ErrorAction SilentlyContinue)) {
        $actualPath = try { [string]$process.Path } catch { '' }
        if (-not [string]::IsNullOrWhiteSpace($actualPath) -and
            [IO.Path]::GetFullPath($actualPath).Equals(
                $expected,
                [StringComparison]::OrdinalIgnoreCase)) {
            [pscustomobject]@{
                ProcessId = [int]$process.Id
                ExecutablePath = $actualPath
            }
        }
    }
    return @($matches)
}

function Test-ManagedOAuthProcess {
    param(
        [Parameter(Mandatory)]$Process,
        [Parameter(Mandatory)][string]$ScriptPath
    )
    $expectedPowerShell = [IO.Path]::GetFullPath(
        "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe")
    if ([string]::IsNullOrWhiteSpace([string]$Process.ExecutablePath) -or
        -not [IO.Path]::GetFullPath([string]$Process.ExecutablePath).Equals(
            $expectedPowerShell,
            [StringComparison]::OrdinalIgnoreCase)) {
        return $false
    }
    $commandLine = [string]$Process.CommandLine
    if ([string]::IsNullOrWhiteSpace($commandLine)) { return $false }
    $escapedScript = [Regex]::Escape([IO.Path]::GetFullPath($ScriptPath))
    $fileArgument = '(?i)(?:^|\s)-File(?:\s+|:)(?:"' + $escapedScript + '"|' + $escapedScript + ')(?:\s|$)'
    return [Regex]::IsMatch($commandLine, $fileArgument)
}

function Stop-ManagedOAuthProcesses {
    $oauthScript = [IO.Path]::GetFullPath((Join-Path $routerRoot 'scripts\Start-ChatGPTOAuth.ps1'))
    $oauthPidFile = Join-Path $dataRoot 'pids\oauth.pid'
    $candidateProcesses = @()
    if (Test-Path -LiteralPath $oauthPidFile -PathType Leaf) {
        $savedPid = 0
        if ([int]::TryParse([IO.File]::ReadAllText($oauthPidFile).Trim(), [ref]$savedPid) -and
            $savedPid -gt 0) {
            $savedProcess = Get-CimInstance Win32_Process -Filter "ProcessId=$savedPid" -ErrorAction SilentlyContinue
            if ($null -ne $savedProcess -and
                (Test-ManagedOAuthProcess -Process $savedProcess -ScriptPath $oauthScript)) {
                $candidateProcesses += $savedProcess
            }
        }
    }
    $candidateProcesses += @(Get-CimInstance `
        -ClassName Win32_Process `
        -Filter "Name='powershell.exe'" `
        -ErrorAction SilentlyContinue | Where-Object {
            Test-ManagedOAuthProcess -Process $_ -ScriptPath $oauthScript
        })

    $oauthPids = @($candidateProcesses | Sort-Object ProcessId -Unique |
        ForEach-Object { [int]$_.ProcessId })
    foreach ($oauthPid in $oauthPids) {
        Stop-Process -Id $oauthPid -Force -ErrorAction SilentlyContinue
    }
    $oauthTimeoutSeconds = if ($Force) { 1 } else { 2 }
    if (-not (Wait-ProcessSetExit -ProcessIds $oauthPids -TimeoutSeconds $oauthTimeoutSeconds)) {
        throw 'One or more managed OAuth processes did not exit after termination.'
    }
    Remove-Item -LiteralPath $oauthPidFile -Force -ErrorAction SilentlyContinue
}

function Stop-ProcessTree {
    param([Parameter(Mandatory)][int]$ProcessId)
    $taskKill = "$env:SystemRoot\System32\taskkill.exe"
    if (Test-Path -LiteralPath $taskKill -PathType Leaf) {
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $taskKill
        $startInfo.Arguments = "/PID $ProcessId /T /F"
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $startInfo
        try {
            if ($process.Start() -and -not $process.WaitForExit(2000)) {
                Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
                [void]$process.WaitForExit(1000)
            }
        } catch {
            # Fall through to the verified single-process kill below.
        } finally {
            $process.Dispose()
        }
    }
    if ($null -ne (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $ProcessId -Force -ErrorAction SilentlyContinue
    }
}

function Test-TcpListenerPort {
    param([Parameter(Mandatory)][int]$Port)
    return @([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() |
        Where-Object { $_.Port -eq $Port }).Count -gt 0
}

function Assert-ManagedServiceStopped {
    param(
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$ServiceName,
        [Parameter(Mandatory)][string]$ExpectedPath,
        [int[]]$ProcessIds = @()
    )
    foreach ($processId in @($ProcessIds | Where-Object { $_ -gt 0 } | Select-Object -Unique)) {
        if ($null -ne (Get-Process -Id $processId -ErrorAction SilentlyContinue)) {
            throw "$ServiceName process $processId is still running after shutdown."
        }
    }
    if (Test-TcpListenerPort -Port $Port) {
        throw "$ServiceName is still listening on port $Port after shutdown."
    }
    $remaining = @(Get-ProcessesByExecutablePath -ExpectedPath $ExpectedPath)
    if ($remaining.Count -gt 0) {
        throw "$ServiceName still has $($remaining.Count) managed process(es) after shutdown."
    }
}

function Stop-RemainingManagedProcesses {
    param(
        [Parameter(Mandatory)][string]$ExpectedPath,
        [Parameter(Mandatory)][string]$ServiceName
    )
    $remaining = @(Get-ProcessesByExecutablePath -ExpectedPath $ExpectedPath)
    $forcedExitTimeoutSeconds = if ($Force) { 1 } else { 3 }
    foreach ($process in $remaining) {
        $managedPid = [int]$process.ProcessId
        Stop-Process -Id $managedPid -Force -ErrorAction SilentlyContinue
    }
    $managedPids = @($remaining | ForEach-Object { [int]$_.ProcessId })
    if (-not (Wait-ProcessSetExit -ProcessIds $managedPids -TimeoutSeconds $forcedExitTimeoutSeconds)) {
        throw "$ServiceName processes did not exit after forced termination."
    }
}

function Get-LoopbackListenerProcess {
    param(
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$ServiceName
    )
    if (-not (Test-TcpListenerPort -Port $Port)) { return $null }
    $listeners = @(Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)
    if ($listeners.Count -eq 0) { return $null }
    if (@($listeners | Where-Object { $_.LocalAddress -ne '127.0.0.1' }).Count -gt 0) {
        throw "$ServiceName has a non-loopback listener; refusing to manage it."
    }
    $processIds = @($listeners | Select-Object -ExpandProperty OwningProcess -Unique)
    if ($processIds.Count -ne 1) { throw "$ServiceName has an ambiguous listener owner." }
    $process = Get-Process -Id ([int]$processIds[0]) -ErrorAction SilentlyContinue
    if ($null -eq $process) { throw "$ServiceName listener owner disappeared during verification." }
    $actualPath = try { [string]$process.Path } catch { '' }
    return [pscustomobject]@{
        ProcessId = [int]$process.Id
        ExecutablePath = $actualPath
    }
}

function Resolve-PortableRouterOwnerRoot {
    param(
        [Parameter(Mandatory)]$Process,
        [Parameter(Mandatory)][string]$RelativeExecutable,
        [Parameter(Mandatory)][string]$ComponentName,
        [Parameter(Mandatory)][string]$ServiceName
    )
    if ([string]::IsNullOrWhiteSpace([string]$Process.ExecutablePath)) {
        throw "$ServiceName listener owner has no executable path; refusing portable adoption."
    }
    $executablePath = [IO.Path]::GetFullPath([string]$Process.ExecutablePath)
    $relativeWindows = $RelativeExecutable.Replace('/', [IO.Path]::DirectorySeparatorChar)
    $suffix = [string][IO.Path]::DirectorySeparatorChar + $relativeWindows
    if (-not $executablePath.EndsWith($suffix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$ServiceName listener is not from a recognized Codex-Router portable layout."
    }
    $ownerRoot = [IO.Path]::GetFullPath($executablePath.Substring(0, $executablePath.Length - $suffix.Length))
    $manifestPath = Join-Path $ownerRoot 'dependency-manifest.json'
    $releaseManifestPath = Join-Path $ownerRoot 'release-manifest.json'
    $ownerStopScript = Join-Path $ownerRoot 'scripts\Stop-Router.ps1'
    foreach ($requiredPath in @($manifestPath, $releaseManifestPath, $ownerStopScript)) {
        if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
            throw "$ServiceName listener owner is missing portable release verification metadata."
        }
    }

    try {
        $dependencyManifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
        $releaseManifest = @([IO.File]::ReadAllText($releaseManifestPath) | ConvertFrom-Json)
    } catch {
        throw "$ServiceName listener owner has invalid portable release verification metadata."
    }
    $componentMatches = @($dependencyManifest.components | Where-Object {
        [string]$_.name -eq $ComponentName -and
        ([string]$_.executable).Replace('\', '/').Equals($RelativeExecutable.Replace('\', '/'), [StringComparison]::OrdinalIgnoreCase)
    })
    if ($componentMatches.Count -ne 1) {
        throw "$ServiceName listener owner is not declared by its portable dependency manifest."
    }
    $expectedExecutableHash = [string]$componentMatches[0].executableSha256
    $actualExecutableHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
    if ([string]::IsNullOrWhiteSpace($expectedExecutableHash) -or
        -not $actualExecutableHash.Equals($expectedExecutableHash, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$ServiceName executable does not match its portable dependency manifest."
    }

    $stopRelativePath = 'scripts/Stop-Router.ps1'
    $stopEntries = @($releaseManifest | Where-Object {
        ([string]$_.path).Replace('\', '/').Equals($stopRelativePath, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($stopEntries.Count -ne 1) {
        throw 'The active portable release does not declare its shutdown helper.'
    }
    $expectedStopHash = [string]$stopEntries[0].sha256
    $actualStopHash = (Get-FileHash -LiteralPath $ownerStopScript -Algorithm SHA256).Hash
    if ([string]::IsNullOrWhiteSpace($expectedStopHash) -or
        -not $actualStopHash.Equals($expectedStopHash, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The active portable release shutdown helper failed integrity verification.'
    }
    return $ownerRoot
}

function Get-ActivePortableOwnerRoot {
    $listenerSpecs = @(
        [pscustomobject]@{ Port = $sub2apiPort; Relative = 'app/sub2api.exe'; Component = 'Sub2API'; Service = 'Sub2API' },
        [pscustomobject]@{ Port = $RedisPort; Relative = 'redis/Redis-8.10.0-Windows-x64-msys2/redis-server.exe'; Component = 'Redis'; Service = 'Redis' },
        [pscustomobject]@{ Port = $PostgresPort; Relative = 'postgres/pgsql/bin/postgres.exe'; Component = 'PostgreSQL'; Service = 'PostgreSQL' }
    )
    $foreignOwner = $null
    $currentOwnerSeen = $false
    foreach ($spec in $listenerSpecs) {
        $process = Get-LoopbackListenerProcess -Port $spec.Port -ServiceName $spec.Service
        if ($null -eq $process) { continue }
        $expectedPath = [IO.Path]::GetFullPath((Join-Path $routerRoot $spec.Relative))
        $actualPath = if ([string]::IsNullOrWhiteSpace([string]$process.ExecutablePath)) {
            ''
        } else {
            [IO.Path]::GetFullPath([string]$process.ExecutablePath)
        }
        if ($actualPath.Equals($expectedPath, [StringComparison]::OrdinalIgnoreCase)) {
            $currentOwnerSeen = $true
            continue
        }
        $candidate = Resolve-PortableRouterOwnerRoot `
            -Process $process `
            -RelativeExecutable $spec.Relative `
            -ComponentName $spec.Component `
            -ServiceName $spec.Service
        if ($null -ne $foreignOwner -and
            -not $foreignOwner.Equals($candidate, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Router listeners belong to multiple portable releases; refusing automatic adoption.'
        }
        $foreignOwner = $candidate
    }
    if ($currentOwnerSeen -and $null -ne $foreignOwner) {
        throw 'Router listeners are split across portable releases; refusing automatic adoption.'
    }
    return $foreignOwner
}

function Invoke-PortableOwnerShutdown {
    param([Parameter(Mandatory)][string]$OwnerRoot)
    $ownerStopScript = Join-Path $OwnerRoot 'scripts\Stop-Router.ps1'
    if ($ownerStopScript.Contains('"')) { throw 'The active portable release path is invalid.' }
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $startInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$ownerStopScript`" -Force -DependencyTimeoutSeconds 3 -RedisPort $RedisPort -PostgresPort $PostgresPort"
    $startInfo.WorkingDirectory = $OwnerRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { throw 'Could not start the verified portable shutdown helper.' }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        # Leave time for the outer GUI helper to reap this process tree and
        # report a deterministic result inside its own 12-second budget.
        if (-not $process.WaitForExit(9000)) {
            Stop-ProcessTree -ProcessId $process.Id
            [void]$process.WaitForExit(2000)
            throw 'The verified portable shutdown helper exceeded its time budget.'
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult().Trim()
        $stderr = $stderrTask.GetAwaiter().GetResult().Trim()
        if ($process.ExitCode -ne 0) {
            $detail = if ([string]::IsNullOrWhiteSpace($stderr)) { $stdout } else { $stderr }
            throw "The verified portable shutdown helper failed with exit code $($process.ExitCode): $detail"
        }
    } finally {
        $process.Dispose()
    }
}

function Wait-ProcessExit {
    param([Parameter(Mandatory)][int]$ProcessId, [Parameter(Mandatory)][int]$TimeoutSeconds)
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if ($null -eq (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Wait-ProcessSetExit {
    param([int[]]$ProcessIds = @(), [Parameter(Mandatory)][int]$TimeoutSeconds)
    $remaining = @($ProcessIds | Where-Object { $_ -gt 0 } | Select-Object -Unique)
    if ($remaining.Count -eq 0) { return $true }
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $remaining = @($remaining | Where-Object {
            $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue)
        })
        if ($remaining.Count -eq 0) { return $true }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Invoke-NativeQuiet {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$ArgumentList
    )
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        & $FilePath @ArgumentList *> $null
        return [int]$LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
}

function Invoke-RedisShutdown {
    param(
        [Parameter(Mandatory)][string]$Password,
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][int]$TimeoutSeconds,
        [Parameter(Mandatory)][bool]$SaveSnapshot
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-cli.exe"
    $shutdownMode = if ($SaveSnapshot) { 'save' } else { 'nosave' }
    $startInfo.Arguments = "-h 127.0.0.1 -p $Port shutdown $shutdownMode"
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.EnvironmentVariables['REDISCLI_AUTH'] = $Password
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { return $false }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            Stop-ProcessTree -ProcessId $process.Id
            [void]$process.WaitForExit(2000)
            return $false
        }
        [void]$stdoutTask.GetAwaiter().GetResult()
        [void]$stderrTask.GetAwaiter().GetResult()
        return $process.ExitCode -eq 0
    } catch {
        return $false
    } finally {
        $process.Dispose()
    }
}

Stop-ManagedOAuthProcesses

if ($Force -and $AdoptActivePortableOwner) {
    # The normal case is a complete stack owned by this release. PID + image
    # verification is enough to skip three comparatively expensive listener
    # and release-manifest ownership scans. Any incomplete/mixed stack still takes the full
    # portable-owner verification path below.
    if (-not (Test-CurrentPortableStackOwned)) {
        $activePortableOwner = Get-ActivePortableOwnerRoot
        if ($null -ne $activePortableOwner) {
            Invoke-PortableOwnerShutdown -OwnerRoot $activePortableOwner
            Write-Output "Codex Router services from the active verified portable release are stopped."
            return
        }
    }
}

$effectiveDependencyTimeoutSeconds = if ($Force) {
    [Math]::Min($DependencyTimeoutSeconds, 1)
} else {
    $DependencyTimeoutSeconds
}
$lifecycleLockTimeoutMilliseconds = if ($Force) { 1500 } else { 10000 }
$lifecycleLock = Enter-RouterLifecycleLock `
    -RouterRoot $routerRoot `
    -TimeoutMilliseconds $lifecycleLockTimeoutMilliseconds `
    -Operation 'Stop Router'
$previousLifecycleLockMarker = [Environment]::GetEnvironmentVariable(
    'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
    'Process')
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_LIFECYCLE_LOCK_HELD', [string]$PID, 'Process')
try {
$sub2apiPidFile = Join-Path $dataRoot 'pids\sub2api.pid'
$sub2apiProcess = $null
$sub2apiPid = 0
if (Test-Path -LiteralPath $sub2apiPidFile) {
    $savedPid = 0
    if ([int]::TryParse([IO.File]::ReadAllText($sub2apiPidFile).Trim(), [ref]$savedPid)) {
        $sub2apiProcess = Get-VerifiedProcess `
            -ProcessId $savedPid `
            -ExpectedPath "$routerRoot\app\sub2api.exe" `
            -ServiceName 'Sub2API'
    }
}
if ($null -eq $sub2apiProcess) {
    $sub2apiProcess = Get-VerifiedListenerProcess `
        -Port $sub2apiPort `
        -ExpectedPath "$routerRoot\app\sub2api.exe" `
        -ServiceName 'Sub2API'
}

if ($null -ne $sub2apiProcess -and -not $Force) {
    $sub2apiPid = [int]$sub2apiProcess.ProcessId
    Assert-RouterServiceInterruptionAllowed `
        -ProcessId $sub2apiPid `
        -Port $sub2apiPort `
        -Operation 'Stop Router'
}

if ($null -ne $sub2apiProcess) {
    $sub2apiPid = [int]$sub2apiProcess.ProcessId
    Stop-Process -Id $sub2apiPid -Force -ErrorAction Stop
    $sub2apiExitTimeoutSeconds = if ($Force) { 1 } else { 5 }
    if (-not (Wait-ProcessExit -ProcessId $sub2apiPid -TimeoutSeconds $sub2apiExitTimeoutSeconds)) {
        throw 'Sub2API did not exit after termination.'
    }
}
Remove-Item -LiteralPath $sub2apiPidFile -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath (Join-Path $dataRoot 'pids\sub2api-network.hmac') -Force -ErrorAction SilentlyContinue
Stop-RemainingManagedProcesses -ExpectedPath "$routerRoot\app\sub2api.exe" -ServiceName 'Sub2API'
Assert-ManagedServiceStopped `
    -Port $sub2apiPort `
    -ServiceName 'Sub2API' `
    -ExpectedPath "$routerRoot\app\sub2api.exe" `
    -ProcessIds @($sub2apiPid)

$redisPath = "$routerRoot\redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe"
$redisPidFile = Join-Path $dataRoot 'pids\redis.pid'
$redisProcess = Get-VerifiedPidFileProcess `
    -PidFile $redisPidFile `
    -ExpectedPath $redisPath `
    -ServiceName 'Redis'
if ($null -eq $redisProcess) {
    $redisProcess = Get-VerifiedListenerProcess `
        -Port $RedisPort `
        -ExpectedPath $redisPath `
        -ServiceName 'Redis'
}
$redisPid = 0
if ($null -ne $redisProcess) {
    $redisPid = [int]$redisProcess.ProcessId
    $redisPassword = Get-RouterCredential -Name 'RedisPassword'
    try {
        $redisShutdownRequested = Invoke-RedisShutdown `
            -Password $redisPassword `
            -Port $RedisPort `
            -TimeoutSeconds $effectiveDependencyTimeoutSeconds `
            -SaveSnapshot (-not $Force)
    } finally {
        $redisPassword = $null
    }
    if (-not (Wait-ProcessExit -ProcessId $redisPid -TimeoutSeconds $effectiveDependencyTimeoutSeconds)) {
        if (-not $redisShutdownRequested) {
            Write-Warning 'Redis did not accept the authenticated shutdown request; forcing shutdown.'
        } else {
            Write-Warning 'Redis did not finish its bounded SAVE shutdown; forcing shutdown.'
        }
        Stop-Process -Id $redisPid -Force -ErrorAction Stop
        $redisForcedExitTimeoutSeconds = if ($Force) { 1 } else { 3 }
        if (-not (Wait-ProcessExit -ProcessId $redisPid -TimeoutSeconds $redisForcedExitTimeoutSeconds)) {
            throw 'Redis did not exit after forced termination.'
        }
    }
}
Stop-RemainingManagedProcesses -ExpectedPath $redisPath -ServiceName 'Redis'
Assert-ManagedServiceStopped `
    -Port $RedisPort `
    -ServiceName 'Redis' `
    -ExpectedPath $redisPath `
    -ProcessIds @($redisPid)
Remove-Item -LiteralPath $redisPidFile -Force -ErrorAction SilentlyContinue

$pgCtl = "$routerRoot\postgres\pgsql\bin\pg_ctl.exe"
$pgData = Join-Path $dataRoot 'postgres'
$postgresPath = "$routerRoot\postgres\pgsql\bin\postgres.exe"
$postgresPidFile = Join-Path $pgData 'postmaster.pid'
$postgresProcess = Get-VerifiedPidFileProcess `
    -PidFile $postgresPidFile `
    -ExpectedPath $postgresPath `
    -ServiceName 'PostgreSQL'
if ($null -eq $postgresProcess) {
    $postgresProcess = Get-VerifiedListenerProcess `
        -Port $PostgresPort `
        -ExpectedPath $postgresPath `
        -ServiceName 'PostgreSQL'
}
$postgresPid = 0
if ($null -ne $postgresProcess) {
    $postgresPid = [int]$postgresProcess.ProcessId
    $initialShutdownMode = if ($Force) { 'immediate' } else { 'smart' }
    $pgStopExitCode = Invoke-NativeQuiet `
        -FilePath $pgCtl `
        -ArgumentList @('stop', '-D', $pgData, '-s', '-m', $initialShutdownMode, '-w', '-t', [string]$effectiveDependencyTimeoutSeconds)
    if ($pgStopExitCode -ne 0 -and -not $Force) {
        Write-Warning "PostgreSQL $initialShutdownMode shutdown exceeded its budget; switching to immediate shutdown."
        $pgImmediateStopExitCode = Invoke-NativeQuiet `
            -FilePath $pgCtl `
            -ArgumentList @('stop', '-D', $pgData, '-s', '-m', 'immediate', '-w', '-t', '3')
        if ($pgImmediateStopExitCode -ne 0 -and
            $null -ne (Get-Process -Id $postgresPid -ErrorAction SilentlyContinue)) {
            Stop-Process -Id $postgresPid -Force -ErrorAction Stop
        }
    } elseif ($pgStopExitCode -ne 0 -and
        $null -ne (Get-Process -Id $postgresPid -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $postgresPid -Force -ErrorAction Stop
    }
    $postgresExitTimeoutSeconds = if ($Force) { 1 } else { 3 }
    if (-not (Wait-ProcessExit -ProcessId $postgresPid -TimeoutSeconds $postgresExitTimeoutSeconds)) {
        throw 'PostgreSQL did not exit after bounded shutdown.'
    }
}
Stop-RemainingManagedProcesses -ExpectedPath $postgresPath -ServiceName 'PostgreSQL'
Assert-ManagedServiceStopped `
    -Port $PostgresPort `
    -ServiceName 'PostgreSQL' `
    -ExpectedPath $postgresPath `
    -ProcessIds @($postgresPid)

Write-Output 'Codex Router is stopped.'
} finally {
    [Environment]::SetEnvironmentVariable(
        'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
        $previousLifecycleLockMarker,
        'Process')
    Exit-RouterLifecycleLock -Lock $lifecycleLock
}
