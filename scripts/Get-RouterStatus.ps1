$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'ProxyDiscovery.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force
$routerBaseUri = Get-RouterBaseUri
$routerPort = ([Uri]$routerBaseUri).Port
$cliPort = 18081
$cliPortOverride = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CLI_PORT', 'Process')
if ([string]::IsNullOrWhiteSpace($cliPortOverride)) {
    $cliPortOverride = [Environment]::GetEnvironmentVariable('CODEX_ROUTER_CLI_PORT', 'User')
}
if (-not [string]::IsNullOrWhiteSpace($cliPortOverride)) {
    $parsed = 0
    if ([int]::TryParse($cliPortOverride.Trim(), [ref]$parsed) -and $parsed -gt 0 -and $parsed -le 65535) {
        $cliPort = $parsed
    }
}
$ports = @(
    @{Name='Router Host'; Port=$routerPort},
    @{Name='CLIProxyAPI'; Port=$cliPort}
)

foreach ($item in $ports) {
    $client = [Net.Sockets.TcpClient]::new()
    $running = $false
    try {
        $task = $client.ConnectAsync('127.0.0.1', $item.Port)
        $running = $task.Wait(500) -and $client.Connected
    } catch { } finally { $client.Dispose() }
    $listener = if ($running) { Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $item.Port -State Listen -ErrorAction SilentlyContinue } else { $null }
    [pscustomobject]@{
        Component = $item.Name
        Endpoint = "127.0.0.1:$($item.Port)"
        Running = $running
        ProcessId = if ($listener) { ($listener.OwningProcess -join ',') } else { '' }
    }
}

$configPath = Get-RouterConfigPath -RouterRoot $routerRoot
$routerConfig = if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    [IO.File]::ReadAllText($configPath) | ConvertFrom-Json
} else {
    $null
}
$proxyConfig = if ($null -ne $routerConfig) {
    $property = $routerConfig.PSObject.Properties['proxy']
    if ($null -ne $property) { $property.Value } else { $null }
} else {
    $null
}
$proxySettings = Resolve-RouterProxySettings -ProxyConfig $proxyConfig -ProxyPassword $null
$proxyRunning = $true
if ($null -ne $proxySettings.ProxyUrl) {
    $proxyUri = [Uri]$proxySettings.ProxyUrl
    $proxyClient = [Net.Sockets.TcpClient]::new()
    try {
        $proxyTask = $proxyClient.ConnectAsync($proxyUri.Host, $proxyUri.Port)
        $proxyRunning = $proxyTask.Wait(1000) -and $proxyClient.Connected
    } catch {
        $proxyRunning = $false
    } finally {
        $proxyClient.Dispose()
    }
}
[pscustomobject]@{
    Component = 'Network Path'
    Endpoint = if ($null -eq $proxySettings.ProxyUrl) { 'direct' } else { "$($proxySettings.Source) proxy" }
    Running = $proxyRunning
    ProcessId = ''
}

try {
    $apiKey = Get-RouterCredential -Name 'LocalApiKey'
    $request = [Net.HttpWebRequest]::Create("$routerBaseUri/v1/models")
    $request.Method = 'GET'
    $request.Proxy = $null
    $request.Timeout = 4000
    $request.ReadWriteTimeout = 4000
    $request.KeepAlive = $false
    $request.Headers['Authorization'] = "Bearer $apiKey"
    $response = [Net.HttpWebResponse]$request.GetResponse()
    try {
        [pscustomobject]@{ Component='Deep Health'; Endpoint='/v1/models'; Running=([int]$response.StatusCode -eq 200); ProcessId='' }
    } finally {
        $response.Dispose()
    }
} catch {
    [pscustomobject]@{ Component='Deep Health'; Endpoint='/v1/models'; Running=$false; ProcessId='' }
} finally {
    $apiKey = $null
}