Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force

if (-not ('CodexRouter.CredentialNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

namespace CodexRouter {
    public static class CredentialNative {
        private const int CRED_TYPE_GENERIC = 1;
        private const int CRED_PERSIST_LOCAL_MACHINE = 2;

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct CREDENTIAL {
            public int Flags;
            public int Type;
            public string TargetName;
            public string Comment;
            public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
            public int CredentialBlobSize;
            public IntPtr CredentialBlob;
            public int Persist;
            public int AttributeCount;
            public IntPtr Attributes;
            public string TargetAlias;
            public string UserName;
        }

        [DllImport("advapi32.dll", EntryPoint = "CredWriteW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredWrite(ref CREDENTIAL credential, int flags);

        [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredRead(string target, int type, int flags, out IntPtr credential);

        [DllImport("advapi32.dll", EntryPoint = "CredDeleteW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredDelete(string target, int type, int flags);

        [DllImport("advapi32.dll", SetLastError = false)]
        private static extern void CredFree(IntPtr buffer);

        [DllImport("kernel32.dll", EntryPoint = "MoveFileExW", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool MoveFileEx(string source, string destination, int flags);

        public static void Write(string target, string userName, string secret) {
            byte[] blob = Encoding.Unicode.GetBytes(secret ?? string.Empty);
            if (blob.Length > 2560) throw new ArgumentOutOfRangeException("secret", "Credential exceeds 2560 bytes.");
            IntPtr blobPtr = Marshal.AllocHGlobal(blob.Length == 0 ? 1 : blob.Length);
            try {
                if (blob.Length > 0) Marshal.Copy(blob, 0, blobPtr, blob.Length);
                var credential = new CREDENTIAL {
                    Type = CRED_TYPE_GENERIC,
                    TargetName = target,
                    UserName = userName,
                    CredentialBlob = blobPtr,
                    CredentialBlobSize = blob.Length,
                    Persist = CRED_PERSIST_LOCAL_MACHINE,
                    Comment = "Codex Router local secret"
                };
                if (!CredWrite(ref credential, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
            } finally {
                Marshal.FreeHGlobal(blobPtr);
                Array.Clear(blob, 0, blob.Length);
            }
        }

        public static string Read(string target) {
            IntPtr pointer;
            if (!CredRead(target, CRED_TYPE_GENERIC, 0, out pointer)) {
                int error = Marshal.GetLastWin32Error();
                if (error == 1168) return null;
                throw new Win32Exception(error);
            }
            try {
                var credential = (CREDENTIAL)Marshal.PtrToStructure(pointer, typeof(CREDENTIAL));
                if (credential.CredentialBlobSize == 0) return string.Empty;
                byte[] blob = new byte[credential.CredentialBlobSize];
                Marshal.Copy(credential.CredentialBlob, blob, 0, blob.Length);
                try { return Encoding.Unicode.GetString(blob); }
                finally { Array.Clear(blob, 0, blob.Length); }
            } finally {
                CredFree(pointer);
            }
        }

        public static void Delete(string target) {
            if (!CredDelete(target, CRED_TYPE_GENERIC, 0)) {
                int error = Marshal.GetLastWin32Error();
                if (error != 1168) throw new Win32Exception(error);
            }
        }

        public static void AtomicReplace(string source, string destination) {
            const int MOVEFILE_REPLACE_EXISTING = 0x1;
            const int MOVEFILE_WRITE_THROUGH = 0x8;
            if (!MoveFileEx(source, destination, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
                throw new Win32Exception(Marshal.GetLastWin32Error());
            }
        }
    }
}
'@
}

function Set-RouterCredential {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][string]$Secret)
    [CodexRouter.CredentialNative]::Write("CodexRouter/$Name", $env:USERNAME, $Secret)
}

function Get-RouterCredential {
    param([Parameter(Mandatory)][string]$Name, [switch]$AllowMissing)
    $value = [CodexRouter.CredentialNative]::Read("CodexRouter/$Name")
    if ($null -eq $value -and -not $AllowMissing) { throw "Missing Windows credential: CodexRouter/$Name" }
    return $value
}

function Remove-RouterCredential {
    param([Parameter(Mandatory)][string]$Name)
    [CodexRouter.CredentialNative]::Delete("CodexRouter/$Name")
}

function Write-RouterFileAtomic {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][byte[]]$Bytes
    )
    $fullPath = [IO.Path]::GetFullPath($Path)
    $parent = Split-Path -Parent $fullPath
    if ([string]::IsNullOrWhiteSpace($parent)) { throw "Atomic file path has no parent: $Path" }
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = Join-Path $parent ('.' + [IO.Path]::GetFileName($fullPath) + '.' + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        $stream = [IO.FileStream]::new(
            $temporary,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None,
            4096,
            [IO.FileOptions]::WriteThrough)
        try {
            $stream.Write($Bytes, 0, $Bytes.Length)
            $stream.Flush($true)
        } finally {
            $stream.Dispose()
        }
        [CodexRouter.CredentialNative]::AtomicReplace($temporary, $fullPath)
    } finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
        }
    }
}

function Write-RouterTextFileAtomic {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][AllowEmptyString()][string]$Text
    )
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text)
    try { Write-RouterFileAtomic -Path $Path -Bytes $bytes }
    finally { [Array]::Clear($bytes, 0, $bytes.Length) }
}

function Enter-RouterConfigLock {
    param(
        [Parameter(Mandatory)][string]$RouterRoot,
        [ValidateRange(100, 120000)][int]$TimeoutMilliseconds = 10000
    )
    if ($env:CODEX_ROUTER_CONFIG_LOCK_HELD -eq '1') {
        return [pscustomobject]@{ Stream = $null; Inherited = $true }
    }

    $lockDirectory = Join-Path (Get-RouterDataRoot -RouterRoot $RouterRoot) 'locks'
    [IO.Directory]::CreateDirectory($lockDirectory) | Out-Null
    $lockPath = Join-Path $lockDirectory 'config-apply.lock'
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    do {
        try {
            $stream = [IO.File]::Open(
                $lockPath,
                [IO.FileMode]::OpenOrCreate,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None)
            $owner = [Text.Encoding]::ASCII.GetBytes("pid=$PID`r`n")
            try {
                $stream.SetLength(0)
                $stream.Write($owner, 0, $owner.Length)
                $stream.Flush($true)
            } finally {
                [Array]::Clear($owner, 0, $owner.Length)
            }
            return [pscustomobject]@{ Stream = $stream; Inherited = $false }
        } catch [IO.IOException] {
            if ($stopwatch.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
                throw "Timed out waiting for another Router configuration operation."
            }
            Start-Sleep -Milliseconds 75
        }
    } while ($true)
}

function Exit-RouterConfigLock {
    param([AllowNull()]$Lock)
    if ($null -ne $Lock -and $null -ne $Lock.Stream) {
        $Lock.Stream.Dispose()
    }
}

function Enter-RouterLifecycleLock {
    param(
        [Parameter(Mandatory)][string]$RouterRoot,
        [ValidateRange(100, 120000)][int]$TimeoutMilliseconds = 10000,
        [string]$Operation = 'Router lifecycle operation'
    )
    if ($env:CODEX_ROUTER_LIFECYCLE_LOCK_HELD -eq [string]$PID) {
        return [pscustomobject]@{ Stream = $null; Inherited = $true }
    }

    $lockDirectory = Join-Path (Get-RouterDataRoot -RouterRoot $RouterRoot) 'locks'
    [IO.Directory]::CreateDirectory($lockDirectory) | Out-Null
    $lockPath = Join-Path $lockDirectory 'service-lifecycle.lock'
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    do {
        try {
            $stream = [IO.File]::Open(
                $lockPath,
                [IO.FileMode]::OpenOrCreate,
                [IO.FileAccess]::ReadWrite,
                [IO.FileShare]::None)
            $owner = [Text.Encoding]::ASCII.GetBytes(
                "pid=$PID`r`noperation=$Operation`r`n")
            try {
                $stream.SetLength(0)
                $stream.Write($owner, 0, $owner.Length)
                $stream.Flush($true)
            } finally {
                [Array]::Clear($owner, 0, $owner.Length)
            }
            return [pscustomobject]@{ Stream = $stream; Inherited = $false }
        } catch [IO.IOException] {
            if ($stopwatch.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
                throw 'ROUTER_LIFECYCLE_BUSY: Timed out waiting for another Start, Stop, Apply, or OAuth startup operation.'
            }
            Start-Sleep -Milliseconds 75
        }
    } while ($true)
}

function Exit-RouterLifecycleLock {
    param([AllowNull()]$Lock)
    if ($null -ne $Lock -and $null -ne $Lock.Stream) {
        $Lock.Stream.Dispose()
    }
}

function Get-RouterEstablishedConnectionCount {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][int]$Port
    )
    try {
        $connections = @(Get-NetTCPConnection `
            -LocalPort $Port `
            -OwningProcess $ProcessId `
            -ErrorAction Stop)
        return @($connections | Where-Object { $_.State -eq 'Established' }).Count
    } catch {
        throw "ROUTER_LIFECYCLE_SAFETY_CHECK_FAILED: Could not inspect active Sub2API connections for PID $ProcessId on port $Port. No Router service was changed."
    }
}

function Assert-RouterServiceInterruptionAllowed {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$Operation
    )
    $activeConnections = Get-RouterEstablishedConnectionCount `
        -ProcessId $ProcessId `
        -Port $Port
    if ($activeConnections -gt 0) {
        throw "ROUTER_LIFECYCLE_DEFERRED: $Operation was deferred because Sub2API PID $ProcessId has $activeConnections active Established connection(s). Sub2API, Redis, and PostgreSQL were left unchanged; retry after the active requests finish."
    }
}

function Protect-RouterFileWithDpapi {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination,
        [switch]$RemoveSource
    )
    Add-Type -AssemblyName System.Security
    $plain = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Source))
    $protected = $null
    try {
        $protected = [Security.Cryptography.ProtectedData]::Protect(
            $plain,
            $null,
            [Security.Cryptography.DataProtectionScope]::CurrentUser)
        Write-RouterFileAtomic -Path $Destination -Bytes $protected
        if ($RemoveSource) { Remove-Item -LiteralPath $Source -Force }
    } finally {
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
        if ($null -ne $protected) { [Array]::Clear($protected, 0, $protected.Length) }
    }
}

function Unprotect-RouterFileWithDpapi {
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination
    )
    Add-Type -AssemblyName System.Security
    $protected = [IO.File]::ReadAllBytes([IO.Path]::GetFullPath($Source))
    $plain = $null
    try {
        $plain = [Security.Cryptography.ProtectedData]::Unprotect(
            $protected,
            $null,
            [Security.Cryptography.DataProtectionScope]::CurrentUser)
        Write-RouterFileAtomic -Path $Destination -Bytes $plain
    } finally {
        if ($null -ne $protected) { [Array]::Clear($protected, 0, $protected.Length) }
        if ($null -ne $plain) { [Array]::Clear($plain, 0, $plain.Length) }
    }
}

function Test-RouterFileSystemAclSupport {
    param([AllowEmptyString()][string]$FileSystemName)

    if ([string]::IsNullOrWhiteSpace($FileSystemName)) { return $true }
    return $FileSystemName.Trim() -in @('NTFS', 'ReFS')
}

function Test-RouterPathAclSupport {
    param([Parameter(Mandatory)][string]$Path)

    $fullPath = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($fullPath)
    if ([string]::IsNullOrWhiteSpace($root) -or $root.StartsWith('\\')) {
        return $true
    }
    try {
        $drive = [IO.DriveInfo]::new($root)
        if (-not $drive.IsReady) { return $true }
        return Test-RouterFileSystemAclSupport -FileSystemName $drive.DriveFormat
    } catch {
        # Mapped/network providers may not expose a DriveInfo format. Let
        # Set-Acl perform the authoritative check instead of rejecting them.
        return $true
    }
}

function Protect-RouterPathAcl {
    param(
        [Parameter(Mandatory)][string]$Path,
        [switch]$Recurse
    )
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (-not (Test-Path -LiteralPath $fullPath)) { return }
    $currentUser = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $allowed = @(
        $currentUser,
        [Security.Principal.SecurityIdentifier]::new('S-1-5-18'),
        [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')
    )

    function Set-PrivateAcl([string]$Target) {
        $item = Get-Item -LiteralPath $Target -Force
        $isDirectory = $item.PSIsContainer
        $acl = if ($isDirectory) {
            [Security.AccessControl.DirectorySecurity]::new()
        } else {
            [Security.AccessControl.FileSecurity]::new()
        }
        $acl.SetAccessRuleProtection($true, $false)
        $acl.SetOwner($currentUser)
        $inheritance = if ($isDirectory) {
            [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
                [Security.AccessControl.InheritanceFlags]::ObjectInherit
        } else {
            [Security.AccessControl.InheritanceFlags]::None
        }
        foreach ($identity in $allowed) {
            $rule = [Security.AccessControl.FileSystemAccessRule]::new(
                $identity,
                [Security.AccessControl.FileSystemRights]::FullControl,
                $inheritance,
                [Security.AccessControl.PropagationFlags]::None,
                [Security.AccessControl.AccessControlType]::Allow)
            [void]$acl.AddAccessRule($rule)
        }
        # Use Set-Acl on every supported host. FileSystemAclExtensions is not
        # reliably available under Windows PowerShell 5.1, which the GUI uses.
        Set-Acl -LiteralPath $Target -AclObject $acl
    }

    Set-PrivateAcl $fullPath
    if ($Recurse -and (Get-Item -LiteralPath $fullPath -Force).PSIsContainer) {
        Get-ChildItem -LiteralPath $fullPath -Force -Recurse |
            Where-Object { -not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) } |
            ForEach-Object { Set-PrivateAcl $_.FullName }
    }
}

Export-ModuleMember -Function `
    Set-RouterCredential, `
    Get-RouterCredential, `
    Remove-RouterCredential, `
    Write-RouterFileAtomic, `
    Write-RouterTextFileAtomic, `
    Enter-RouterConfigLock, `
    Exit-RouterConfigLock, `
    Enter-RouterLifecycleLock, `
    Exit-RouterLifecycleLock, `
    Get-RouterEstablishedConnectionCount, `
    Assert-RouterServiceInterruptionAllowed, `
    Protect-RouterFileWithDpapi, `
    Unprotect-RouterFileWithDpapi, `
    Test-RouterFileSystemAclSupport, `
    Test-RouterPathAclSupport, `
    Protect-RouterPathAcl
