param(
    [Parameter(Mandatory)][long]$AccountId,
    [Parameter(Mandatory)][ValidateRange(1, 999)][int]$Priority
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

$session = New-RouterAdminSession
try {
    $detail = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$AccountId")
    $type = [string]$detail.type
    if ($type -ne 'oauth') {
        throw "Account $AccountId is not an OAuth account."
    }
    [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/accounts/$AccountId" -Body @{
        priority = $Priority
        confirm_mixed_channel_risk = $true
    })
    $updated = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$AccountId")
    [ordered]@{
        id = [long]$updated.id
        name = [string]$updated.name
        platform = [string]$updated.platform
        priority = [int]$updated.priority
    } | ConvertTo-Json -Compress
} finally {
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
