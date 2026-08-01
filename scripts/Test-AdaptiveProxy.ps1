Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$python = (Get-Command python.exe -ErrorAction Stop).Source
$listenPort = 17898
$missingClashPort = 57997

foreach ($port in @($listenPort, $missingClashPort)) {
    if (Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        throw "Test port is already in use: $port"
    }
}

$process = Start-Process `
    -FilePath $python `
    -ArgumentList @(
        "$routerRoot\scripts\adaptive_proxy.py",
        '--listen-port', [string]$listenPort,
        '--clash-port', [string]$missingClashPort
    ) `
    -WorkingDirectory $routerRoot `
    -WindowStyle Hidden `
    -PassThru

try {
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $listener = Get-NetTCPConnection `
            -LocalAddress 127.0.0.1 `
            -LocalPort $listenPort `
            -State Listen `
            -ErrorAction SilentlyContinue
        if ($listener) { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)

    if (-not $listener) { throw 'The isolated adaptive proxy did not start.' }

    $targets = @(
        'https://chatgpt.com/backend-api/codex/models',
        'https://api.kimi.com/coding/v1/models',
        'https://openrouter.ai/api/v1/models',
        'https://api.430123.xyz/v1/models'
    )
    foreach ($target in $targets) {
        $httpCode = & curl.exe `
            --silent `
            --show-error `
            --output NUL `
            --write-out '%{http_code}' `
            --connect-timeout 8 `
            --max-time 20 `
            --proxy "http://127.0.0.1:$listenPort" `
            $target `
            2>$null
        [pscustomobject]@{
            Mode = 'No-Clash-Port-Direct'
            Target = ([Uri]$target).Host
            HttpCode = $httpCode
            CurlExit = $LASTEXITCODE
        }
    }
} finally {
    $running = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
    if ($running -and [string]::Equals($running.Path, $python, [StringComparison]::OrdinalIgnoreCase)) {
        Stop-Process -Id $running.Id -Force -ErrorAction SilentlyContinue
    }
}
