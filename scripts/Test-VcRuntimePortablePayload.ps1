param(
    [Parameter(Mandatory)][string]$Stage,
    [string]$BuilderPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$stageRoot = [IO.Path]::GetFullPath($Stage)
if ([string]::IsNullOrWhiteSpace($BuilderPath)) {
    $BuilderPath = Join-Path $PSScriptRoot 'Build-PortableRelease.ps1'
}
if (-not (Test-Path -LiteralPath $stageRoot -PathType Container)) {
    throw "Release stage does not exist: $stageRoot"
}
if (-not (Test-Path -LiteralPath $BuilderPath -PathType Leaf)) {
    throw "Release builder does not exist: $BuilderPath"
}

$validation = @(& $BuilderPath -ValidateStage $stageRoot)

$runtimeNames = @('VCRUNTIME140.dll', 'VCRUNTIME140_1.dll', 'MSVCP140.dll')
$destinationDirectories = @('', 'postgres\pgsql\bin')
$expectedPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$rootHashes = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
$versions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

foreach ($destinationDirectory in $destinationDirectories) {
    foreach ($name in $runtimeNames) {
        $relative = if ([string]::IsNullOrEmpty($destinationDirectory)) { $name } else { "$destinationDirectory\$name" }
        $path = Join-Path $stageRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "VC runtime payload is missing: $relative" }
        $item = Get-Item -LiteralPath $path -Force
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "VC runtime payload is a reparse point: $relative" }
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        $signerSubject = if ($null -eq $signature.SignerCertificate) { '' } else { [string]$signature.SignerCertificate.Subject }
        if ([string]$signature.Status -ne 'Valid' -or
            $signerSubject -notmatch '(?i)(?:^|,\s*)O=Microsoft Corporation(?:,|$)') {
            throw "VC runtime payload does not have a valid Microsoft signature: $relative"
        }
        $version = [string]$item.VersionInfo.ProductVersion
        if ([string]::IsNullOrWhiteSpace($version)) { throw "VC runtime payload has no version: $relative" }
        [void]$versions.Add($version)
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ([string]::IsNullOrEmpty($destinationDirectory)) {
            $rootHashes.Add($name, $hash)
        } elseif ($rootHashes[$name] -ne $hash) {
            throw "VC runtime copies differ between the GUI and PostgreSQL directories: $name"
        }
        [void]$expectedPaths.Add($relative.Replace('\', '/'))
    }
}
if ($versions.Count -ne 1) { throw "VC runtime payload contains multiple versions: $($versions -join ', ')" }

$dependencyPath = Join-Path $stageRoot 'dependency-manifest.json'
$dependency = [IO.File]::ReadAllText($dependencyPath) | ConvertFrom-Json
$components = @($dependency.components | Where-Object { [string]$_.name -eq 'Microsoft Visual C++ Runtime' })
if ($components.Count -ne 1) { throw 'VC runtime dependency component is missing or duplicated.' }
$component = $components[0]
$manifestPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in @($component.files)) { [void]$manifestPaths.Add([string]$entry.path) }
if ($manifestPaths.Count -ne $expectedPaths.Count) { throw 'VC runtime dependency component has the wrong file count.' }
foreach ($relative in $expectedPaths) {
    if (-not $manifestPaths.Contains($relative)) { throw "VC runtime dependency component is missing: $relative" }
}

$postgresVersionOutput = @(& (Join-Path $stageRoot 'postgres\pgsql\bin\postgres.exe') --version 2>&1)
if ($LASTEXITCODE -ne 0 -or ($postgresVersionOutput -join "`n") -notmatch '(?i)postgres') {
    throw "Packaged PostgreSQL loader smoke test failed: $($postgresVersionOutput -join ' ')"
}

[ordered]@{
    stage = $stageRoot
    valid = $true
    version = @($versions)[0]
    files = $expectedPaths.Count
    destinations = $destinationDirectories.Count
    builderValidation = ($validation -join "`n")
    postgresLoaderSmokeTest = ($postgresVersionOutput -join ' ').Trim()
} | ConvertTo-Json -Compress
