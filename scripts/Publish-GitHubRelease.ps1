[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$StagePath,
    [Parameter(Mandatory)][string]$ArchivePath,
    [string]$InstallerPath,
    [string]$NotesPath,
    [switch]$SkipAcceptance
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$stage = [IO.Path]::GetFullPath($StagePath)
$archive = [IO.Path]::GetFullPath($ArchivePath)
$installer = if ([string]::IsNullOrWhiteSpace($InstallerPath)) { $null } else { [IO.Path]::GetFullPath($InstallerPath) }
$notes = if ([string]::IsNullOrWhiteSpace($NotesPath)) { $null } else { [IO.Path]::GetFullPath($NotesPath) }
$repository = 'HernanJiang/Codex-Router'
if (-not (Test-Path -LiteralPath $stage -PathType Container)) { throw 'Release stage does not exist.' }
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) { throw 'Release archive does not exist.' }
if ($null -ne $installer -and -not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw 'Release installer does not exist.'
}
if ($null -ne $notes -and -not (Test-Path -LiteralPath $notes -PathType Leaf)) {
    throw 'Release notes do not exist.'
}

$assetPaths = @($archive)
if ($null -ne $installer) { $assetPaths += $installer }
$assetNames = @($assetPaths | ForEach-Object { [IO.Path]::GetFileName($_) })
if (@($assetNames | Select-Object -Unique).Count -ne $assetNames.Count) {
    throw 'Release assets must have unique file names.'
}

$manifestPath = Join-Path $stage 'release-manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Release manifest is missing.' }
$dependencyManifestPath = Join-Path $stage 'dependency-manifest.json'
if (-not (Test-Path -LiteralPath $dependencyManifestPath -PathType Leaf)) {
    throw 'Dependency manifest is missing.'
}
$dependencyManifest = Get-Content -LiteralPath $dependencyManifestPath -Raw | ConvertFrom-Json
$routerComponents = @($dependencyManifest.components | Where-Object { $_.name -eq 'Codex-Router' })
if ($routerComponents.Count -ne 1) {
    throw 'Dependency manifest must contain exactly one Codex-Router component.'
}
$version = [string]$routerComponents[0].version
if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Release manifest version is invalid.' }
$tag = "v$version"

$cargoManifest = Get-Content -LiteralPath (Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml') -Raw
if ($cargoManifest -notmatch '(?m)^version\s*=\s*"([^"]+)"' -or $Matches[1] -ne $version) {
    throw 'Release manifest and Cargo package versions do not match.'
}

$repoInfo = gh repo view $repository --json visibility,isPrivate,nameWithOwner | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not [bool]$repoInfo.isPrivate) {
    throw 'The release repository must remain private during this publishing phase.'
}
$branch = (git branch --show-current).Trim()
if ([string]::IsNullOrWhiteSpace($branch)) { throw 'A local Git branch is required.' }
$changes = @(git status --porcelain=v1 --untracked-files=all)
if ($changes.Count -gt 0) { throw 'Commit or remove all local source changes before publishing.' }

git push origin $branch
if ($LASTEXITCODE -ne 0) { throw 'Could not push the release source branch.' }
$head = (git rev-parse HEAD).Trim()
$remoteHead = (git rev-parse "origin/$branch").Trim()
if ($head -ne $remoteHead) { throw 'The release source commit is not present on GitHub.' }

if (-not $SkipAcceptance) {
    & (Join-Path $PSScriptRoot 'Test-LocalAcceptance.ps1') `
        -StagePath $stage `
        -ArchivePath $archive `
        -FaultInjection `
        -SkipToolchainTests | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Final release acceptance failed.' }
}

$existingRelease = gh release view $tag --repo $repository --json tagName,targetCommitish 2>$null
if ($LASTEXITCODE -eq 0) {
    $release = $existingRelease | ConvertFrom-Json
    if ([string]$release.tagName -ne $tag) { throw 'GitHub returned an unexpected release tag.' }
    gh release upload $tag @assetPaths --repo $repository --clobber
    if ($LASTEXITCODE -eq 0 -and $null -ne $notes) {
        gh release edit $tag --repo $repository --title "Codex-Router $tag" --notes-file $notes
    }
} else {
    $createArguments = @(
        'release', 'create', $tag
    ) + $assetPaths + @(
        '--repo', $repository,
        '--target', $head,
        '--title', "Codex-Router $tag"
    )
    if ($null -ne $notes) {
        $createArguments += @('--notes-file', $notes)
    } else {
        $createArguments += '--generate-notes'
    }
    gh @createArguments
}
if ($LASTEXITCODE -ne 0) { throw 'Could not publish the GitHub Release assets or notes.' }

$releaseInfo = gh release view $tag --repo $repository --json assets,url | ConvertFrom-Json
$publishedAssets = @()
foreach ($assetPath in $assetPaths) {
    $assetName = [IO.Path]::GetFileName($assetPath)
    $assets = @($releaseInfo.assets | Where-Object { $_.name -eq $assetName })
    if ($assets.Count -ne 1 -or [long]$assets[0].size -ne (Get-Item -LiteralPath $assetPath).Length) {
        throw "The published GitHub asset size does not match the verified local file: $assetName"
    }
    $publishedAssets += [ordered]@{
        name = $assetName
        bytes = [long](Get-Item -LiteralPath $assetPath).Length
        sha256 = (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$updateTestRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-private-update-' + [Guid]::NewGuid().ToString('N'))
try {
    $testScripts = Join-Path $updateTestRoot 'scripts'
    [IO.Directory]::CreateDirectory($testScripts) | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'GitHub-Update.ps1') -Destination $testScripts
    $check = & (Join-Path $testScripts 'GitHub-Update.ps1') -Action Check -CurrentVersion '0.0.0' |
        Select-Object -Last 1 |
        ConvertFrom-Json
    if ([string]$check.status -ne 'update_available' -or [string]$check.latestVersion -ne $tag) {
        throw 'The private GitHub update check did not discover the new release.'
    }
    if ([string]$check.assetName -ne [IO.Path]::GetFileName($archive)) {
        throw 'The private GitHub update check selected an unexpected release asset.'
    }
    $download = & (Join-Path $testScripts 'GitHub-Update.ps1') `
        -Action Download `
        -CurrentVersion '0.0.0' `
        -DownloadUrl ([string]$check.downloadUrl) `
        -FileName ([string]$check.assetName) `
        -ExpectedSize ([long]$check.assetSize) |
        Select-Object -Last 1 |
        ConvertFrom-Json
    if ([string]$download.status -ne 'downloaded') {
        throw 'The private GitHub update download did not complete.'
    }
    $localHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    $downloadHash = (Get-FileHash -LiteralPath ([string]$download.downloadPath) -Algorithm SHA256).Hash
    if ($localHash -ne $downloadHash) { throw 'The downloaded private release hash does not match.' }

    if ($null -ne $installer) {
        $installerDownloadRoot = Join-Path $updateTestRoot 'installer'
        [IO.Directory]::CreateDirectory($installerDownloadRoot) | Out-Null
        $installerName = [IO.Path]::GetFileName($installer)
        gh release download $tag --repo $repository --pattern $installerName --dir $installerDownloadRoot --clobber
        if ($LASTEXITCODE -ne 0) { throw 'Could not download the published installer for verification.' }
        $downloadedInstaller = Join-Path $installerDownloadRoot $installerName
        if (-not (Test-Path -LiteralPath $downloadedInstaller -PathType Leaf)) {
            throw 'The published installer download is missing.'
        }
        $localInstallerHash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash
        $downloadedInstallerHash = (Get-FileHash -LiteralPath $downloadedInstaller -Algorithm SHA256).Hash
        if ($localInstallerHash -ne $downloadedInstallerHash) {
            throw 'The downloaded installer hash does not match.'
        }
    }
} finally {
    if (Test-Path -LiteralPath $updateTestRoot) {
        Remove-Item -LiteralPath $updateTestRoot -Recurse -Force
    }
}

[ordered]@{
    repository = $repository
    visibility = 'private'
    tag = $tag
    commit = $head
    assets = $publishedAssets
    updateCheck = 'passed'
    updateDownloadHash = 'passed'
    installerDownloadHash = if ($null -eq $installer) { 'not-requested' } else { 'passed' }
    url = [string]$releaseInfo.url
} | ConvertTo-Json -Compress
