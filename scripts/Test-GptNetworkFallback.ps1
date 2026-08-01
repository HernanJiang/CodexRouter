Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$failurePort = 57998
$proxyId = 1

Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force

if (Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $failurePort -State Listen -ErrorAction SilentlyContinue) {
    throw "Failure-test port is unexpectedly in use: $failurePort"
}

$session = New-RouterAdminSession
$headers = $null
$localKey = $null
$originalPort = $null
try {
    $proxyResponse = Invoke-RouterApi `
        -Session $session `
        -Method GET `
        -Path '/api/v1/admin/proxies?page=1&page_size=200'
    $proxyData = Get-RouterResponseData -Response $proxyResponse
    $proxies = if ($proxyData.PSObject.Properties['items']) { @($proxyData.items) } else { @($proxyData) }
    $proxy = $proxies | Where-Object id -eq $proxyId | Select-Object -First 1
    if (-not $proxy) { throw "Adaptive proxy record was not found: $proxyId" }
    $originalPort = [int]$proxy.port

    [void](Invoke-RouterApi `
        -Session $session `
        -Method PUT `
        -Path "/api/v1/admin/proxies/$proxyId" `
        -Body @{ port = $failurePort })

    $localKey = & "$routerRoot\scripts\Get-LocalApiKey.ps1"
    $headers = @{ Authorization = "Bearer $localKey" }
    $body = @{
        model = 'gpt-5.6-sol'
        input = 'Reply with exactly NETWORK_FALLBACK_OK.'
        reasoning = @{ effort = 'low' }
        stream = $false
    } | ConvertTo-Json -Depth 10 -Compress

    $started = Get-Date
    $response = Invoke-RestMethod `
        -Method POST `
        -Uri 'http://127.0.0.1:18080/v1/responses' `
        -Headers $headers `
        -ContentType 'application/json' `
        -Body $body `
        -TimeoutSec 180
    $elapsed = [Math]::Round(((Get-Date) - $started).TotalSeconds, 2)

    Start-Sleep -Milliseconds 500
    $usageResponse = Invoke-RouterApi `
        -Session $session `
        -Method GET `
        -Path '/api/v1/admin/usage?page=1&page_size=10&model=gpt-5.6-sol&sort_by=id&sort_order=desc'
    $usageData = Get-RouterResponseData -Response $usageResponse
    $items = if ($usageData.PSObject.Properties['items']) { @($usageData.items) } else { @($usageData) }
    $latest = $items | Select-Object -First 1

    [pscustomobject]@{
        Request = 'GPT proxy-error fallback'
        Http = 200
        ResponseModel = $response.model
        Seconds = $elapsed
        UsageId = $latest.id
        AccountId = $latest.account_id
        RequestedModel = $latest.model
        UpstreamModel = $latest.upstream_model
    }
} finally {
    if ($null -ne $originalPort) {
        [void](Invoke-RouterApi `
            -Session $session `
            -Method PUT `
            -Path "/api/v1/admin/proxies/$proxyId" `
            -Body @{ port = $originalPort })
    }
    if ($headers) { $headers.Clear() }
    $session.Headers.Clear()
    $localKey = $null
    $body = $null
}
