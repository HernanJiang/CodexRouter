Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

$targetEmail = 'admin@admin.com'
$targetPassword = Get-RouterCredential -Name 'AdminPassword'
$session = New-RouterAdminSession
try {
    $user = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/users/1')
    $body = @{
        email = $targetEmail
        password = $targetPassword
        username = [string]$user.username
        notes = [string]$user.notes
        role = [string]$user.role
        concurrency = [int]$user.concurrency
        rpm_limit = [int]$user.rpm_limit
    }
    try {
        $loginSubtitle = '"\u4ec5\u9650 127.0.0.1\uff1b\u767b\u5f55\u51ed\u636e\u7531 Codex-Router \u5b89\u5168\u7ba1\u7406"' | ConvertFrom-Json
        [void](Invoke-RouterApi -Session $session -Method PUT -Path '/api/v1/admin/settings' -Body @{
            site_subtitle = $loginSubtitle
        })
    } catch {
        Write-Warning 'Sub2API administrator was updated, but the login-page hint could not be refreshed.'
    }
    [void](Invoke-RouterApi -Session $session -Method PUT -Path '/api/v1/admin/users/1' -Body $body)
    Write-Output "Sub2API administrator ready: $targetEmail"
} finally {
    $targetPassword = $null
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
