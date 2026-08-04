Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Import-Module (Join-Path $PSScriptRoot 'ProxyDiscovery.psm1') -Force

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$explicit = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{
        autoDetect = $true
        enabled = $true
        proxyType = 'socks5h'
        host = '::1'
        port = '1080'
        username = 'test-user'
    }) `
    -ProxyPassword 'test-password' `
    -EnvironmentValues @{ HTTPS_PROXY = 'http://127.0.0.1:9080' } `
    -SkipWindowsProxy
Assert-True ($explicit.Source -eq 'explicit') 'Explicit proxy did not take precedence.'
$explicitUri = [Uri]$explicit.ProxyUrl
Assert-True ($explicitUri.Scheme -eq 'socks5h') 'Explicit proxy scheme was not preserved.'
Assert-True ($explicitUri.IsLoopback -and $explicitUri.Port -eq 1080) 'Explicit IPv6 proxy authority was not normalized.'
Assert-True ($explicitUri.UserInfo.StartsWith('test-user:')) 'Explicit proxy username was not preserved.'

$environment = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $true; enabled = $false }) `
    -EnvironmentValues @{
        HTTPS_PROXY = 'http://127.0.0.1:2080'
        NO_PROXY = '.example.cn'
    } `
    -SkipWindowsProxy
Assert-True ($environment.Source -eq 'environment') 'Environment proxy was not discovered.'
Assert-True ($environment.ProxyUrl -eq 'http://127.0.0.1:2080') 'Environment proxy was normalized incorrectly.'
Assert-True ($environment.NoProxy -match '(^|,)\.example\.cn(,|$)') 'Environment bypass list was not preserved.'

$windows = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $true; enabled = $false }) `
    -EnvironmentValues @{} `
    -InternetSettings ([pscustomobject]@{
        ProxyEnable = 1
        ProxyServer = 'http=127.0.0.1:3080;https=127.0.0.1:3081;socks=127.0.0.1:1080'
        ProxyOverride = '*.cn;<local>;10.0.0.0/8'
    })
Assert-True ($windows.Source -eq 'windows') 'Windows user proxy was not discovered.'
Assert-True ($windows.ProxyUrl -eq 'http://127.0.0.1:3081') 'Windows HTTPS mapping was not preferred.'
Assert-True ($windows.NoProxy -match '(^|,)\.cn(,|$)') 'Windows domain bypass was not normalized.'
Assert-True ($windows.NoProxy -match '(^|,)10\.0\.0\.0/8(,|$)') 'Windows CIDR bypass was not preserved.'

$direct = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $false; enabled = $false }) `
    -EnvironmentValues @{ HTTPS_PROXY = 'http://127.0.0.1:4080' } `
    -InternetSettings ([pscustomobject]@{ ProxyEnable = 1; ProxyServer = '127.0.0.1:4081' })
Assert-True ($direct.Mode -eq 'direct') 'Direct mode unexpectedly inherited a proxy.'
Assert-True ($null -eq $direct.ProxyUrl) 'Direct mode returned a proxy URL.'
Assert-True ($direct.NoProxy -match '(^|,)127\.0\.0\.1(,|$)') 'Loopback bypass is missing.'

$fallback = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $true; enabled = $false }) `
    -EnvironmentValues @{ HTTPS_PROXY = 'not a proxy URL' } `
    -InternetSettings ([pscustomobject]@{ ProxyEnable = 1; ProxyServer = '127.0.0.1:5080' })
Assert-True ($fallback.Source -eq 'windows') 'Invalid environment proxy blocked Windows fallback.'

$windowsUser = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $true; enabled = $false }) `
    -EnvironmentValues @{} `
    -InternetSettings ([pscustomobject]@{ ProxyEnable = 0 }) `
    -CurrentUserProxyConfiguration ([ordered]@{
        Proxy = 'http=127.0.0.1:6080;https=127.0.0.1:6081'
        ProxyBypass = '*.internal.test;<local>'
    }) `
    -WinHttpProxyConfiguration ([ordered]@{ Proxy = '127.0.0.1:7080' })
Assert-True ($windowsUser.Source -eq 'windows-user') 'Current-user WinHTTP proxy was not discovered.'
Assert-True ($windowsUser.ProxyUrl -eq 'http://127.0.0.1:6081') 'Current-user HTTPS proxy was not preferred.'
Assert-True ($windowsUser.NoProxy -match '(^|,)\.internal\.test(,|$)') 'Current-user bypass list was not preserved.'

$winHttpMachine = Resolve-RouterProxySettings `
    -ProxyConfig ([pscustomobject]@{ autoDetect = $true; enabled = $false }) `
    -EnvironmentValues @{} `
    -InternetSettings ([pscustomobject]@{ ProxyEnable = 0 }) `
    -CurrentUserProxyConfiguration ([ordered]@{ Proxy = '' }) `
    -WinHttpProxyConfiguration ([ordered]@{
        AccessType = 3
        Proxy = '127.0.0.1:7080'
        ProxyBypass = 'build.internal'
    })
Assert-True ($winHttpMachine.Source -eq 'winhttp-machine') 'Machine WinHTTP proxy was not discovered.'
Assert-True ($winHttpMachine.ProxyUrl -eq 'http://127.0.0.1:7080') 'Machine WinHTTP proxy was normalized incorrectly.'
Assert-True ($winHttpMachine.NoProxy -match '(^|,)build\.internal(,|$)') 'Machine WinHTTP bypass list was not preserved.'

Assert-True (
    Test-RouterProxyBypass -TargetUri 'http://127.0.0.1:18080/v1' -NoProxy $direct.NoProxy
) 'IPv4 loopback was not bypassed.'
Assert-True (
    Test-RouterProxyBypass -TargetUri 'http://[::1]:18080/v1' -NoProxy $direct.NoProxy
) 'IPv6 loopback was not bypassed.'
Assert-True (
    Test-RouterProxyBypass -TargetUri 'https://api.example.cn/v1' -NoProxy '.example.cn'
) 'Domain suffix bypass was not applied.'
Assert-True (
    Test-RouterProxyBypass -TargetUri 'https://10.23.4.5/v1' -NoProxy '10.0.0.0/8'
) 'CIDR bypass was not applied.'
Assert-True (
    Test-RouterProxyBypass -TargetUri 'http://intranet/v1' -NoProxy '<local>'
) 'Windows <local> bypass was not applied.'
Assert-True (-not (
    Test-RouterProxyBypass -TargetUri 'https://api.example.com/v1' -NoProxy '.example.cn'
)) 'Unrelated target was bypassed.'

$clashSettings = [pscustomobject]@{
    Mode = 'proxy'
    Source = 'windows'
    ProxyUrl = 'http://127.0.0.1:7897'
    NoProxy = '127.0.0.1,localhost,::1'
}
Assert-True (
    Test-RouterLoopbackProxy -ProxyUrl $clashSettings.ProxyUrl
) 'The Clash mixed port was not recognized as a local proxy.'
Assert-True (
    Test-RouterDirectFallbackEligible `
        -ProxySettings $clashSettings `
        -TargetUri 'https://api.example.com/v1' `
        -DirectReachable $true
) 'An auto-detected Clash route did not permit a verified direct fallback.'
Assert-True (-not (
    Test-RouterDirectFallbackEligible `
        -ProxySettings ([pscustomobject]@{
            Mode = 'proxy'
            Source = 'explicit'
            ProxyUrl = 'http://127.0.0.1:7897'
            NoProxy = ''
        }) `
        -TargetUri 'https://api.example.com/v1' `
        -DirectReachable $true
)) 'A manually required proxy incorrectly permitted direct fallback.'
Assert-True (-not (
    Test-RouterDirectFallbackEligible `
        -ProxySettings $clashSettings `
        -TargetUri 'https://api.example.com/v1' `
        -DirectReachable $false
)) 'An unreachable direct route was added as a fallback.'
Assert-True (
    Test-RouterDirectTargetReachability `
        -TargetUri 'https://api.example.com/v1' `
        -RequestInvoker { param($Uri, $Timeout) $Uri.AbsolutePath -eq '/v1/models' -and $Timeout -eq 3000 }
) 'The direct target probe did not use the provider models endpoint.'

Write-Output 'Proxy discovery tests passed.'
