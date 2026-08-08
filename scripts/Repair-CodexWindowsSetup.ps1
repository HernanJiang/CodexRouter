#Requires -Version 5.1
param(
    [switch]$RepairAcl
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Repair Codex Desktop's one-time Windows elevated setup marker without printing
# secrets. The official helper often fails while granting ACLs on the MSIX
# WindowsApps package path (SetNamedSecurityInfoW error 5). When sandbox users
# and setup_marker already exist, clearing the stale error and restoring
# [windows].sandbox = "elevated" lets the desktop skip the login -> UAC loop.

function Get-CodexHome {
    if (-not [string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
        return [IO.Path]::GetFullPath($env:CODEX_HOME)
    }
    return (Join-Path $env:USERPROFILE '.codex')
}

function Ensure-ElevatedSandboxMarker {
    param([Parameter(Mandatory = $true)][string]$ConfigPath)

    if (-not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
        throw "Codex config.toml was not found: $ConfigPath"
    }

    $text = [IO.File]::ReadAllText($ConfigPath)
    if ($text -match '(?ms)^\[windows\][^\[]*?^sandbox\s*=\s*"elevated"\s*$') {
        return $false
    }

    if ($text -match '(?m)^sandbox\s*=\s*".*"\s*$' -and $text -match '(?m)^\[windows\]\s*$') {
        $updated = [regex]::Replace($text, '(?m)^sandbox\s*=\s*".*"\s*$', 'sandbox = "elevated"')
    }
    elseif ($text -match '(?m)^\[windows\]\s*$') {
        $updated = [regex]::Replace($text, '(?m)^\[windows\]\s*$', "[windows]`r`nsandbox = `"elevated`"")
    }
    else {
        $updated = $text.TrimEnd() + "`r`n`r`n[windows]`r`nsandbox = `"elevated`"`r`n"
    }

    $temp = "$ConfigPath.tmp"
    [IO.File]::WriteAllText($temp, $updated)
    Move-Item -LiteralPath $temp -Destination $ConfigPath -Force
    return $true
}

function Test-PathHasIdentityReadAccess {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][Security.Principal.SecurityIdentifier]$Sid
    )

    foreach ($rule in (Get-Acl -LiteralPath $Path).Access) {
        try {
            $ruleSid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier])
        }
        catch { continue }
        if ($ruleSid -ne $Sid -or $rule.AccessControlType -ne 'Allow') { continue }
        $readMask = [Security.AccessControl.FileSystemRights]::ReadAndExecute
        if (($rule.FileSystemRights -band $readMask) -eq $readMask) { return $true }
    }
    return $false
}

function Repair-CodexPackageReadAcl {
    $package = Get-AppxPackage -Name 'OpenAI.Codex' -ErrorAction Stop |
        Sort-Object Version -Descending |
        Select-Object -First 1
    if ($null -eq $package -or [string]::IsNullOrWhiteSpace($package.InstallLocation)) {
        throw 'The installed OpenAI.Codex package could not be found.'
    }
    $appPath = Join-Path $package.InstallLocation 'app'
    if (-not (Test-Path -LiteralPath $appPath -PathType Container)) {
        throw "The OpenAI.Codex app directory was not found: $appPath"
    }
    $group = Get-LocalGroup -Name 'CodexSandboxUsers' -ErrorAction Stop
    $groupSid = [Security.Principal.SecurityIdentifier]$group.SID
    $hasReadAccess = Test-PathHasIdentityReadAccess -Path $appPath -Sid $groupSid
    $systemSid = [Security.Principal.SecurityIdentifier]'S-1-5-18'
    $ownerSid = (Get-Acl -LiteralPath $appPath).Owner |
        ForEach-Object { ([Security.Principal.NTAccount]$_).Translate([Security.Principal.SecurityIdentifier]) }
    if ($hasReadAccess -and $ownerSid -eq $systemSid) {
        Write-Output 'CodexSandboxUsers already has package read access and the owner is SYSTEM.'
        return
    }
    if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
            [Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Administrator elevation is required to repair the Codex package ACL.'
    }

    # The MSIX package root is owned by SYSTEM and grants Administrators only RX.
    # Take ownership of this one app directory, add the missing sandbox-group ACE,
    # then immediately restore the original SYSTEM owner.
    if (-not $hasReadAccess) {
        & "$env:SystemRoot\System32\takeown.exe" /F $appPath /A | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "takeown failed with exit code $LASTEXITCODE" }
    }
    try {
        if (-not $hasReadAccess) {
            & "$env:SystemRoot\System32\icacls.exe" $appPath /grant "*$($groupSid.Value):(OI)(CI)(RX)" | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "icacls grant failed with exit code $LASTEXITCODE" }
        }
    }
    finally {
        $acl = Get-Acl -LiteralPath $appPath
        $acl.SetOwner($systemSid)
        Set-Acl -LiteralPath $appPath -AclObject $acl
    }
    if (-not (Test-PathHasIdentityReadAccess -Path $appPath -Sid $groupSid)) {
        throw 'CodexSandboxUsers read access is still missing after ACL repair.'
    }
    $finalOwner = ([Security.Principal.NTAccount](Get-Acl -LiteralPath $appPath).Owner).Translate(
        [Security.Principal.SecurityIdentifier])
    if ($finalOwner -ne $systemSid) {
        throw 'The Codex app owner was not restored to SYSTEM.'
    }
    Write-Output 'Repaired CodexSandboxUsers read access and restored the package owner to SYSTEM.'
}

$codexHome = Get-CodexHome
$sandboxDir = Join-Path $codexHome '.sandbox'
$markerPath = Join-Path $sandboxDir 'setup_marker.json'
$errorPath = Join-Path $sandboxDir 'setup_error.json'
$configPath = Join-Path $codexHome 'config.toml'
$usersPath = Join-Path $codexHome '.sandbox-secrets\sandbox_users.json'

if ($RepairAcl) {
    Repair-CodexPackageReadAcl
}

$offline = Get-LocalUser -Name 'CodexSandboxOffline' -ErrorAction SilentlyContinue
$online = Get-LocalUser -Name 'CodexSandboxOnline' -ErrorAction SilentlyContinue
if (-not $offline -or -not $online) {
    throw 'Codex sandbox local users are missing. Open ChatGPT/Codex once and approve the Windows setup UAC prompt first.'
}
if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
    throw "Codex setup marker is missing: $markerPath"
}
if (-not (Test-Path -LiteralPath $usersPath -PathType Leaf)) {
    throw "Codex sandbox user secrets are missing under .sandbox-secrets."
}

$removedError = $false
if (Test-Path -LiteralPath $errorPath -PathType Leaf) {
    $raw = [IO.File]::ReadAllText($errorPath)
    Remove-Item -LiteralPath $errorPath -Force
    $removedError = $true
    Write-Output "Removed stale setup_error.json ($(($raw -replace '\s+', ' ').Trim()))"
}

$wroteMarker = Ensure-ElevatedSandboxMarker -ConfigPath $configPath
if ($wroteMarker) {
    Write-Output 'Restored [windows].sandbox = "elevated" in config.toml'
}
else {
    Write-Output 'config.toml already contains [windows].sandbox = "elevated"'
}

Write-Output "Codex home: $codexHome"
Write-Output 'Repair complete. Fully quit ChatGPT/Codex and reopen it.'
if (-not $removedError -and -not $wroteMarker) {
    Write-Output 'No changes were required.'
}
