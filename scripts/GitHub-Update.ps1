[CmdletBinding()]
param(
    [ValidateSet('Check', 'Download')]
    [string]$Action = 'Check',
    [string]$CurrentVersion = '',
    [string]$DownloadUrl = '',
    [string]$FileName = '',
    [long]$ExpectedSize = 0
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$OutputEncoding = [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$routerRoot = Split-Path -Parent $PSScriptRoot
$repositoryUrl = 'https://github.com/HernanJiang/CodexRouter'
$releaseApiUrl = 'https://api.github.com/repos/HernanJiang/CodexRouter/releases/latest'
$repositoryName = 'HernanJiang/CodexRouter'

function Get-GitHubCliPath {
    $command = Get-Command gh -ErrorAction SilentlyContinue
    if ($null -eq $command -or [string]::IsNullOrWhiteSpace([string]$command.Source)) {
        throw 'PRIVATE_GITHUB_AUTH_REQUIRED'
    }
    return [string]$command.Source
}

function Get-LatestGitHubRelease {
    try {
        return Invoke-RestMethod -Headers @{
            'User-Agent' = 'CodexRouter-Updater'
            'Accept' = 'application/vnd.github+json'
            'X-GitHub-Api-Version' = '2022-11-28'
        } -Uri $releaseApiUrl -TimeoutSec 30
    } catch {
        $statusCode = 0
        if ($null -ne $_.Exception.Response -and $null -ne $_.Exception.Response.StatusCode) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }
        if ($statusCode -notin @(403, 404)) { throw }
    }

    $gh = Get-GitHubCliPath
    $json = & $gh api "repos/$repositoryName/releases/latest" 2>$null
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace(($json -join "`n"))) {
        throw 'PRIVATE_GITHUB_AUTH_REQUIRED'
    }
    return ($json -join "`n") | ConvertFrom-Json
}

function Receive-PrivateReleaseAsset {
    param(
        [Parameter(Mandatory)][Uri]$Uri,
        [Parameter(Mandatory)][string]$AssetName,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$UpdatesDirectory
    )
    $match = [regex]::Match(
        $Uri.AbsolutePath,
        '^/HernanJiang/Codex(?:-)?Router/releases/download/([^/]+)/([^/]+)$',
        [Text.RegularExpressions.RegexOptions]::IgnoreCase)
    if (-not $match.Success -or
        [Uri]::UnescapeDataString($match.Groups[2].Value) -ne $AssetName) {
        throw 'The private release asset URL is not recognized.'
    }
    $tag = [Uri]::UnescapeDataString($match.Groups[1].Value)
    $gh = Get-GitHubCliPath
    $staging = Join-Path $UpdatesDirectory ('.gh-release-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($staging) | Out-Null
    try {
        & $gh release download $tag --repo $repositoryName --pattern $AssetName --dir $staging --clobber 2>$null
        $downloaded = Join-Path $staging $AssetName
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $downloaded -PathType Leaf)) {
            throw 'PRIVATE_GITHUB_AUTH_REQUIRED'
        }
        Move-Item -LiteralPath $downloaded -Destination $Destination -Force
    } finally {
        if (Test-Path -LiteralPath $staging) {
            Remove-Item -LiteralPath $staging -Recurse -Force
        }
    }
}

function Write-UpdateResult {
    param(
        [string]$Status,
        [string]$LatestVersion = '',
        [string]$ReleaseName = '',
        [string]$ReleaseNotes = '',
        [string]$ReleaseUrl = '',
        [string]$AssetName = '',
        [string]$AssetUrl = '',
        [long]$AssetSize = 0,
        [string]$DownloadPath = '',
        [string]$Message = ''
    )
    [ordered]@{
        status = $Status
        currentVersion = $CurrentVersion
        latestVersion = $LatestVersion
        releaseName = $ReleaseName
        releaseNotes = $ReleaseNotes
        releaseUrl = if ($ReleaseUrl) { $ReleaseUrl } else { $repositoryUrl }
        assetName = $AssetName
        downloadUrl = $AssetUrl
        assetSize = $AssetSize
        downloadPath = $DownloadPath
        message = $Message
    } | ConvertTo-Json -Depth 6 -Compress
}

function ConvertTo-NormalizedVersion {
    param([string]$Value)
    $match = [regex]::Match($Value.Trim(), '^v?(\d+)\.(\d+)\.(\d+)')
    if (-not $match.Success) { return $null }
    return [version]("{0}.{1}.{2}" -f $match.Groups[1].Value, $match.Groups[2].Value, $match.Groups[3].Value)
}

if ($Action -eq 'Download') {
    try {
        $uri = [Uri]$DownloadUrl
        if (
            $uri.Scheme -ne 'https' -or
            $uri.Host -ne 'github.com' -or
            -not ($uri.AbsolutePath.StartsWith('/HernanJiang/CodexRouter/releases/download/', [StringComparison]::OrdinalIgnoreCase) -or $uri.AbsolutePath.StartsWith('/HernanJiang/Codex-Router/releases/download/', [StringComparison]::OrdinalIgnoreCase))
        ) {
            throw 'The update download URL is not an official GitHub release URL.'
        }
        $safeName = [IO.Path]::GetFileName($FileName)
        $extension = [IO.Path]::GetExtension($safeName).ToLowerInvariant()
        if (-not $safeName -or $extension -notin @('.zip', '.exe', '.msi', '.7z')) {
            throw 'The GitHub release does not contain a supported Windows update package.'
        }
        $updatesDirectory = Join-Path $routerRoot 'updates'
        New-Item -ItemType Directory -Path $updatesDirectory -Force | Out-Null
        $destination = Join-Path $updatesDirectory $safeName
        $temporary = "$destination.download"
        try {
            Invoke-WebRequest -UseBasicParsing -Headers @{
                'User-Agent' = 'CodexRouter-Updater'
                'Accept' = 'application/octet-stream'
            } -Uri $uri.AbsoluteUri -OutFile $temporary -TimeoutSec 600
        } catch {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            Receive-PrivateReleaseAsset `
                -Uri $uri `
                -AssetName $safeName `
                -Destination $temporary `
                -UpdatesDirectory $updatesDirectory
        }
        $actualSize = (Get-Item -LiteralPath $temporary).Length
        if ($ExpectedSize -gt 0 -and $actualSize -ne $ExpectedSize) {
            Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            throw "Downloaded file size mismatch (expected $ExpectedSize, received $actualSize)."
        }
        Move-Item -LiteralPath $temporary -Destination $destination -Force
        Write-UpdateResult -Status 'downloaded' -AssetName $safeName -AssetUrl $DownloadUrl -AssetSize $actualSize -DownloadPath $destination -Message 'Update package downloaded from the official GitHub release.'
    } catch {
        Write-UpdateResult -Status 'error' -Message $_.Exception.Message
    }
    exit 0
}

try {
    $release = Get-LatestGitHubRelease

    $latestTag = [string]$release.tag_name
    $latestVersion = ConvertTo-NormalizedVersion -Value $latestTag
    $installedVersion = ConvertTo-NormalizedVersion -Value $CurrentVersion
    $hasUpdate = if ($null -ne $latestVersion -and $null -ne $installedVersion) {
        $latestVersion -gt $installedVersion
    } else {
        $latestTag.TrimStart('v') -ne $CurrentVersion.TrimStart('v')
    }

    $candidates = @()
    foreach ($asset in @($release.assets)) {
        $name = [string]$asset.name
        $extension = [IO.Path]::GetExtension($name).ToLowerInvariant()
        if ($extension -notin @('.zip', '.exe', '.msi', '.7z')) { continue }
        $score = 0
        if ($name -match '(?i)portable') { $score += 100 }
        if ($name -match '(?i)windows|win') { $score += 60 }
        if ($name -match '(?i)x64|amd64') { $score += 30 }
        if ($name -match '(?i)source|symbols|debug') { $score -= 200 }
        $candidates += [pscustomobject]@{
            Score = $score
            Name = $name
            Url = [string]$asset.browser_download_url
            Size = [long]$asset.size
        }
    }
    $selectedAsset = $candidates | Sort-Object Score -Descending | Select-Object -First 1
    Write-UpdateResult `
        -Status $(if ($hasUpdate) { 'update_available' } else { 'current' }) `
        -LatestVersion $latestTag `
        -ReleaseName ([string]$release.name) `
        -ReleaseNotes ([string]$release.body) `
        -ReleaseUrl ([string]$release.html_url) `
        -AssetName $(if ($selectedAsset) { $selectedAsset.Name } else { '' }) `
        -AssetUrl $(if ($selectedAsset) { $selectedAsset.Url } else { '' }) `
        -AssetSize $(if ($selectedAsset) { $selectedAsset.Size } else { 0 }) `
        -Message $(if ($hasUpdate -and -not $selectedAsset) { 'A new release exists, but it has no supported Windows package.' } else { '' })
} catch {
    if ($_.Exception.Message -eq 'PRIVATE_GITHUB_AUTH_REQUIRED') {
        Write-UpdateResult -Status 'private_auth_required' -Message 'This repository is private. Install GitHub CLI and sign in to receive private releases; public releases update without GitHub CLI.'
        exit 0
    }
    $statusCode = 0
    if ($null -ne $_.Exception.Response -and $null -ne $_.Exception.Response.StatusCode) {
        $statusCode = [int]$_.Exception.Response.StatusCode
    }
    if ($statusCode -eq 404) {
        Write-UpdateResult -Status 'no_release' -Message 'The official GitHub repository has not published a Release yet.'
    } else {
        Write-UpdateResult -Status 'error' -Message $_.Exception.Message
    }
}
