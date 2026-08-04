Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-RouterProxyProperty {
    param(
        [AllowNull()]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )
    if ($null -eq $InputObject) { return $null }
    if ($InputObject -is [Collections.IDictionary]) {
        foreach ($key in $InputObject.Keys) {
            if ([string]::Equals([string]$key, $Name, [StringComparison]::OrdinalIgnoreCase)) {
                return $InputObject[$key]
            }
        }
        return $null
    }
    $property = $InputObject.PSObject.Properties |
        Where-Object { [string]::Equals($_.Name, $Name, [StringComparison]::OrdinalIgnoreCase) } |
        Select-Object -First 1
    if ($null -eq $property) { return $null }
    return $property.Value
}

function ConvertTo-RouterProxyUrl {
    param(
        [Parameter(Mandatory)][string]$Value,
        [ValidateSet('http', 'https', 'socks5', 'socks5h')][string]$DefaultScheme = 'http'
    )
    $candidate = $Value.Trim()
    if ([string]::IsNullOrWhiteSpace($candidate) -or $candidate -match "[`r`n]") {
        throw 'Proxy URL is empty or contains an invalid line break.'
    }
    if ($candidate -notmatch '^[a-zA-Z][a-zA-Z0-9+.-]*://') {
        $candidate = $DefaultScheme + '://' + $candidate
    }
    $uri = $null
    if (-not [Uri]::TryCreate($candidate, [UriKind]::Absolute, [ref]$uri)) {
        throw 'Proxy URL is invalid.'
    }
    $scheme = $uri.Scheme.ToLowerInvariant()
    if ($scheme -notin @('http', 'https', 'socks5', 'socks5h') -or
        [string]::IsNullOrWhiteSpace($uri.Host) -or
        $uri.Port -lt 1 -or $uri.Port -gt 65535 -or
        -not [string]::IsNullOrEmpty($uri.Query) -or
        -not [string]::IsNullOrEmpty($uri.Fragment) -or
        $uri.AbsolutePath -notin @('', '/')) {
        throw 'Proxy URL has an unsupported scheme, authority, or path.'
    }
    return $uri.AbsoluteUri.TrimEnd('/')
}

function Merge-RouterNoProxy {
    param([AllowNull()][string[]]$Values)
    $entries = [Collections.Generic.List[string]]::new()
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($required in @('127.0.0.1', 'localhost', '::1')) {
        if ($seen.Add($required)) { $entries.Add($required) }
    }
    foreach ($value in @($Values)) {
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        foreach ($rawEntry in ($value -split '[,;\s]+')) {
            $entry = $rawEntry.Trim()
            if ([string]::IsNullOrWhiteSpace($entry)) { continue }
            if ($entry -eq '<local>') {
                if ($seen.Add($entry)) { $entries.Add($entry) }
                continue
            }
            if ($entry.StartsWith('*.')) { $entry = $entry.Substring(1) }
            if ($entry -ne '*' -and $entry -notmatch '^[a-zA-Z0-9._*:\[\]-]+(?:/[0-9]{1,3})?$') { continue }
            if ($seen.Add($entry)) { $entries.Add($entry) }
        }
    }
    return $entries -join ','
}

function Test-RouterIpInCidr {
    param(
        [Parameter(Mandatory)][Net.IPAddress]$Address,
        [Parameter(Mandatory)][Net.IPAddress]$Network,
        [Parameter(Mandatory)][int]$PrefixLength
    )
    $addressBytes = $Address.GetAddressBytes()
    $networkBytes = $Network.GetAddressBytes()
    if ($addressBytes.Length -ne $networkBytes.Length -or
        $PrefixLength -lt 0 -or $PrefixLength -gt ($addressBytes.Length * 8)) {
        return $false
    }
    $wholeBytes = [Math]::Floor($PrefixLength / 8)
    for ($index = 0; $index -lt $wholeBytes; $index++) {
        if ($addressBytes[$index] -ne $networkBytes[$index]) { return $false }
    }
    $remainingBits = $PrefixLength % 8
    if ($remainingBits -eq 0) { return $true }
    $mask = (0xff -shl (8 - $remainingBits)) -band 0xff
    return ($addressBytes[$wholeBytes] -band $mask) -eq ($networkBytes[$wholeBytes] -band $mask)
}

function Test-RouterProxyBypass {
    param(
        [Parameter(Mandatory)][string]$TargetUri,
        [AllowNull()][string]$NoProxy
    )
    $uri = $null
    if (-not [Uri]::TryCreate($TargetUri, [UriKind]::Absolute, [ref]$uri) -or
        [string]::IsNullOrWhiteSpace($uri.Host)) {
        return $false
    }
    $host = $uri.Host.TrimStart('[').TrimEnd(']').TrimEnd('.').ToLowerInvariant()
    $targetAddress = $null
    $isIpAddress = [Net.IPAddress]::TryParse($host, [ref]$targetAddress)
    if ($host -eq 'localhost' -or ($isIpAddress -and [Net.IPAddress]::IsLoopback($targetAddress))) {
        return $true
    }

    foreach ($rawEntry in ([string]$NoProxy -split '[,;\s]+')) {
        $entry = $rawEntry.Trim()
        if ([string]::IsNullOrWhiteSpace($entry)) { continue }
        if ($entry -eq '*') { return $true }
        if ($entry -eq '<local>') {
            if (-not $isIpAddress -and -not $host.Contains('.')) { return $true }
            continue
        }

        $candidate = $entry
        $slash = $candidate.LastIndexOf('/')
        if ($slash -gt 0) {
            $network = $null
            $prefix = 0
            if ([Net.IPAddress]::TryParse($candidate.Substring(0, $slash).Trim('[', ']'), [ref]$network) -and
                [int]::TryParse($candidate.Substring($slash + 1), [ref]$prefix) -and
                $isIpAddress -and
                (Test-RouterIpInCidr -Address $targetAddress -Network $network -PrefixLength $prefix)) {
                return $true
            }
            continue
        }

        if ($candidate.StartsWith('[')) {
            $closingBracket = $candidate.IndexOf(']')
            if ($closingBracket -gt 0) { $candidate = $candidate.Substring(1, $closingBracket - 1) }
        } elseif (@($candidate.ToCharArray() | Where-Object { $_ -eq ':' }).Count -eq 1) {
            $name, $portText = $candidate -split ':', 2
            if ($portText -match '^\d+$') {
                if ([int]$portText -ne $uri.Port) { continue }
                $candidate = $name
            }
        }

        $candidate = $candidate.TrimEnd('.').ToLowerInvariant()
        if ($candidate.StartsWith('*.')) { $candidate = $candidate.Substring(1) }
        if ($candidate.StartsWith('.')) {
            if ($host.EndsWith($candidate, [StringComparison]::OrdinalIgnoreCase) -or
                [string]::Equals($host, $candidate.Substring(1), [StringComparison]::OrdinalIgnoreCase)) {
                return $true
            }
            continue
        }
        if ($candidate.Contains('*')) {
            $pattern = '^' + [Regex]::Escape($candidate).Replace('\*', '.*') + '$'
            if ($host -match $pattern) { return $true }
            continue
        }
        if ([string]::Equals($host, $candidate.Trim('[', ']'), [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Test-RouterLoopbackProxy {
    param([AllowNull()][string]$ProxyUrl)

    $uri = $null
    if ([string]::IsNullOrWhiteSpace($ProxyUrl) -or
        -not [Uri]::TryCreate($ProxyUrl, [UriKind]::Absolute, [ref]$uri)) {
        return $false
    }
    if ([string]::Equals($uri.Host, 'localhost', [StringComparison]::OrdinalIgnoreCase)) {
        return $true
    }
    $address = $null
    return [Net.IPAddress]::TryParse($uri.Host.Trim('[', ']'), [ref]$address) -and
        [Net.IPAddress]::IsLoopback($address)
}

function Test-RouterDirectTargetReachability {
    param(
        [Parameter(Mandatory)][string]$TargetUri,
        [ValidateRange(250, 30000)][int]$TimeoutMilliseconds = 3000,
        [scriptblock]$RequestInvoker
    )

    $baseUri = $null
    if (-not [Uri]::TryCreate($TargetUri, [UriKind]::Absolute, [ref]$baseUri) -or
        $baseUri.Scheme -notin @('http', 'https')) {
        return $false
    }
    $probeBuilder = [UriBuilder]$baseUri
    $probeBuilder.Path = $probeBuilder.Path.TrimEnd('/') + '/models'
    $probeBuilder.Query = ''
    $probeBuilder.Fragment = ''
    $probeUri = $probeBuilder.Uri

    if ($null -ne $RequestInvoker) {
        return [bool](& $RequestInvoker $probeUri $TimeoutMilliseconds)
    }

    $request = [Net.HttpWebRequest]::Create($probeUri)
    $request.Method = 'GET'
    $request.Proxy = $null
    $request.AllowAutoRedirect = $false
    $request.KeepAlive = $false
    $request.Timeout = $TimeoutMilliseconds
    $request.ReadWriteTimeout = $TimeoutMilliseconds
    $request.UserAgent = 'Codex-Router/1.1 direct-route-probe'
    try {
        $response = $request.GetResponse()
        try { return $true } finally { $response.Dispose() }
    } catch [Net.WebException] {
        if ($null -ne $_.Exception.Response) {
            $_.Exception.Response.Dispose()
            return $true
        }
        return $false
    } catch {
        return $false
    }
}

function Test-RouterDirectFallbackEligible {
    param(
        [Parameter(Mandatory)]$ProxySettings,
        [Parameter(Mandatory)][string]$TargetUri,
        [Parameter(Mandatory)][bool]$DirectReachable
    )

    if ([string]$ProxySettings.Mode -ne 'proxy' -or -not $DirectReachable) { return $false }
    if ([string]$ProxySettings.Source -eq 'explicit') { return $false }
    if (-not (Test-RouterLoopbackProxy -ProxyUrl ([string]$ProxySettings.ProxyUrl))) { return $false }
    if (Test-RouterProxyBypass -TargetUri $TargetUri -NoProxy ([string]$ProxySettings.NoProxy)) {
        return $false
    }
    return $true
}

function ConvertFrom-RouterProxyServer {
    param([AllowNull()][string]$Value)
    $httpProxy = $null
    $httpsProxy = $null
    $allProxy = $null
    $raw = ([string]$Value).Trim()
    if ([string]::IsNullOrWhiteSpace($raw)) {
        return [pscustomobject]@{ HttpProxyUrl = $null; HttpsProxyUrl = $null; AllProxyUrl = $null }
    }

    if (-not $raw.Contains('=')) {
        try { $allProxy = ConvertTo-RouterProxyUrl -Value $raw } catch { $allProxy = $null }
        return [pscustomobject]@{
            HttpProxyUrl = $allProxy
            HttpsProxyUrl = $allProxy
            AllProxyUrl = $allProxy
        }
    }

    $mapping = @{}
    foreach ($part in ($raw -split ';')) {
        $pair = $part -split '=', 2
        if ($pair.Count -eq 2 -and -not [string]::IsNullOrWhiteSpace($pair[1])) {
            $mapping[$pair[0].Trim().ToLowerInvariant()] = $pair[1].Trim()
        }
    }
    if ($mapping.ContainsKey('http')) {
        try { $httpProxy = ConvertTo-RouterProxyUrl -Value ([string]$mapping.http) } catch { $httpProxy = $null }
    }
    if ($mapping.ContainsKey('https')) {
        try { $httpsProxy = ConvertTo-RouterProxyUrl -Value ([string]$mapping.https) } catch { $httpsProxy = $null }
    }
    foreach ($name in @('socks5', 'socks')) {
        if ($mapping.ContainsKey($name)) {
            try { $allProxy = ConvertTo-RouterProxyUrl -Value ([string]$mapping[$name]) -DefaultScheme 'socks5' } catch { $allProxy = $null }
            break
        }
    }
    return [pscustomobject]@{
        HttpProxyUrl = $httpProxy
        HttpsProxyUrl = $httpsProxy
        AllProxyUrl = $allProxy
    }
}

function Get-RouterWindowsProxyCandidates {
    param([AllowNull()]$InternetSettings)
    if ($null -eq $InternetSettings -or [int](Get-RouterProxyProperty $InternetSettings 'ProxyEnable') -ne 1) {
        return ConvertFrom-RouterProxyServer -Value $null
    }
    return ConvertFrom-RouterProxyServer -Value ([string](Get-RouterProxyProperty $InternetSettings 'ProxyServer'))
}

function Get-RouterWindowsProxyCandidate {
    param([AllowNull()]$InternetSettings)
    $candidates = Get-RouterWindowsProxyCandidates -InternetSettings $InternetSettings
    foreach ($candidate in @($candidates.HttpsProxyUrl, $candidates.AllProxyUrl, $candidates.HttpProxyUrl)) {
        if (-not [string]::IsNullOrWhiteSpace([string]$candidate)) { return [string]$candidate }
    }
    return $null
}

function Initialize-RouterWinHttpNative {
    if ('CodexRouter.WinHttpProxyNative' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace CodexRouter {
    public static class WinHttpProxyNative {
        [StructLayout(LayoutKind.Sequential)]
        private struct CurrentUserProxyConfig {
            [MarshalAs(UnmanagedType.Bool)] public bool AutoDetect;
            public IntPtr AutoConfigUrl;
            public IntPtr Proxy;
            public IntPtr ProxyBypass;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct DefaultProxyInfo {
            public uint AccessType;
            public IntPtr Proxy;
            public IntPtr ProxyBypass;
        }

        [DllImport("winhttp.dll", SetLastError = true)]
        private static extern bool WinHttpGetIEProxyConfigForCurrentUser(out CurrentUserProxyConfig config);

        [DllImport("winhttp.dll", SetLastError = true)]
        private static extern bool WinHttpGetDefaultProxyConfiguration(out DefaultProxyInfo info);

        [DllImport("kernel32.dll")]
        private static extern IntPtr GlobalFree(IntPtr memory);

        private static string TakeString(ref IntPtr value) {
            if (value == IntPtr.Zero) return String.Empty;
            try { return Marshal.PtrToStringUni(value) ?? String.Empty; }
            finally { GlobalFree(value); value = IntPtr.Zero; }
        }

        public static IDictionary<string, object> GetCurrentUserConfiguration() {
            CurrentUserProxyConfig config;
            if (!WinHttpGetIEProxyConfigForCurrentUser(out config)) return null;
            return new Dictionary<string, object>(StringComparer.OrdinalIgnoreCase) {
                { "AutoDetect", config.AutoDetect },
                { "AutoConfigUrl", TakeString(ref config.AutoConfigUrl) },
                { "Proxy", TakeString(ref config.Proxy) },
                { "ProxyBypass", TakeString(ref config.ProxyBypass) }
            };
        }

        public static IDictionary<string, object> GetDefaultConfiguration() {
            DefaultProxyInfo info;
            if (!WinHttpGetDefaultProxyConfiguration(out info)) return null;
            return new Dictionary<string, object>(StringComparer.OrdinalIgnoreCase) {
                { "AccessType", info.AccessType },
                { "Proxy", TakeString(ref info.Proxy) },
                { "ProxyBypass", TakeString(ref info.ProxyBypass) }
            };
        }
    }
}
'@
}

function Get-RouterCurrentUserAutoProxyConfiguration {
    try {
        Initialize-RouterWinHttpNative
        return [CodexRouter.WinHttpProxyNative]::GetCurrentUserConfiguration()
    } catch {
        return $null
    }
}

function Get-RouterWinHttpProxyConfiguration {
    try {
        Initialize-RouterWinHttpNative
        return [CodexRouter.WinHttpProxyNative]::GetDefaultConfiguration()
    } catch {
        return $null
    }
}

function Resolve-RouterProxySettings {
    param(
        [AllowNull()]$ProxyConfig,
        [AllowNull()][string]$ProxyPassword,
        [AllowNull()][Collections.IDictionary]$EnvironmentValues,
        [AllowNull()]$InternetSettings,
        [AllowNull()]$CurrentUserProxyConfiguration,
        [AllowNull()]$WinHttpProxyConfiguration,
        [switch]$SkipWindowsProxy
    )
    if ($null -eq $EnvironmentValues) {
        $EnvironmentValues = @{}
        foreach ($name in @('HTTPS_PROXY', 'https_proxy', 'HTTP_PROXY', 'http_proxy', 'ALL_PROXY', 'all_proxy', 'NO_PROXY', 'no_proxy')) {
            $EnvironmentValues[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
    }

    $enabled = [bool](Get-RouterProxyProperty $ProxyConfig 'enabled')
    $autoDetect = if ($null -eq $ProxyConfig) { $true } else { [bool](Get-RouterProxyProperty $ProxyConfig 'autoDetect') }
    $proxyUrl = $null
    $source = 'direct'
    $additionalBypass = [Collections.Generic.List[string]]::new()
    foreach ($name in @('NO_PROXY', 'no_proxy')) {
        $value = [string](Get-RouterProxyProperty $EnvironmentValues $name)
        if (-not [string]::IsNullOrWhiteSpace($value)) { $additionalBypass.Add($value) }
    }

    if ($enabled) {
        $proxyType = [string](Get-RouterProxyProperty $ProxyConfig 'proxyType')
        if ([string]::IsNullOrWhiteSpace($proxyType)) {
            $proxyType = [string](Get-RouterProxyProperty $ProxyConfig 'type')
        }
        if ([string]::IsNullOrWhiteSpace($proxyType)) { $proxyType = 'http' }
        $proxyType = $proxyType.Trim().ToLowerInvariant()
        if ($proxyType -notin @('http', 'https', 'socks5', 'socks5h')) {
            throw "Unsupported proxy type '$proxyType'."
        }
        $proxyHost = [string](Get-RouterProxyProperty $ProxyConfig 'host')
        $proxyHost = $proxyHost.Trim().TrimStart('[').TrimEnd(']')
        $proxyPort = 0
        if ([string]::IsNullOrWhiteSpace($proxyHost) -or
            -not [int]::TryParse([string](Get-RouterProxyProperty $ProxyConfig 'port'), [ref]$proxyPort) -or
            $proxyPort -lt 1 -or $proxyPort -gt 65535 -or
            $proxyHost -match "[`r`n/@]") {
            throw 'Proxy host or port is invalid.'
        }
        $hostForUri = if ($proxyHost.Contains(':')) { '[' + $proxyHost + ']' } else { $proxyHost }
        $username = [string](Get-RouterProxyProperty $ProxyConfig 'username')
        $authority = ''
        if (-not [string]::IsNullOrWhiteSpace($username)) {
            $authority = [Uri]::EscapeDataString($username) + ':' +
                [Uri]::EscapeDataString([string]$ProxyPassword) + '@'
        }
        $proxyUrl = ConvertTo-RouterProxyUrl -Value ($proxyType + '://' + $authority + $hostForUri + ':' + $proxyPort)
        $source = 'explicit'
    } elseif ($autoDetect) {
        foreach ($name in @('HTTPS_PROXY', 'https_proxy', 'HTTP_PROXY', 'http_proxy', 'ALL_PROXY', 'all_proxy')) {
            $candidate = [string](Get-RouterProxyProperty $EnvironmentValues $name)
            if ([string]::IsNullOrWhiteSpace($candidate)) { continue }
            try { $proxyUrl = ConvertTo-RouterProxyUrl -Value $candidate } catch { $proxyUrl = $null }
            if ($null -ne $proxyUrl) {
                $source = 'environment'
                break
            }
        }
        if ($null -eq $proxyUrl -and -not $SkipWindowsProxy) {
            if ($null -eq $InternetSettings) {
                try {
                    $InternetSettings = Get-ItemProperty `
                        -LiteralPath 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings' `
                        -ErrorAction Stop
                } catch { $InternetSettings = $null }
            }
            $proxyUrl = Get-RouterWindowsProxyCandidate -InternetSettings $InternetSettings
            if ($null -ne $proxyUrl) {
                $source = 'windows'
                $override = [string](Get-RouterProxyProperty $InternetSettings 'ProxyOverride')
                if (-not [string]::IsNullOrWhiteSpace($override)) { $additionalBypass.Add($override) }
            }
            if ($null -eq $proxyUrl) {
                if ($null -eq $CurrentUserProxyConfiguration) {
                    $CurrentUserProxyConfiguration = Get-RouterCurrentUserAutoProxyConfiguration
                }
                $currentUserCandidates = ConvertFrom-RouterProxyServer -Value ([string](
                    Get-RouterProxyProperty $CurrentUserProxyConfiguration 'Proxy'))
                foreach ($candidate in @(
                    $currentUserCandidates.HttpsProxyUrl,
                    $currentUserCandidates.AllProxyUrl,
                    $currentUserCandidates.HttpProxyUrl
                )) {
                    if (-not [string]::IsNullOrWhiteSpace([string]$candidate)) {
                        $proxyUrl = [string]$candidate
                        $source = 'windows-user'
                        break
                    }
                }
                $bypass = [string](Get-RouterProxyProperty $CurrentUserProxyConfiguration 'ProxyBypass')
                if (-not [string]::IsNullOrWhiteSpace($bypass)) { $additionalBypass.Add($bypass) }
            }
            if ($null -eq $proxyUrl) {
                if ($null -eq $WinHttpProxyConfiguration) {
                    $WinHttpProxyConfiguration = Get-RouterWinHttpProxyConfiguration
                }
                $winHttpCandidates = ConvertFrom-RouterProxyServer -Value ([string](
                    Get-RouterProxyProperty $WinHttpProxyConfiguration 'Proxy'))
                foreach ($candidate in @(
                    $winHttpCandidates.HttpsProxyUrl,
                    $winHttpCandidates.AllProxyUrl,
                    $winHttpCandidates.HttpProxyUrl
                )) {
                    if (-not [string]::IsNullOrWhiteSpace([string]$candidate)) {
                        $proxyUrl = [string]$candidate
                        $source = 'winhttp-machine'
                        break
                    }
                }
                $bypass = [string](Get-RouterProxyProperty $WinHttpProxyConfiguration 'ProxyBypass')
                if (-not [string]::IsNullOrWhiteSpace($bypass)) { $additionalBypass.Add($bypass) }
            }
        }
    }

    $hasCredentials = $false
    if ($null -ne $proxyUrl) {
        $proxyUri = [Uri]$proxyUrl
        $hasCredentials = -not [string]::IsNullOrWhiteSpace($proxyUri.UserInfo)
    }
    return [pscustomobject][ordered]@{
        Mode = if ($null -eq $proxyUrl) { 'direct' } else { 'proxy' }
        Source = $source
        ProxyUrl = $proxyUrl
        NoProxy = Merge-RouterNoProxy -Values @($additionalBypass)
        HasCredentials = $hasCredentials
        SupportsAccountBinding = $null -ne $proxyUrl -and -not $hasCredentials
        Diagnostic = ''
    }
}

Export-ModuleMember -Function `
    ConvertTo-RouterProxyUrl, `
    Get-RouterWindowsProxyCandidate, `
    Merge-RouterNoProxy, `
    Test-RouterProxyBypass, `
    Test-RouterLoopbackProxy, `
    Test-RouterDirectTargetReachability, `
    Test-RouterDirectFallbackEligible, `
    Resolve-RouterProxySettings
