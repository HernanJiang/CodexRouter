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

$aclRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-acl-' + [Guid]::NewGuid().ToString('N'))
try {
    $nested = Join-Path $aclRoot 'nested'
    [IO.Directory]::CreateDirectory($nested) | Out-Null
    [IO.File]::WriteAllText((Join-Path $nested 'probe.txt'), 'acl-test')
    Protect-RouterPathAcl -Path $aclRoot -Recurse
    Import-Module Microsoft.PowerShell.Security -Force -ErrorAction Stop
    $directoryAcl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath $aclRoot
    $fileAcl = Microsoft.PowerShell.Security\Get-Acl -LiteralPath (Join-Path $nested 'probe.txt')
    Assert-Equal $true $directoryAcl.AreAccessRulesProtected 'The protected directory still inherits access rules.'
    Assert-Equal $true $fileAcl.AreAccessRulesProtected 'The protected file still inherits access rules.'
} finally {
    if ([IO.Directory]::Exists($aclRoot)) {
        [IO.Directory]::Delete($aclRoot, $true)
    }
}

Write-Output 'Credential store portability tests passed.'
