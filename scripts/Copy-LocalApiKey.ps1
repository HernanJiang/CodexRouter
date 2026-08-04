$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
$key = Get-RouterCredential -Name 'LocalApiKey'
try {
    $key | Set-Clipboard
    Write-Output 'Codex-Router local API key copied to the clipboard.'
} finally {
    $key = $null
}
