Set-StrictMode -Version Latest

$script:RouterRoot = Split-Path -Parent $PSScriptRoot
$script:BaseUri = 'http://127.0.0.1:18080'
Import-Module "$script:RouterRoot\scripts\CredentialStore.psm1" -Force

function New-RouterAdminSession {
    $password = Get-RouterCredential -Name 'AdminPassword'
    try {
        $login = Invoke-RestMethod `
            -Method Post `
            -Uri "$script:BaseUri/api/v1/auth/login" `
            -ContentType 'application/json' `
            -Body (@{ email = 'admin@sub2api.local'; password = $password } | ConvertTo-Json -Compress) `
            -TimeoutSec 15
        $token = $login.data.access_token
        if (-not $token) { $token = $login.access_token }
        if (-not $token) { throw 'Sub2API admin login returned no access token.' }

        return [pscustomobject]@{
            BaseUri = $script:BaseUri
            Headers = @{ Authorization = "Bearer $token" }
        }
    } finally {
        $password = $null
        $token = $null
        $login = $null
    }
}

function Get-RouterResponseData {
    param([Parameter(Mandatory)][AllowNull()]$Response)
    if ($null -eq $Response) { return }
    if ($null -ne $Response.PSObject.Properties['data']) { return $Response.data }
    return $Response
}

function Invoke-RouterApi {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][ValidateSet('GET', 'POST', 'PUT', 'DELETE')][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [object]$Body,
        [string]$IdempotencyKey
    )

    $headers = @{}
    foreach ($entry in $Session.Headers.GetEnumerator()) { $headers[$entry.Key] = $entry.Value }
    if ($IdempotencyKey) { $headers['Idempotency-Key'] = $IdempotencyKey }

    $arguments = @{
        Method = $Method
        Uri = "$($Session.BaseUri)$Path"
        Headers = $headers
        TimeoutSec = 30
    }
    $bodyBytes = $null
    if ($PSBoundParameters.ContainsKey('Body')) {
        $arguments.ContentType = 'application/json'
        $jsonBody = $Body | ConvertTo-Json -Depth 100 -Compress
        $bodyBytes = [Text.UTF8Encoding]::new($false).GetBytes($jsonBody)
        $arguments.Body = $bodyBytes
    }

    try {
        return Invoke-RestMethod @arguments
    } catch {
        $statusCode = 'transport'
        if ($null -ne $_.Exception.Response -and $null -ne $_.Exception.Response.StatusCode) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }
        throw "Sub2API request failed ($statusCode): $Method $Path"
    } finally {
        if ($null -ne $bodyBytes) { [Array]::Clear($bodyBytes, 0, $bodyBytes.Length) }
        $jsonBody = $null
        $headers.Clear()
        $arguments.Clear()
    }
}

function Get-RouterGroups {
    param([Parameter(Mandatory)]$Session)
    $response = Invoke-RouterApi -Session $Session -Method GET -Path '/api/v1/admin/groups/all?include_inactive=true'
    return @(Get-RouterResponseData -Response $response)
}

function Get-RouterAccounts {
    param([Parameter(Mandatory)]$Session)
    $response = Invoke-RouterApi -Session $Session -Method GET -Path '/api/v1/admin/accounts?page=1&page_size=200'
    $data = Get-RouterResponseData -Response $response
    if ($null -ne $data.PSObject.Properties['items']) { return @($data.items) }
    return @($data)
}

function Set-RouterLocalAdaptiveProxy {
    param([Parameter(Mandatory)]$Session)

    $name = 'Local Adaptive HTTP'
    $response = Invoke-RouterApi -Session $Session -Method GET -Path '/api/v1/admin/proxies?page=1&page_size=200'
    $data = Get-RouterResponseData -Response $response
    $proxies = if ($null -ne $data.PSObject.Properties['items']) { @($data.items) } else { @($data) }
    $existing = $proxies | Where-Object { $_.name -in @($name, 'Local Clash HTTP') } | Select-Object -First 1
    $body = @{
        name = $name
        protocol = 'http'
        host = '127.0.0.1'
        port = 17897
        username = ''
        password = ''
        status = 'active'
        fallback_mode = 'none'
        expiry_warn_days = 0
    }

    if ($existing) {
        $updated = Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/proxies/$($existing.id)" -Body $body
        $proxy = Get-RouterResponseData -Response $updated
        $action = 'updated'
    } else {
        $created = Invoke-RouterApi `
            -Session $Session `
            -Method POST `
            -Path '/api/v1/admin/proxies' `
            -Body $body `
            -IdempotencyKey 'codex-router-local-adaptive-http-v1'
        $proxy = Get-RouterResponseData -Response $created
        $action = 'created'
    }

    return [pscustomobject]@{
        Id = [long]$proxy.id
        Name = [string]$proxy.name
        Endpoint = 'http://127.0.0.1:17897'
        Action = $action
    }
}

function Set-RouterAccountProxy {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId,
        [Parameter(Mandatory)][long]$ProxyId
    )

    [void](Invoke-RouterApi `
        -Session $Session `
        -Method PUT `
        -Path "/api/v1/admin/accounts/$AccountId" `
        -Body @{ proxy_id = $ProxyId })
}

function Set-RouterScheduledRecovery {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId,
        [Parameter(Mandatory)][string]$ModelId
    )

    $response = Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$AccountId/scheduled-test-plans"
    $plans = @(Get-RouterResponseData -Response $response)
    $plan = $plans | Where-Object {
        $null -ne $_ -and
        $null -ne $_.PSObject.Properties['cron_expression'] -and
        $_.cron_expression -eq '0 * * * *'
    } | Select-Object -First 1
    $body = @{
        account_id = $AccountId
        model_id = $ModelId
        cron_expression = '0 * * * *'
        enabled = $true
        max_results = 48
        auto_recover = $true
    }
    if ($plan) {
        [void](Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/scheduled-test-plans/$($plan.id)" -Body $body)
        return [long]$plan.id
    }

    $created = Invoke-RouterApi -Session $Session -Method POST -Path '/api/v1/admin/scheduled-test-plans' -Body $body
    return [long](Get-RouterResponseData -Response $created).id
}

Export-ModuleMember -Function `
    New-RouterAdminSession, `
    Get-RouterResponseData, `
    Invoke-RouterApi, `
    Get-RouterGroups, `
    Get-RouterAccounts, `
    Set-RouterLocalAdaptiveProxy, `
    Set-RouterAccountProxy, `
    Set-RouterScheduledRecovery
