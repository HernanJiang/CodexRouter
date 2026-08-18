param(
    [switch]$RepairUnhealthy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force

# 2.0: the native binary owns the Router Host + CLIProxyAPI lifecycle
# (loopback port ownership, verified termination, active-request deferral,
# and bounded repair). This script is a thin console-friendly wrapper so the
# GUI button, autostart entry, installer, and manual recovery all share one
# implementation.
$native = Join-Path $routerRoot 'Codex-Router.exe'
if (-not (Test-Path -LiteralPath $native -PathType Leaf)) {
    throw "Codex Router executable is missing: $native"
}

$arguments = @('--ensure-router-services', "--router-root=$routerRoot")
if ($RepairUnhealthy) { $arguments += '--repair-unhealthy' }


function Read-SharedText {
    param([Parameter(Mandatory)][string]$Path)
    # The long-lived Router Host inherits the redirected stdout handle of the
    # launch command, so ReadAllText (FileShare.Read) can fail while the host
    # stays alive. Open with read/write sharing to read the completed content.
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try {
        $reader = [IO.StreamReader]::new($stream)
        try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally { $stream.Dispose() }
}

$stdoutFile = Join-Path $env:TEMP ("codex-router-start-" + [Guid]::NewGuid().ToString('N') + '.out.log')
$stderrFile = Join-Path $env:TEMP ("codex-router-start-" + [Guid]::NewGuid().ToString('N') + '.err.log')
$process = $null
try {
    $process = Start-Process -FilePath $native -ArgumentList $arguments -NoNewWindow -PassThru -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile
    # The release GUI binary is a windows-subsystem executable. When the native
    # ensure command starts the Router Host it spawns a long-lived child, and
    # Start-Process -Wait never returns for such a parent (PowerShell waits on
    # the inherited console/stream state). Poll HasExited instead; the child
    # processes are intentionally detached and outlive this wrapper.
    $deadline = (Get-Date).AddSeconds(150)
    while (-not $process.HasExited) {
        if ((Get-Date) -gt $deadline) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            throw 'Router startup timed out waiting for the native lifecycle command.'
        }
        Start-Sleep -Milliseconds 250
        $process.Refresh()
    }
    Start-Sleep -Milliseconds 300
    $stdout = Read-SharedText -Path $stdoutFile
    $stderr = Read-SharedText -Path $stderrFile
} finally {
    Remove-Item -LiteralPath $stdoutFile, $stderrFile -Force -ErrorAction SilentlyContinue
}

if ($null -eq $process -or $process.ExitCode -ne 0) {
    $detail = @($stdout.Trim(), $stderr.Trim()) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    foreach ($line in $detail) { Write-Warning $line }
    $code = if ($null -ne $process) { $process.ExitCode } else { -1 }
    throw "Router startup failed with exit code $code."
}

try {
    $status = $stdout | ConvertFrom-Json
    foreach ($service in @($status.services)) {
        Write-Output ("{0}: running={1} ready={2} endpoint={3}" -f $service.component, $service.running, $service.ready, $service.endpoint)
    }
} catch {
    # The JSON status is diagnostic sugar only; the exit code already
    # confirmed success.
}

Write-Output "Codex Router is running at $(Get-RouterBaseUri)"