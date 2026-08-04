param(
    [Parameter(Mandatory)]
    [ValidateRange(1, [long]::MaxValue)]
    [long]$AccountId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

$session = New-RouterAdminSession
try {
    $account = Get-RouterResponseData (
        Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$AccountId"
    )
    if ([string]$account.type -ne 'oauth') {
        throw "Account $AccountId is not an OAuth account; deletion was refused."
    }

    [void](Invoke-RouterApi `
        -Session $session `
        -Method DELETE `
        -Path "/api/v1/admin/accounts/$AccountId")

    [ordered]@{
        accountId = $AccountId
        name = [string]$account.name
        platform = [string]$account.platform
        status = 'revoked'
    } | ConvertTo-Json -Compress
} finally {
    if ($session -and $session.Headers) { $session.Headers.Clear() }
    $account = $null
}
