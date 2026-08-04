Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force

function Assert-Equal($Expected, $Actual, [string]$Message) {
    if ($Expected -ne $Actual) {
        throw "$Message Expected '$Expected', got '$Actual'."
    }
}

Assert-Equal $true (Test-RouterFileSystemAclSupport -FileSystemName 'NTFS') 'NTFS ACL support changed.'
Assert-Equal $true (Test-RouterFileSystemAclSupport -FileSystemName 'ReFS') 'ReFS ACL support changed.'
Assert-Equal $false (Test-RouterFileSystemAclSupport -FileSystemName 'exFAT') 'exFAT was incorrectly treated as ACL-capable.'
Assert-Equal $false (Test-RouterFileSystemAclSupport -FileSystemName 'FAT32') 'FAT32 was incorrectly treated as ACL-capable.'
Assert-Equal $true (Test-RouterFileSystemAclSupport -FileSystemName '') 'Unknown/network filesystems must be checked by Set-Acl.'
Assert-Equal $true (Test-RouterPathAclSupport -Path $routerRoot) 'The source volume should support Windows ACLs.'

Write-Output 'Credential store portability tests passed.'
