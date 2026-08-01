Set-StrictMode -Version Latest

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

Export-ModuleMember -Function Set-RouterCredential, Get-RouterCredential, Remove-RouterCredential
