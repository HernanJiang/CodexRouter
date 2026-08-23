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

function Get-LatestGitHubRelease {
    $urls = @(
        $releaseApiUrl
        'https://ghfast.top/https://api.github.com/repos/HernanJiang/CodexRouter/releases/latest'
        'https://ghproxy.net/https://api.github.com/repos/HernanJiang/CodexRouter/releases/latest'
        'https://mirror.ghproxy.com/https://api.github.com/repos/HernanJiang/CodexRouter/releases/latest'
    )
    $lastError = $null
    foreach ($url in $urls) {
        try {
            return Invoke-RestMethod -Headers @{
                'User-Agent' = 'CodexRouter-Updater'
                'Accept' = 'application/vnd.github+json'
                'X-GitHub-Api-Version' = '2022-11-28'
            } -Uri $url -TimeoutSec 30
        } catch {
            $statusCode = 0
            if ($null -ne $_.Exception.Response -and $null -ne $_.Exception.Response.StatusCode) {
                $statusCode = [int]$_.Exception.Response.StatusCode
            }
            if ($statusCode -eq 404) { throw }
            $lastError = $_
        }
    }
    if ($null -ne $lastError) { throw $lastError }
    throw 'GitHub and public mirrors could not return the latest release'
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
        $downloadUrls = @(
            $uri.AbsoluteUri
            ('https://ghfast.top/' + $uri.AbsoluteUri)
            ('https://ghproxy.net/' + $uri.AbsoluteUri)
            ('https://mirror.ghproxy.com/' + $uri.AbsoluteUri)
            ($uri.AbsoluteUri -replace 'https://github.com/', 'https://kkgithub.com/')
        )
        $downloaded = $false
        $lastError = $null
        foreach ($candidate in $downloadUrls) {
            try {
                Invoke-WebRequest -UseBasicParsing -Headers @{
                    'User-Agent' = 'CodexRouter-Updater'
                    'Accept' = 'application/octet-stream'
                } -Uri $candidate -OutFile $temporary -TimeoutSec 600
                $downloaded = $true
                break
            } catch {
                $lastError = $_
                Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
            }
        }
        if (-not $downloaded) {
            if ($null -ne $lastError) { throw $lastError }
            throw 'GitHub and public mirrors could not download the update package.'
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
