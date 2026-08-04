$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
$password = Get-RouterCredential -Name 'AdminPassword'
try {
    "admin@admin.com`r`n$password" | Set-Clipboard
    Write-Output 'Sub2API login copied to the clipboard.'
} finally {
    $password = $null
}
