param(
    [Parameter(Mandatory)][string]$StagePath,
    [Parameter(Mandatory)][string]$ArchivePath,
    [Parameter(Mandatory)][string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$stage = [IO.Path]::GetFullPath($StagePath)
$archive = [IO.Path]::GetFullPath($ArchivePath)
$output = [IO.Path]::GetFullPath($OutputPath)
$iexpress = (Get-Command iexpress.exe -ErrorAction Stop).Source
$dependencyManifestPath = Join-Path $stage 'dependency-manifest.json'
if (-not (Test-Path -LiteralPath $dependencyManifestPath -PathType Leaf)) {
    throw "Release dependency manifest is missing: $dependencyManifestPath"
}
$dependencyManifest = Get-Content -LiteralPath $dependencyManifestPath -Raw | ConvertFrom-Json
$routerComponents = @($dependencyManifest.components | Where-Object { [string]$_.name -eq 'Codex-Router' })
if ($routerComponents.Count -ne 1) {
    throw 'Release dependency manifest must contain exactly one Codex-Router component.'
}
$version = [string]$routerComponents[0].version
if ($version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Release dependency manifest has an invalid Codex-Router version: $version"
}

function Copy-VersionResource {
    param(
        [Parameter(Mandatory)][string]$SourcePath,
        [Parameter(Mandatory)][string]$DestinationPath
    )

    if (-not ('CodexRouter.Build.VersionResourceCopier' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace CodexRouter.Build
{
    public static class VersionResourceCopier
    {
        private const uint LoadLibraryAsDataFile = 0x00000002;
        private const uint LoadLibraryAsImageResource = 0x00000020;
        private static readonly IntPtr VersionResourceType = new IntPtr(16);
        private static readonly IntPtr VersionResourceName = new IntPtr(1);

        private delegate bool EnumResourceLanguageCallback(
            IntPtr module,
            IntPtr type,
            IntPtr name,
            ushort language,
            IntPtr parameter);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr LoadLibraryExW(string fileName, IntPtr file, uint flags);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr FindResourceW(IntPtr module, IntPtr name, IntPtr type);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint SizeofResource(IntPtr module, IntPtr resourceInfo);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LoadResource(IntPtr module, IntPtr resourceInfo);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LockResource(IntPtr resourceData);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool FreeLibrary(IntPtr module);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool EnumResourceLanguagesW(
            IntPtr module,
            IntPtr type,
            IntPtr name,
            EnumResourceLanguageCallback callback,
            IntPtr parameter);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr BeginUpdateResourceW(string fileName, bool deleteExistingResources);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool UpdateResourceW(
            IntPtr update,
            IntPtr type,
            IntPtr name,
            ushort language,
            IntPtr data,
            uint dataLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool EndUpdateResourceW(IntPtr update, bool discard);

        private static Win32Exception Failure(string operation)
        {
            return new Win32Exception(Marshal.GetLastWin32Error(), operation + " failed");
        }

        private static ushort[] GetVersionResourceLanguages(string path)
        {
            IntPtr module = LoadLibraryExW(
                path,
                IntPtr.Zero,
                LoadLibraryAsDataFile | LoadLibraryAsImageResource);
            if (module == IntPtr.Zero) throw Failure("LoadLibraryExW");

            try
            {
                var languages = new List<ushort>();
                EnumResourceLanguageCallback callback = delegate(
                    IntPtr callbackModule,
                    IntPtr callbackType,
                    IntPtr callbackName,
                    ushort language,
                    IntPtr parameter)
                {
                    languages.Add(language);
                    return true;
                };
                if (!EnumResourceLanguagesW(
                    module,
                    VersionResourceType,
                    VersionResourceName,
                    callback,
                    IntPtr.Zero))
                {
                    throw Failure("EnumResourceLanguagesW");
                }
                return languages.ToArray();
            }
            finally
            {
                FreeLibrary(module);
            }
        }

        public static void Copy(string sourcePath, string destinationPath)
        {
            ushort[] destinationLanguages = GetVersionResourceLanguages(destinationPath);
            byte[] versionData;
            IntPtr module = LoadLibraryExW(
                sourcePath,
                IntPtr.Zero,
                LoadLibraryAsDataFile | LoadLibraryAsImageResource);
            if (module == IntPtr.Zero) throw Failure("LoadLibraryExW");

            try
            {
                IntPtr resourceInfo = FindResourceW(module, VersionResourceName, VersionResourceType);
                if (resourceInfo == IntPtr.Zero) throw Failure("FindResourceW");
                uint size = SizeofResource(module, resourceInfo);
                if (size == 0) throw Failure("SizeofResource");
                IntPtr resource = LoadResource(module, resourceInfo);
                if (resource == IntPtr.Zero) throw Failure("LoadResource");
                IntPtr data = LockResource(resource);
                if (data == IntPtr.Zero) throw Failure("LockResource");
                versionData = new byte[size];
                Marshal.Copy(data, versionData, 0, checked((int)size));
            }
            finally
            {
                FreeLibrary(module);
            }

            IntPtr update = BeginUpdateResourceW(destinationPath, false);
            if (update == IntPtr.Zero) throw Failure("BeginUpdateResourceW");

            bool committed = false;
            try
            {
                GCHandle pinned = GCHandle.Alloc(versionData, GCHandleType.Pinned);
                try
                {
                    foreach (ushort language in destinationLanguages)
                    {
                        if (!UpdateResourceW(
                            update,
                            VersionResourceType,
                            VersionResourceName,
                            language,
                            pinned.AddrOfPinnedObject(),
                            checked((uint)versionData.Length)))
                        {
                            throw Failure("UpdateResourceW");
                        }
                    }
                }
                finally
                {
                    pinned.Free();
                }

                if (!EndUpdateResourceW(update, false)) throw Failure("EndUpdateResourceW");
                committed = true;
            }
            finally
            {
                if (!committed) EndUpdateResourceW(update, true);
            }
        }
    }
}
'@
    }

    [CodexRouter.Build.VersionResourceCopier]::Copy($SourcePath, $DestinationPath)
}

if (-not (Test-Path -LiteralPath $stage -PathType Container)) { throw "Release stage is missing: $stage" }
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) { throw "Release archive is missing: $archive" }
if ([IO.Path]::GetExtension($output) -ne '.exe') { throw 'Installer output must be an .exe file.' }
[IO.Directory]::CreateDirectory((Split-Path -Parent $output)) | Out-Null

$work = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-iexpress-' + [Guid]::NewGuid().ToString('N'))
$payload = Join-Path $work 'payload'
$sedPath = Join-Path $work "Codex-Router-$version.sed"
try {
    [IO.Directory]::CreateDirectory($payload) | Out-Null
    Copy-Item -LiteralPath $archive -Destination (Join-Path $payload (Split-Path -Leaf $archive)) -Force
    Copy-Item -LiteralPath (Join-Path $stage 'Codex-Router.exe') -Destination (Join-Path $payload 'Codex-Router-Setup.exe') -Force
    $setupRuntimeFiles = @('VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'MSVCP140.dll')
    foreach ($runtimeName in $setupRuntimeFiles) {
        $runtimePath = Join-Path $stage $runtimeName
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "Native installer runtime is missing: $runtimePath"
        }
        Copy-Item -LiteralPath $runtimePath -Destination $payload -Force
    }

    $archiveName = Split-Path -Leaf $archive
    $launcherName = 'Install-Codex-Router.cmd'
    $launcher = @"
@echo off
start "" /wait "%~dp0Codex-Router-Setup.exe" --install-portable "--install-package=%~dp0$archiveName" --install-version=$version
exit /b %errorlevel%
"@
    [IO.File]::WriteAllText(
        (Join-Path $payload $launcherName),
        $launcher,
        [Text.Encoding]::ASCII
    )
    $sed = @"
[Version]
Class=IEXPRESS
SEDVersion=3
[Options]
PackagePurpose=InstallApp
ShowInstallProgramWindow=0
HideExtractAnimation=1
UseLongFileName=1
InsideCompressed=1
CAB_FixedSize=0
CAB_ResvCodeSigning=0
RebootMode=N
InstallPrompt=%InstallPrompt%
DisplayLicense=%DisplayLicense%
FinishMessage=%FinishMessage%
TargetName=$output
FriendlyName=%FriendlyName%
AppLaunched=%AppLaunched%
PostInstallCmd=%PostInstallCmd%
AdminQuietInstCmd=%AppLaunched%
UserQuietInstCmd=%AppLaunched%
SourceFiles=SourceFiles
[SourceFiles]
SourceFiles0=$payload\
[SourceFiles0]
%FILE0%=
%FILE1%=
%FILE2%=
%FILE3%=
%FILE4%=
%FILE5%=
[Strings]
InstallPrompt="Install Codex-Router $version for the current Windows user. Publisher: Hernan_JIANG."
DisplayLicense=""
FinishMessage="Codex-Router $version was installed. Your existing user data was preserved."
FriendlyName="Codex-Router $version by Hernan_JIANG"
AppLaunched="$launcherName"
PostInstallCmd="<None>"
FILE0="$archiveName"
FILE1="Codex-Router-Setup.exe"
FILE2="VCRUNTIME140.dll"
FILE3="VCRUNTIME140_1.dll"
FILE4="MSVCP140.dll"
FILE5="$launcherName"
"@
    [IO.File]::WriteAllText($sedPath, $sed, [Text.Encoding]::ASCII)

    $process = Start-Process -FilePath $iexpress -ArgumentList @('/N', $sedPath) -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -ne 0) { throw "IExpress failed with exit code $($process.ExitCode)" }
    if (-not (Test-Path -LiteralPath $output -PathType Leaf)) { throw "IExpress did not create the installer: $output" }

    Copy-VersionResource -SourcePath (Join-Path $stage 'Codex-Router.exe') -DestinationPath $output
    $versionInfo = (Get-Item -LiteralPath $output).VersionInfo
    if ([string]$versionInfo.ProductVersion -ne $version -or
        [string]$versionInfo.FileVersion -ne $version -or
        [string]$versionInfo.CompanyName -ne 'Hernan_JIANG' -or
        [string]$versionInfo.ProductName -ne 'Codex-Router') {
        throw "Installer version resource does not match Codex-Router $version metadata."
    }

    [ordered]@{
        installer = $output
        version = $version
        publisher = 'Hernan_JIANG'
        payloadArchive = $archiveName
        sha256 = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
        signed = $false
        installScope = 'per-user'
    } | ConvertTo-Json -Compress
} finally {
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
}
