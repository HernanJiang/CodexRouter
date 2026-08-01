$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Get-RouterCredential -Name 'AdminPassword' | Set-Clipboard
Write-Output 'Sub2API admin password copied to the clipboard.'
