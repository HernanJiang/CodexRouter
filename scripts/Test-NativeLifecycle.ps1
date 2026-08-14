[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$RouterExecutable,

    [Parameter(Mandatory)]
    [int]$ProtectedProcessId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Get-FreeTcpPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $listener.Start()
        return ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    } finally {
        $listener.Stop()
    }
}

function Assert-ProtectedRouter {
    param([string]$ExpectedPath)

    $process = Get-Process -Id $ProtectedProcessId -ErrorAction Stop
    if (-not $process.Responding) {
        throw "Protected Codex-Router process $ProtectedProcessId is not responding."
    }
    if (-not [string]::Equals($process.Path, $ExpectedPath, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Protected Codex-Router process path changed unexpectedly."
    }
}

function Remove-IsolatedTestRoot {
    param([Parameter(Mandatory)][string]$Path)

    $resolvedRoot = [IO.Path]::GetFullPath($Path)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if (-not $resolvedRoot.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Split-Path -Leaf $resolvedRoot).StartsWith('codex-router-native-lifecycle-test-', [StringComparison]::Ordinal)) {
        throw 'Refusing to clean an unexpected lifecycle test directory.'
    }
    $ownedProcesses = @(Get-CimInstance Win32_Process | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_.ExecutablePath) -and
        $_.ExecutablePath.StartsWith($resolvedRoot, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($ownedProcesses.Count -ne 0) {
        throw "Refusing to clean a lifecycle test directory with running processes: $resolvedRoot"
    }
    if (Test-Path -LiteralPath $resolvedRoot) {
        Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
    }
}

function Invoke-NativeLifecycle {
    param([Parameter(Mandatory)][string[]]$Arguments)

    $outputPath = Join-Path $testRoot 'native-cli-output.json'
    $previousOutput = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CLI_OUTPUT', 'Process')
    try {
        Remove-Item -LiteralPath $outputPath -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('CODEX_ROUTER_CLI_OUTPUT', $outputPath, 'Process')
        if ($Arguments | Where-Object { $_.Contains('"') }) {
            throw 'Native lifecycle test arguments must not contain quotes.'
        }
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $testExecutable
        $startInfo.Arguments = ($Arguments | ForEach-Object {
            if ($_ -match '\s') { '"' + $_ + '"' } else { $_ }
        }) -join ' '
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $process = [Diagnostics.Process]::Start($startInfo)
        if (-not $process.WaitForExit(240000)) {
            $process.Kill()
            throw 'Native lifecycle command exceeded its four-minute test budget.'
        }
        $exitCode = $process.ExitCode
        if ($exitCode -ne 0) {
            throw "Native lifecycle command failed with exit $exitCode."
        }
        return (Get-Content -LiteralPath $outputPath -Raw | ConvertFrom-Json)
    } finally {
        [Environment]::SetEnvironmentVariable('CODEX_ROUTER_CLI_OUTPUT', $previousOutput, 'Process')
        Remove-Item -LiteralPath $outputPath -Force -ErrorAction SilentlyContinue
    }
}

$sourceRoot = Split-Path -Parent $PSScriptRoot
$protected = Get-Process -Id $ProtectedProcessId -ErrorAction Stop
$protectedPath = $protected.Path
Assert-ProtectedRouter -ExpectedPath $protectedPath
Get-ChildItem ([IO.Path]::GetTempPath()) -Directory -Filter 'codex-router-native-lifecycle-test-*' |
    ForEach-Object { Remove-IsolatedTestRoot -Path $_.FullName }

$testRoot = Join-Path ([IO.Path]::GetTempPath()) (
    'codex-router-native-lifecycle-test-' + [guid]::NewGuid().ToString('N'))
$userDataRoot = Join-Path $testRoot 'UserData'
$testExecutable = Join-Path $testRoot 'Codex-Router.exe'
$started = $false
$previousEnvironment = @{}
foreach ($name in @(
    'CODEX_ROUTER_USER_DATA_ROOT',
    'CODEX_ROUTER_POSTGRES_PORT',
    'CODEX_ROUTER_REDIS_PORT',
    'CODEX_ROUTER_PORTABLE_STATE'
)) {
    $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

try {
    [IO.Directory]::CreateDirectory($testRoot) | Out-Null
    foreach ($relative in @(
        'app',
        'redis',
        'postgres\pgsql\bin',
        'postgres\pgsql\lib',
        'postgres\pgsql\share',
        'config'
    )) {
        $source = Join-Path $sourceRoot $relative
        $destination = Join-Path $testRoot $relative
        [IO.Directory]::CreateDirectory((Split-Path -Parent $destination)) | Out-Null
        Copy-Item -LiteralPath $source -Destination $destination -Recurse -Force
    }
    Copy-Item -LiteralPath $RouterExecutable -Destination $testExecutable -Force

    $postgresPort = Get-FreeTcpPort
    $redisPort = Get-FreeTcpPort
    $sub2apiPort = Get-FreeTcpPort
    $ports = @($postgresPort, $redisPort, $sub2apiPort)
    if (@($ports | Select-Object -Unique).Count -ne 3) {
        throw 'Could not reserve three distinct loopback ports for the lifecycle test.'
    }

    [IO.Directory]::CreateDirectory($userDataRoot) | Out-Null
    $config = [ordered]@{
        version = 'native-lifecycle-test'
        deploy = [ordered]@{
            sub2apiHost = "http://127.0.0.1:$sub2apiPort"
            startWithWindows = $false
        }
        proxy = [ordered]@{
            autoDetect = $false
            enabled = $false
        }
        models = @()
    }
    $config | ConvertTo-Json -Depth 8 | Set-Content `
        -LiteralPath (Join-Path $userDataRoot 'codex-router-config.json') `
        -Encoding utf8NoBOM

    [Environment]::SetEnvironmentVariable('CODEX_ROUTER_USER_DATA_ROOT', $userDataRoot, 'Process')
    [Environment]::SetEnvironmentVariable('CODEX_ROUTER_POSTGRES_PORT', [string]$postgresPort, 'Process')
    [Environment]::SetEnvironmentVariable('CODEX_ROUTER_REDIS_PORT', [string]$redisPort, 'Process')
    [Environment]::SetEnvironmentVariable('CODEX_ROUTER_PORTABLE_STATE', $null, 'Process')

    $ensureArguments = @(
        '--ensure-router-services',
        '--repair-unhealthy',
        "--router-root=$testRoot"
    )
    $first = Invoke-NativeLifecycle -Arguments $ensureArguments
    $started = $true
    $firstServices = @($first.services)
    if ($firstServices.Count -ne 3 -or @($firstServices | Where-Object { -not $_.running -or -not $_.ready }).Count -ne 0) {
        throw 'Native lifecycle did not report all three isolated services as running and ready.'
    }

    $second = Invoke-NativeLifecycle -Arguments $ensureArguments
    $firstPids = @($firstServices.processId | Sort-Object)
    $secondPids = @($second.services.processId | Sort-Object)
    if (($firstPids -join ',') -ne ($secondPids -join ',')) {
        throw 'Repeated native lifecycle start replaced a healthy isolated service.'
    }

    $status = Invoke-NativeLifecycle -Arguments @('--router-status', "--router-root=$testRoot")
    if (@($status.services | Where-Object { -not $_.running -or -not $_.ready }).Count -ne 0) {
        throw 'Native lifecycle status did not pass all authenticated readiness checks.'
    }

    $stopped = Invoke-NativeLifecycle -Arguments @(
        '--stop-router-services',
        '--force',
        "--router-root=$testRoot"
    )
    $started = $false
    if (@($stopped.services | Where-Object { $_.running }).Count -ne 0) {
        throw 'Native lifecycle left an isolated service running after forced stop.'
    }

    Assert-ProtectedRouter -ExpectedPath $protectedPath
    [pscustomobject][ordered]@{
        Result = 'PASS'
        FirstStartProcessIds = $firstPids
        RepeatedStartProcessIds = $secondPids
        PostgreSqlPort = $postgresPort
        RedisPort = $redisPort
        Sub2ApiPort = $sub2apiPort
        ProtectedProcessId = $ProtectedProcessId
    } | ConvertTo-Json -Compress
} finally {
    if ($started -and (Test-Path -LiteralPath $testExecutable -PathType Leaf)) {
        try {
            [void](Invoke-NativeLifecycle -Arguments @(
                '--stop-router-services',
                '--force',
                "--router-root=$testRoot"
            ))
        } catch {
            Write-Warning 'Could not stop the isolated native lifecycle after a failed test.'
        }
    }
    foreach ($entry in $previousEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    Remove-IsolatedTestRoot -Path $testRoot
    Assert-ProtectedRouter -ExpectedPath $protectedPath
}
