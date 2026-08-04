$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

$state = [ordered]@{
    Items = [Collections.Generic.List[object]]::new()
    Calls = [Collections.Generic.List[object]]::new()
    NextId = 41L
}
$requestInvoker = {
    param($Session, [string]$Method, [string]$Path, $Body)
    $state.Calls.Add([pscustomobject]@{ Method = $Method; Path = $Path; Body = $Body })
    if ($Method -eq 'GET') {
        return [pscustomobject]@{ data = [pscustomobject]@{ items = @($state.Items) } }
    }
    if ($Method -eq 'POST') {
        $item = [pscustomobject]@{
            id = $state.NextId
            name = [string]$Body.name
            protocol = [string]$Body.protocol
            host = [string]$Body.host
            port = [int]$Body.port
            username = ''
            password = ''
            fallback_mode = [string]$Body.fallback_mode
        }
        $state.NextId++
        $state.Items.Add($item)
        return [pscustomobject]@{ data = $item }
    }
    if ($Method -eq 'PUT') {
        $item = $state.Items[0]
        $item.protocol = [string]$Body.protocol
        $item.host = [string]$Body.host
        $item.port = [int]$Body.port
        $item.fallback_mode = [string]$Body.fallback_mode
        return [pscustomobject]@{ data = $item }
    }
    throw "Unexpected fake API request: $Method $Path"
}.GetNewClosure()

$proxySettings = [pscustomobject]@{
    Mode = 'proxy'
    Source = 'environment'
    ProxyUrl = 'socks5h://[::1]:1080'
    Diagnostic = ''
}
$created = Sync-RouterManagedProxy `
    -Session ([pscustomobject]@{}) `
    -ProxySettings $proxySettings `
    -RequestInvoker $requestInvoker
Assert-True ($created.Action -eq 'created') 'Managed proxy was not created.'
Assert-True ($created.DesiredProxyId -eq 41) 'Created proxy ID was not returned.'
Assert-True ($state.Items[0].protocol -eq 'socks5h') 'Proxy protocol was not preserved.'
Assert-True (
    ([Net.IPAddress]::Parse([string]$state.Items[0].host)).Equals([Net.IPAddress]::IPv6Loopback)
) 'IPv6 proxy host was not stored without brackets.'
Assert-True ($state.Items[0].port -eq 1080) 'Proxy port was not preserved.'
Assert-True ($null -eq $state.Calls[1].Body.PSObject.Properties['password']) 'A password field was sent to Sub2API.'

$proxySettings.ProxyUrl = 'http://proxy.example.test:8080'
$updated = Sync-RouterManagedProxy `
    -Session ([pscustomobject]@{}) `
    -ProxySettings $proxySettings `
    -RequestInvoker $requestInvoker
Assert-True ($updated.Action -eq 'updated') 'Changed managed proxy was not updated in place.'
Assert-True ($updated.DesiredProxyId -eq 41) 'Managed proxy identity changed during update.'
Assert-True ($state.Items[0].host -eq 'proxy.example.test') 'Managed proxy host was not updated.'

$direct = Sync-RouterManagedProxy `
    -Session ([pscustomobject]@{}) `
    -ProxySettings ([pscustomobject]@{ Mode = 'direct'; Source = 'direct'; ProxyUrl = $null; Diagnostic = '' }) `
    -RequestInvoker $requestInvoker
Assert-True ($direct.ManagedProxyId -eq 41 -and $direct.DesiredProxyId -eq 0) 'Direct mode did not retain the managed ID for safe unassignment.'

$credentialRejected = $false
try {
    [void](Sync-RouterManagedProxy `
        -Session ([pscustomobject]@{}) `
        -ProxySettings ([pscustomobject]@{ Mode = 'proxy'; Source = 'explicit'; ProxyUrl = 'http://user:secret@proxy.example.test:8080'; Diagnostic = '' }) `
        -RequestInvoker $requestInvoker)
} catch {
    $credentialRejected = $_.Exception.Message -match 'CREDENTIAL_STORAGE_UNSUPPORTED'
}
Assert-True $credentialRejected 'Authenticated proxy was not rejected before persistence.'

$assign = Get-RouterAccountProxyReconciliation `
    -CurrentProxyId $null -RouterManagedProxyIds @(41, 17897) -DesiredProxyId 41 -ShouldUseManagedProxy $true
Assert-True ($assign.Action -eq 'assign' -and $assign.ProxyId -eq 41) 'Unconfigured account was not assigned.'
$replace = Get-RouterAccountProxyReconciliation `
    -CurrentProxyId 17897 -RouterManagedProxyIds @(41, 17897) -DesiredProxyId 41 -ShouldUseManagedProxy $true
Assert-True ($replace.Action -eq 'replace' -and $replace.ProxyId -eq 41) 'Legacy managed proxy was not replaced.'
$preserve = Get-RouterAccountProxyReconciliation `
    -CurrentProxyId 99 -RouterManagedProxyIds @(41, 17897) -DesiredProxyId 41 -ShouldUseManagedProxy $true
Assert-True ($preserve.Action -eq 'preserve-custom' -and $preserve.ProxyId -eq 99) 'Custom account proxy was overwritten.'
$clear = Get-RouterAccountProxyReconciliation `
    -CurrentProxyId 41 -RouterManagedProxyIds @(41, 17897) -DesiredProxyId 0 -ShouldUseManagedProxy $false
Assert-True ($clear.Action -eq 'clear' -and $clear.ProxyId -eq 0) 'Router-managed proxy was not cleared with proxy_id=0.'

Write-Output 'Managed proxy tests passed.'
