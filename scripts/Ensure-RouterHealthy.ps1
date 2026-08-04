param(
    [switch]$ProbeOnly,
    [ValidateRange(1, 30)][int]$TimeoutSeconds = 4
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
$baseUri = Get-RouterBaseUri
$monitorLog = Join-Path $routerRoot 'logs\health-monitor.log'

function Write-MonitorEvent([string]$Message) {
    $directory = Split-Path -Parent $monitorLog
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    if ((Test-Path -LiteralPath $monitorLog) -and
        (Get-Item -LiteralPath $monitorLog).Length -gt 262144) {
        Move-Item -LiteralPath $monitorLog -Destination "$monitorLog.1" -Force
    }
    Add-Content `
        -LiteralPath $monitorLog `
        -Value "$(Get-Date -Format o) $Message" `
        -Encoding UTF8
}

function Test-RouterDeepHealth {
    $apiKey = $null
    $request = $null
    $response = $null
    try {
        $apiKey = Get-RouterCredential -Name 'LocalApiKey'
        $request = [Net.HttpWebRequest]::Create("$baseUri/v1/models")
        $request.Method = 'GET'
        $request.Proxy = $null
        $request.Timeout = $TimeoutSeconds * 1000
        $request.ReadWriteTimeout = $TimeoutSeconds * 1000
        $request.KeepAlive = $false
        $request.Headers['Authorization'] = "Bearer $apiKey"
        $response = [Net.HttpWebResponse]$request.GetResponse()
        return [int]$response.StatusCode -eq 200
    } catch {
        return $false
    } finally {
        if ($null -ne $response) { $response.Dispose() }
        $apiKey = $null
    }
}

if (Test-RouterDeepHealth) {
    Write-Output 'Router deep health: ready'
    exit 0
}
if ($ProbeOnly) {
    Write-Output 'Router deep health: unavailable'
    exit 2
}

Write-MonitorEvent 'Deep health probe failed; starting verified local recovery.'
try {
    & (Join-Path $PSScriptRoot 'Start-Router.ps1') -RepairUnhealthy | Out-Null
    if (-not (Test-RouterDeepHealth)) {
        throw 'Deep health probe still fails after recovery.'
    }
    Write-MonitorEvent 'Verified local recovery completed.'
    Write-Output 'Router deep health: recovered'
} catch {
    Write-MonitorEvent 'Verified local recovery failed; manual inspection is required.'
    throw
}
