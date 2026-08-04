[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$StagePath,
    [Parameter(Mandatory)][string]$ArchivePath,
    [switch]$SkipAcceptance
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$stage = [IO.Path]::GetFullPath($StagePath)
$archive = [IO.Path]::GetFullPath($ArchivePath)
$repository = 'HernanJiang/Codex-Router'
if (-not (Test-Path -LiteralPath $stage -PathType Container)) { throw 'Release stage does not exist.' }
if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) { throw 'Release archive does not exist.' }

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
    gh release upload $tag $archive --repo $repository --clobber
} else {
    gh release create $tag $archive `
        --repo $repository `
        --target $head `
        --title "Codex-Router $tag" `
        --generate-notes
}
if ($LASTEXITCODE -ne 0) { throw 'Could not publish the GitHub Release asset.' }

$releaseInfo = gh release view $tag --repo $repository --json assets,url | ConvertFrom-Json
$assetName = [IO.Path]::GetFileName($archive)
$assets = @($releaseInfo.assets | Where-Object { $_.name -eq $assetName })
if ($assets.Count -ne 1 -or [long]$assets[0].size -ne (Get-Item -LiteralPath $archive).Length) {
    throw 'The published GitHub asset size does not match the verified local archive.'
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
    asset = $assetName
    bytes = [long](Get-Item -LiteralPath $archive).Length
    updateCheck = 'passed'
    updateDownloadHash = 'passed'
    url = [string]$releaseInfo.url
} | ConvertTo-Json -Compress
