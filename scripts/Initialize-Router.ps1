Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\UserData.psm1" -Force
$userDataRoot = Get-RouterUserDataRoot -RouterRoot $routerRoot
$dataRoot = Get-RouterDataRoot -RouterRoot $routerRoot

# 2.0 stack layout: the Router Host owns its own bootstrap (management
# secret, local API key, SQLite state) on first start; this step prepares the
# directories, verifies the portable payload, and provisions the local
# administrator credential the compatibility admin API requires.
foreach ($directory in @($dataRoot, (Join-Path $dataRoot 'pids'), (Join-Path $routerRoot 'logs'))) {
    [IO.Directory]::CreateDirectory($directory) | Out-Null
}
foreach ($requiredFile in @(
    'app\codex-router-host.exe',
    'app\cli-proxy-api.exe',
    'app\plugins\windows\amd64\gemini-cli-v1.0.5.dll'
)) {
    if (-not (Test-Path -LiteralPath (Join-Path $routerRoot $requiredFile))) {
        throw "Portable runtime is incomplete; missing: $requiredFile"
    }
}

function New-RandomHex([int]$Bytes) {
    $buffer = [byte[]]::new($Bytes)
    $generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $generator.GetBytes($buffer)
        return ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant()
    } finally {
        $generator.Dispose()
        [Array]::Clear($buffer, 0, $buffer.Length)
    }
}

if ($null -eq (Get-RouterCredential -Name 'AdminPassword' -AllowMissing)) {
    Set-RouterCredential -Name 'AdminPassword' -Secret (New-RandomHex -Bytes 24)
}

$aclMarker = Join-Path $dataRoot '.acl-protected-v2'
if (-not (Test-Path -LiteralPath $aclMarker)) {
    if (Test-RouterPathAclSupport -Path $userDataRoot) {
        # Stable user data contains the local state database and OAuth
        # material, so ACL hardening here is part of successful
        # initialization.
        foreach ($resolved in @($dataRoot, (Join-Path $userDataRoot 'backups'))) {
            if (-not (Test-Path -LiteralPath $resolved)) { continue }
            try {
                Protect-RouterPathAcl -Path $resolved -Recurse
            } catch {
                # ACL hardening must never block first-run Router startup. Secrets
                # remain in Windows Credential Manager even if NTFS ACLs cannot be
                # applied on this machine/PowerShell host.
                Write-Warning ("ROUTER_USERDATA_ACL_SKIPPED: Could not harden '{0}': {1}" -f $resolved, $_.Exception.Message)
            }
        }

        # Package directories may be read-only or inherit ACLs the current
        # user cannot replace. Their hardening is useful, but must not prevent
        # a portable build from starting on an otherwise supported machine.
        foreach ($resolved in @($routerRoot, (Join-Path $routerRoot 'logs'))) {
            if (-not (Test-Path -LiteralPath $resolved)) { continue }
            try {
                if (Test-RouterPathAclSupport -Path $resolved) {
                    Protect-RouterPathAcl -Path $resolved -Recurse
                }
            } catch {
                Write-Warning ("ROUTER_PACKAGE_ACL_SKIPPED: Could not harden '{0}': {1}" -f $resolved, $_.Exception.Message)
            }
        }

        $markerBytes = [Text.Encoding]::ASCII.GetBytes('current-user-only')
        try { Write-RouterFileAtomic -Path $aclMarker -Bytes $markerBytes }
        finally { [Array]::Clear($markerBytes, 0, $markerBytes.Length) }
    } else {
        Write-Warning 'ROUTER_ACL_UNSUPPORTED: The user-data drive does not support Windows ACLs. Credentials remain protected by Windows Credential Manager/DPAPI, but local state files cannot be restricted to the current user.'
    }
}

Write-Output 'Codex Router secrets and data directory are initialized.'