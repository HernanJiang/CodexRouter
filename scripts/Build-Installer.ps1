param(
    [Parameter(Mandatory)][string]$StagePath,
    [Parameter(Mandatory)][string]$ArchivePath,
    [Parameter(Mandatory)][string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$stage = [IO.Path]::GetFullPath($StagePath)
$archive = [IO.Path]::GetFullPath($ArchivePath)
$output = [IO.Path]::GetFullPath($OutputPath)
$iexpress = (Get-Command iexpress.exe -ErrorAction Stop).Source

if (-not (Test-Path -LiteralPath $stage -PathType Container)) { throw "Release stage is missing: $stage" }
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) { throw "Release archive is missing: $archive" }
if ([IO.Path]::GetExtension($output) -ne '.exe') { throw 'Installer output must be an .exe file.' }
[IO.Directory]::CreateDirectory((Split-Path -Parent $output)) | Out-Null

$work = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-iexpress-' + [Guid]::NewGuid().ToString('N'))
$payload = Join-Path $work 'payload'
$sedPath = Join-Path $work 'Codex-Router-1.4.9.sed'
try {
    [IO.Directory]::CreateDirectory($payload) | Out-Null
    Copy-Item -LiteralPath $archive -Destination (Join-Path $payload (Split-Path -Leaf $archive)) -Force
    Copy-Item -LiteralPath (Join-Path $routerRoot 'scripts\Install-CodexRouter.ps1') -Destination $payload -Force

    $archiveName = Split-Path -Leaf $archive
    $sed = @"
[Version]
Class=IEXPRESS
SEDVersion=3
[Options]
PackagePurpose=InstallApp
ShowInstallProgramWindow=1
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
AdminQuietInstCmd=
UserQuietInstCmd=
SourceFiles=SourceFiles
[SourceFiles]
SourceFiles0=$payload\
[SourceFiles0]
%FILE0%=
%FILE1%=
[Strings]
InstallPrompt="Install Codex-Router 1.4.9 for the current Windows user."
DisplayLicense=""
FinishMessage="Codex-Router 1.4.9 was installed. Your existing user data was preserved."
FriendlyName="Codex-Router 1.4.9"
AppLaunched="PowerShell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File Install-CodexRouter.ps1 -PackageZip $archiveName"
PostInstallCmd="<None>"
FILE0="$archiveName"
FILE1="Install-CodexRouter.ps1"
"@
    [IO.File]::WriteAllText($sedPath, $sed, [Text.Encoding]::ASCII)

    $process = Start-Process -FilePath $iexpress -ArgumentList @('/N', $sedPath) -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -ne 0) { throw "IExpress failed with exit code $($process.ExitCode)" }
    if (-not (Test-Path -LiteralPath $output -PathType Leaf)) { throw "IExpress did not create the installer: $output" }

    [ordered]@{
        installer = $output
        version = '1.4.9'
        payloadArchive = $archiveName
        sha256 = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
        signed = $false
        installScope = 'per-user'
    } | ConvertTo-Json -Compress
} finally {
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
}
