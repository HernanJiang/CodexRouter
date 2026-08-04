Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$python = (Get-Command python.exe -ErrorAction Stop).Source
$listenPort = 17898
$originPort = 57996
$missingClashPort = 57997
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-adaptive-proxy-' + [Guid]::NewGuid().ToString('N'))

foreach ($port in @($listenPort, $originPort, $missingClashPort)) {
    if (Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        throw "Test port is already in use: $port"
    }
}

$originProcess = $null
$proxyProcess = $null
try {
    [IO.Directory]::CreateDirectory($testRoot) | Out-Null
    $originProcess = Start-Process `
        -FilePath $python `
        -ArgumentList @(
            '-m', 'http.server', [string]$originPort,
            '--bind', '127.0.0.1'
        ) `
        -WorkingDirectory $testRoot `
        -WindowStyle Hidden `
        -PassThru

    $proxyProcess = Start-Process `
        -FilePath $python `
        -ArgumentList @(
            "$routerRoot\scripts\adaptive_proxy.py",
            '--listen-port', [string]$listenPort,
            '--clash-port', [string]$missingClashPort,
            '--proxy-policy', 'prefer'
        ) `
        -WorkingDirectory $routerRoot `
        -WindowStyle Hidden `
        -PassThru

    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $proxyListener = Get-NetTCPConnection `
            -LocalAddress 127.0.0.1 -LocalPort $listenPort `
            -State Listen -ErrorAction SilentlyContinue
        $originListener = Get-NetTCPConnection `
            -LocalAddress 127.0.0.1 -LocalPort $originPort `
            -State Listen -ErrorAction SilentlyContinue
        if ($proxyListener -and $originListener) { break }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)

    if (-not $proxyListener) { throw 'The isolated adaptive proxy did not start.' }
    if (-not $originListener) { throw 'The isolated loopback origin did not start.' }

    $target = "http://127.0.0.1:$originPort/"
    $httpCode = & curl.exe `
        --silent `
        --show-error `
        --output NUL `
        --write-out '%{http_code}' `
        --connect-timeout 3 `
        --max-time 10 `
        --noproxy '__codex_router_no_bypass__' `
        --proxytunnel `
        --proxy "http://127.0.0.1:$listenPort" `
        $target `
        2>$null
    $curlExit = $LASTEXITCODE
    $httpCode = ([string]$httpCode).Trim()
    if ($curlExit -ne 0 -or $httpCode -ne '200') {
        throw "Adaptive proxy loopback fallback failed (curl=$curlExit, HTTP=$httpCode)."
    }
    [pscustomobject]@{
        Mode = 'Missing-Proxy-Loopback-Direct'
        Target = ([Uri]$target).Authority
        HttpCode = $httpCode
        CurlExit = $curlExit
    }
} finally {
    foreach ($child in @($proxyProcess, $originProcess)) {
        if ($null -eq $child) { continue }
        $running = Get-Process -Id $child.Id -ErrorAction SilentlyContinue
        if ($running -and [string]::Equals($running.Path, $python, [StringComparison]::OrdinalIgnoreCase)) {
            Stop-Process -Id $running.Id -Force -ErrorAction SilentlyContinue
            Wait-Process -Id $running.Id -Timeout 5 -ErrorAction SilentlyContinue
        }
    }
    if (Test-Path -LiteralPath $testRoot -PathType Container) {
        $cleanupDeadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            try {
                [IO.Directory]::Delete($testRoot, $true)
                break
            } catch [IO.IOException] {
                if ([DateTime]::UtcNow -ge $cleanupDeadline) { throw }
                Start-Sleep -Milliseconds 100
            }
        } while (Test-Path -LiteralPath $testRoot -PathType Container)
    }
}
