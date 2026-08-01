Set-StrictMode -Version Latest
$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force

# Codex captures stdout from this command and uses it as the provider bearer token.
Write-Output (Get-RouterCredential -Name 'LocalApiKey')
