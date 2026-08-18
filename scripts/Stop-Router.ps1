param(
    # Retained for command-line compatibility. The native lifecycle defers a
    # normal stop while the Router Host still has active Established
    # connections; there is no separate drain loop in 2.0.
    [ValidateRange(0, 300)][int]$DrainTimeoutSeconds = 30,
    [ValidateRange(1, 120)][int]$DependencyTimeoutSeconds = 15,
    # A deliberate full GUI exit uses this switch. Ownership and loopback
    # verification still apply, but active requests do not leave the stack
    # running after the window has closed.
    [switch]$Force,
    # Retained for compatibility with older callers; the 2.0 stack has no
    # adoptable sibling-owner scenario.
    [switch]$AdoptActivePortableOwner,
    [ValidateRange(1, 65535)][int]$RedisPort = 16379,
    [ValidateRange(1, 65535)][int]$PostgresPort = 15432
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot

# 2.0: the native binary owns shutdown of Router Host + CLIProxyAPI
# (verified loopback ownership, active-connection deferral, Job Object
# cleanup of the CLI child, and stale pid bookkeeping).
$native = Join-Path $routerRoot 'Codex-Router.exe'
if (-not (Test-Path -LiteralPath $native -PathType Leaf)) {
    throw "Codex Router executable is missing: $native"
}

$arguments = @('--stop-router-services', "--router-root=$routerRoot")
if ($Force) { $arguments += '--force' }

$stdoutFile = Join-Path $env:TEMP ("codex-router-stop-" + [Guid]::NewGuid().ToString('N') + '.out.log')
$stderrFile = Join-Path $env:TEMP ("codex-router-stop-" + [Guid]::NewGuid().ToString('N') + '.err.log')
$process = $null
try {
    $process = Start-Process -FilePath $native -ArgumentList $arguments -NoNewWindow -Wait -PassThru -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile
    $stdout = if (Test-Path -LiteralPath $stdoutFile) { [IO.File]::ReadAllText($stdoutFile) } else { '' }
    $stderr = if (Test-Path -LiteralPath $stderrFile) { [IO.File]::ReadAllText($stderrFile) } else { '' }
} finally {
    Remove-Item -LiteralPath $stdoutFile, $stderrFile -Force -ErrorAction SilentlyContinue
}

if ($null -eq $process -or $process.ExitCode -ne 0) {
    $detail = @($stdout.Trim(), $stderr.Trim()) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    foreach ($line in $detail) { Write-Warning $line }
    $code = if ($null -ne $process) { $process.ExitCode } else { -1 }
    throw "Router shutdown failed with exit code $code."
}

Write-Output 'Codex Router is stopped.'