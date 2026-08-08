param(
    [string]$OutputRoot,
    [switch]$SkipBuild,
    [switch]$SkipArchive,
    [string]$ValidateStage,
    [string]$ScanOnlyPath,
    [string]$VcRedistCrtDir = $env:VC_REDIST_CRT_DIR
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ($PSVersionTable.PSEdition -eq 'Desktop') {
    $desktopModuleRoot = Join-Path $PSHOME 'Modules'
    $modulePaths = @($env:PSModulePath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    if ($modulePaths -notcontains $desktopModuleRoot) {
        $env:PSModulePath = $desktopModuleRoot + ';' + ($modulePaths -join ';')
    }
    Import-Module (Join-Path $desktopModuleRoot 'Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1') -ErrorAction Stop
    Import-Module (Join-Path $desktopModuleRoot 'Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1') -ErrorAction Stop
} else {
    Import-Module Microsoft.PowerShell.Utility -ErrorAction Stop
    Import-Module Microsoft.PowerShell.Security -ErrorAction Stop
}

$routerRoot = Split-Path -Parent $PSScriptRoot
$routerRootPath = [IO.Path]::GetFullPath($routerRoot).TrimEnd([char[]]@('\', '/'))
$utf8NoBom = [Text.UTF8Encoding]::new($false)

$runtimeScripts = @(
    'apply-codex-router.ps1',
    'Apply-Router.ps1',
    'Build-ModelCatalog.ps1',
    'CodexIntegration.psm1',
    'Copy-AdminPassword.ps1',
    'Copy-LocalApiKey.ps1',
    'Copy-Sub2ApiLogin.ps1',
    'CredentialStore.psm1',
    'Ensure-RouterHealthy.ps1',
    'Ensure-Sub2ApiAdmin.ps1',
    'Get-LocalApiKey.ps1',
    'Get-OAuthAccounts.ps1',
    'Get-RouterStatus.ps1',
    'Get-UsageMonitor.ps1',
    'GitHub-Update.ps1',
    'Import-GrokSSO.ps1',
    'Initialize-Router.ps1',
    'Install-CodexIntegration.ps1',
    'Invoke-OAuthRecovery.ps1',
    'Launch-ChatGPTOAuth.ps1',
    'ProxyDiscovery.psm1',
    'Register-Autostart.ps1',
    'Remove-OAuthAccount.ps1',
    'RouterAdmin.psm1',
    'Set-OAuthAccountPriority.ps1',
    'Set-ProviderKeys.ps1',
    'Start-ChatGPTOAuth.ps1',
    'Start-CodexRouter.ps1',
    'Start-ProviderOAuth.ps1',
    'Start-Router.ps1',
    'Stop-Router.ps1',
    'Sync-RouterRoutingState.ps1',
    'Test-RouterCapabilities.ps1',
    'Test-RealOAuthFallback.ps1',
    'Repair-CodexWindowsSetup.ps1',
    'Unregister-Autostart.ps1',
    'UserData.psm1'
)

$configFiles = @(
    'model-catalog.example.json',
    'pg_hba.conf',
    'postgresql.conf',
    'redis.conf',
    'sub2api.example.yaml'
)

$staticLicenseFiles = @(
    'Microsoft-Visual-Cpp-Runtime-NOTICE.txt',
    'MSYS2-Runtime-LICENSES.txt',
    'Redis-8.10.0-LICENSES.txt',
    'Rust-SPDX-LICENSE-TEXTS.txt',
    'sub2api-0.1.168-codex-router.3.patch',
    'sub2api-0.1.170-codex-router.2.patch',
    'sub2api-0.1.170-codex-router.3.patch'
)

$generatedLicenseFiles = @(
    'Rust-Crates-LICENSES.txt'
)

$redisRuntimeFiles = @(
    'msys-2.0.dll',
    'msys-crypto-3.dll',
    'msys-gcc_s-seh-1.dll',
    'msys-ssl-3.dll',
    'msys-stdc++-6.dll',
    'redis-cli.exe',
    'redis-server.exe',
    'README.md',
    'README.zh_CN.md'
)

$vcRuntimeFiles = @(
    'VCRUNTIME140.dll',
    'VCRUNTIME140_1.dll',
    'MSVCP140.dll'
)

$vcRuntimeDestinationDirectories = @(
    '',
    'postgres\pgsql\bin'
)

# PostgreSQL 18.4 dumpbin audit: no non-wx PE except StackBuilder imports any wx DLL.
$postgresStackBuilderWxFiles = @(
    'wxbase3210u_net_vc_x64_custom.dll',
    'wxbase3210u_vc_x64_custom.dll',
    'wxbase3210u_xml_vc_x64_custom.dll',
    'wxmsw3210u_adv_vc_x64_custom.dll',
    'wxmsw3210u_aui_vc_x64_custom.dll',
    'wxmsw3210u_core_vc_x64_custom.dll',
    'wxmsw3210u_html_vc_x64_custom.dll',
    'wxmsw3210u_xrc_vc_x64_custom.dll'
)

$releaseScanRules = @(
    [pscustomobject]@{ Name = 'windows_user_path'; Pattern = '(?i)[a-z]:[\\/]+Users[\\/]+[^\\/\s"''<>|]{1,96}'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'windows_work_path'; Pattern = '(?i)[a-z]:[\\/]+Work[\\/]+[^\x00\r\n\t"''<>|]{1,160}'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_sk'; Pattern = '(?i)(?<![a-z0-9])sk-(?:ant-|proj-|svcacct-|admin-|local-)?[a-z0-9_-]{16,}'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_google'; Pattern = '(?i)\bAIza[a-z0-9_-]{20,}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_github'; Pattern = '(?i)\b(?:ghp|github_pat)_[a-z0-9_]{20,}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_aws'; Pattern = '\bAKIA[0-9A-Z]{16}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_slack'; Pattern = '(?i)\bxox[baprs]-[a-z0-9-]{20,}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_stripe'; Pattern = '(?i)\b(?:sk|rk)_live_[a-z0-9]{16,}\b|\bwhsec_[a-z0-9]{16,}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_bearer'; Pattern = '(?i)\bBearer\s+[a-z0-9._~+/-]{20,}={0,2}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'secret_jwt'; Pattern = '\beyJ[a-zA-Z0-9_-]{12,}\.[a-zA-Z0-9_-]{12,}\.[a-zA-Z0-9_-]{12,}\b'; StructuredOnly = $false },
    [pscustomobject]@{ Name = 'private_key'; Pattern = '-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----'; StructuredOnly = $false },
    [pscustomobject]@{
        Name = 'structured_secret_assignment'
        Pattern = '(?im)(?<![a-z0-9_])["'']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)["'']?(?![a-z0-9_])\s*[:=]\s*(?:(?<quote>["''])(?<candidate>[^"''\r\n]{12,})\k<quote>|(?<candidate>[a-zA-Z0-9_./+~=-]{12,}))'
        StructuredOnly = $true
    }
)

$structuredExtensions = @('.json', '.yaml', '.yml', '.toml', '.conf', '.ini', '.env', '.xml', '.ps1', '.psm1', '.py')
$knownSub2ApiPlaceholderFingerprint = 'a30f343064609c55bf30efb519621564f4d180ef3a22513727c437ff52b0853c'
$releaseScanNeedles = @{
    windows_user_path = @(':\Users\', ':/Users/')
    windows_work_path = @(':\Work\', ':/Work/')
    secret_sk = @('sk-')
    secret_google = @('AIza')
    secret_github = @('ghp_', 'github_pat_')
    secret_aws = @('AKIA')
    secret_slack = @('xox')
    secret_stripe = @('sk_live_', 'rk_live_', 'whsec_')
    secret_bearer = @('Bearer')
    secret_jwt = @('eyJ')
    private_key = @('-----BEGIN ')
    structured_secret_assignment = @('api_key', 'api-key', 'apikey', 'access_token', 'access-token', 'accesstoken', 'refresh_token', 'refresh-token', 'refreshtoken', 'client_secret', 'client-secret', 'clientsecret', 'password')
}
$releaseScanUtf16Needles = [Collections.Generic.List[string]]::new()
$releaseScanUtf16NeedleSet = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($needleGroup in $releaseScanNeedles.Values) {
    foreach ($needle in @($needleGroup)) {
        $builder = [Text.StringBuilder]::new($needle.Length * 2)
        foreach ($character in $needle.ToCharArray()) {
            [void]$builder.Append($character)
            [void]$builder.Append([char]0)
        }
        $wideNeedle = $builder.ToString()
        if ($releaseScanUtf16NeedleSet.Add($wideNeedle)) { $releaseScanUtf16Needles.Add($wideNeedle) }
    }
}

function Get-Sha256String {
    param([Parameter(Mandatory)][string]$Value)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        $hash = $algorithm.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value))
        return ([BitConverter]::ToString($hash)).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-NormalizedRelativePath {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Path
    )
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/'))
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (-not $fullPath.StartsWith($rootPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Release file escaped the stage root: $fullPath"
    }
    return $fullPath.Substring($rootPath.Length + 1).Replace('\', '/')
}

function Get-SafeManifestRelativePath {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$RelativePath
    )
    if ([string]::IsNullOrWhiteSpace($RelativePath) -or $RelativePath.IndexOf([char]0) -ge 0) {
        throw 'Manifest contains an empty or invalid path.'
    }
    $normalized = $RelativePath.Replace('\', '/')
    if ([IO.Path]::IsPathRooted($RelativePath) -or
        $normalized.StartsWith('/') -or
        $normalized -match '^[a-zA-Z]:') {
        throw "Manifest contains an absolute path: $RelativePath"
    }
    foreach ($segment in $normalized.Split('/')) {
        if ([string]::IsNullOrEmpty($segment) -or $segment -in @('.', '..')) {
            throw "Manifest contains a non-canonical path: $RelativePath"
        }
    }
    $candidate = Join-Path $Root ($normalized.Replace('/', '\'))
    $canonical = Get-NormalizedRelativePath -Root $Root -Path $candidate
    if (-not $canonical.Equals($normalized, [StringComparison]::Ordinal)) {
        throw "Manifest path is non-canonical or escaped the stage root: $RelativePath"
    }
    return $canonical
}

function Test-AllowedReleaseMatch {
    param(
        [Parameter(Mandatory)]$Rule,
        [Parameter(Mandatory)][Text.RegularExpressions.Match]$Match,
        [Parameter(Mandatory)][string]$RelativePath
    )
    if ($Rule.Name -eq 'secret_sk' -and
        $RelativePath.Equals('app/sub2api.exe', [StringComparison]::OrdinalIgnoreCase) -and
        (Get-Sha256String -Value $Match.Value) -eq $knownSub2ApiPlaceholderFingerprint) {
        return $true
    }
    if ($Rule.Name -eq 'structured_secret_assignment') {
        $candidate = $Match.Groups['candidate'].Value
        $extension = [IO.Path]::GetExtension($RelativePath).ToLowerInvariant()
        if ($extension -in @('.ps1', '.psm1', '.py') -and -not $Match.Groups['quote'].Success) {
            return $true
        }
        if ($candidate -match '(?i)^(?:change[_-]?me|placeholder|example|sample|fake|dummy|proxy_managed|must[_-]?not(?:[_-]?(?:use|write))?|your[_-](?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)|test[_-](?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password))$' -or
            $candidate -match '^\{env:[A-Z][A-Z0-9_]*\}$') {
            return $true
        }
    }
    return $false
}

function Assert-ScanText {
    param(
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$RelativePath,
        [Parameter(Mandatory)][string]$EncodingName,
        [Parameter(Mandatory)][bool]$Structured
    )
    foreach ($rule in $releaseScanRules) {
        if ($rule.StructuredOnly -and -not $Structured) { continue }
        $possible = $false
        foreach ($needle in @($releaseScanNeedles[$rule.Name])) {
            if ($Text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                $possible = $true
                break
            }
        }
        if (-not $possible) { continue }
        foreach ($match in [Text.RegularExpressions.Regex]::Matches($Text, $rule.Pattern)) {
            if (Test-AllowedReleaseMatch -Rule $rule -Match $match -RelativePath $RelativePath) { continue }
            throw "Release scan rejected '$RelativePath' ($($rule.Name), $EncodingName). Match content was redacted."
        }
    }
}

function Assert-NoSensitiveContent {
    param([Parameter(Mandatory)][string]$Root)
    $rootPath = [IO.Path]::GetFullPath($Root)
    if (-not (Test-Path -LiteralPath $rootPath -PathType Container)) {
        throw "Release scan root does not exist: $rootPath"
    }
    $bufferSize = 1024 * 1024
    $overlapSize = 1024
    foreach ($file in Get-ChildItem -LiteralPath $rootPath -Recurse -File -Force | Sort-Object FullName) {
        $relative = Get-NormalizedRelativePath -Root $rootPath -Path $file.FullName
        $structured = $file.Extension.ToLowerInvariant() -in $structuredExtensions
        $stream = [IO.File]::Open($file.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            $buffer = [byte[]]::new($bufferSize)
            $carry = [byte[]]::new(0)
            while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $combined = [byte[]]::new($carry.Length + $read)
                if ($carry.Length -gt 0) { [Buffer]::BlockCopy($carry, 0, $combined, 0, $carry.Length) }
                [Buffer]::BlockCopy($buffer, 0, $combined, $carry.Length, $read)

                $ascii = [Text.Encoding]::ASCII.GetString($combined)
                Assert-ScanText -Text $ascii -RelativePath $relative -EncodingName 'ASCII' -Structured $structured

                $scanUtf16 = $false
                foreach ($wideNeedle in $releaseScanUtf16Needles) {
                    if ($ascii.IndexOf($wideNeedle, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                        $scanUtf16 = $true
                        break
                    }
                }
                if ($scanUtf16) {
                    foreach ($offset in @(0, 1)) {
                        $usable = $combined.Length - $offset
                        if ($usable -lt 2) { continue }
                        if (($usable % 2) -ne 0) { $usable-- }
                        $unicode = [Text.Encoding]::Unicode.GetString($combined, $offset, $usable)
                        Assert-ScanText -Text $unicode -RelativePath $relative -EncodingName "UTF-16LE/$offset" -Structured $structured
                    }
                }

                $carryLength = [Math]::Min($overlapSize, $combined.Length)
                $carry = [byte[]]::new($carryLength)
                [Buffer]::BlockCopy($combined, $combined.Length - $carryLength, $carry, 0, $carryLength)
            }
        } finally {
            $stream.Dispose()
        }
    }
}

function Assert-NoReparsePoints {
    param([Parameter(Mandatory)][string]$Root)
    $items = @(
        Get-Item -LiteralPath $Root -Force
        Get-ChildItem -LiteralPath $Root -Force -Recurse
    )
    $reparse = $items | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint } | Select-Object -First 1
    if ($null -ne $reparse) {
        throw "Release input contains a reparse point: $($reparse.FullName)"
    }
}

function Assert-ReleaseLayout {
    param(
        [Parameter(Mandatory)][string]$Root,
        [switch]$RequireManifests
    )
    $rootPath = [IO.Path]::GetFullPath($Root)
    Assert-NoReparsePoints -Root $rootPath

    $allowedTopDirectories = @('app', 'assets', 'config', 'licenses', 'postgres', 'redis', 'scripts')
    foreach ($directory in Get-ChildItem -LiteralPath $rootPath -Directory -Force) {
        if ($directory.Name -notin $allowedTopDirectories) {
            throw "Unexpected top-level release directory: $($directory.Name)"
        }
    }

    foreach ($forbidden in @(
        'codex-router-config.json',
        'codex-router-ui-preferences.json',
        'data',
        'logs',
        'backups',
        'downloads',
        'updates',
        'config\model-catalog.json',
        'config\models.json',
        'config\sub2api-channels.json',
        'postgres\pgsql\pgAdmin 4',
        'postgres\pgsql\doc',
        'postgres\pgsql\include',
        'postgres\pgsql\StackBuilder',
        'postgres\pgsql\lib\pgxs',
        'postgres\pgsql\lib\pkgconfig',
        'postgres\pgsql\share\locale',
        'postgres\pgsql\bin\stackbuilder.exe'
    )) {
        if (Test-Path -LiteralPath (Join-Path $rootPath $forbidden)) {
            throw "Forbidden development or runtime state entered the release: $forbidden"
        }
    }

    $allowedExact = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($path in @(
        'Codex-Router.exe',
        'LICENSE',
        'README.md',
        'README.zh-CN.md',
        'TERMS.en.md',
        'TERMS.zh-CN.md',
        'THIRD_PARTY_NOTICES.md',
        'release-manifest.json',
        'dependency-manifest.json',
        'app/sub2api.exe',
        'app/data/model_pricing.json',
        'app/data/model_pricing.sha256',
        'app/resources/model-pricing/model_prices_and_context_window.json',
        'assets/logo.ico',
        'assets/logo.png',
        'licenses/Sub2API-LICENSE.txt',
        'postgres/pgsql/server_license.txt',
        'postgres/pgsql/commandlinetools_3rd_party_licenses.txt'
    )) { [void]$allowedExact.Add($path) }
    foreach ($name in $runtimeScripts) { [void]$allowedExact.Add("scripts/$name") }
    foreach ($name in $configFiles) { [void]$allowedExact.Add("config/$name") }
    foreach ($name in $staticLicenseFiles + $generatedLicenseFiles) { [void]$allowedExact.Add("licenses/$name") }
    foreach ($name in $redisRuntimeFiles) { [void]$allowedExact.Add("redis/Redis-8.10.0-Windows-x64-msys2/$name") }
    foreach ($name in $vcRuntimeFiles) { [void]$allowedExact.Add($name) }

    foreach ($file in Get-ChildItem -LiteralPath $rootPath -Recurse -File -Force) {
        $relative = Get-NormalizedRelativePath -Root $rootPath -Path $file.FullName
        $allowed = $allowedExact.Contains($relative) -or
            $relative.StartsWith('postgres/pgsql/bin/', [StringComparison]::OrdinalIgnoreCase) -or
            $relative.StartsWith('postgres/pgsql/lib/', [StringComparison]::OrdinalIgnoreCase) -or
            $relative.StartsWith('postgres/pgsql/share/', [StringComparison]::OrdinalIgnoreCase)
        if (-not $allowed) { throw "Release file is not on the allowlist: $relative" }
        if ($relative.StartsWith('postgres/pgsql/', [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetExtension($relative).ToLowerInvariant() -in @('.a', '.lib')) {
            throw "PostgreSQL development library entered the release: $relative"
        }
        if ($relative -match '(?i)^postgres/pgsql/bin/wx[^/]*\.dll$') {
            throw "PostgreSQL StackBuilder wxWidgets runtime entered the release: $relative"
        }
        if ($relative -match '(?i)(?:^|/)(?:postmaster\.pid|dump\.rdb|appendonly\.aof)$' -or
            $relative -match '(?i)\.(?:pdb|ilk|dmp|log|tmp|bak|sqlite3?|db)$') {
            throw "Debug or runtime-state file entered the release: $relative"
        }
    }

    $required = @(
        'Codex-Router.exe',
        'README.md',
        'README.zh-CN.md',
        'app\sub2api.exe',
        'app\data\model_pricing.json',
        'app\resources\model-pricing\model_prices_and_context_window.json',
        'postgres\pgsql\bin\initdb.exe',
        'postgres\pgsql\bin\postgres.exe',
        'redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe',
        'redis\Redis-8.10.0-Windows-x64-msys2\redis-cli.exe',
        'scripts\Start-Router.ps1',
        'scripts\Apply-Router.ps1',
        'config\postgresql.conf',
        'config\redis.conf'
    )
    $required += @($runtimeScripts | ForEach-Object { "scripts\$_" })
    $required += @($configFiles | ForEach-Object { "config\$_" })
    $required += @(($staticLicenseFiles + $generatedLicenseFiles) | ForEach-Object { "licenses\$_" })
    $required += @($redisRuntimeFiles | ForEach-Object { "redis\Redis-8.10.0-Windows-x64-msys2\$_" })
    foreach ($destinationDirectory in $vcRuntimeDestinationDirectories) {
        $required += @($vcRuntimeFiles | ForEach-Object {
            if ([string]::IsNullOrEmpty($destinationDirectory)) { $_ } else { "$destinationDirectory\$_" }
        })
    }
    if ($RequireManifests) { $required += @('release-manifest.json', 'dependency-manifest.json') }
    foreach ($relative in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $rootPath $relative) -PathType Leaf)) {
            throw "Required release file is missing: $relative"
        }
    }
    [void](Assert-VcRuntimePayload -Root $rootPath)
}

function Get-PeMetadata {
    param([Parameter(Mandatory)][string]$Path)
    $stream = [IO.File]::OpenRead($Path)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) { throw "Not a PE executable: $Path" }
        $stream.Position = 0x3c
        $peOffset = $reader.ReadInt32()
        if ($peOffset -lt 0 -or $peOffset + 6 -gt $stream.Length) { throw "Invalid PE header: $Path" }
        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "Invalid PE signature: $Path" }
        $machine = $reader.ReadUInt16()
    } finally {
        $reader.Dispose()
        $stream.Dispose()
    }
    $architecture = switch ($machine) {
        0x8664 { 'x64' }
        0xAA64 { 'arm64' }
        0x014c { 'x86' }
        default { 'unknown' }
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    return [ordered]@{
        machine = ('0x{0:X4}' -f $machine)
        architecture = $architecture
        signatureStatus = [string]$signature.Status
    }
}

function Get-VcRuntimeBinaryMetadata {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required Microsoft Visual C++ runtime file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Microsoft Visual C++ runtime file must not be a reparse point: $Path"
    }
    $pe = Get-PeMetadata -Path $item.FullName
    if ($pe.architecture -ne 'x64') {
        throw "Microsoft Visual C++ runtime architecture mismatch: $($item.Name) is $($pe.architecture)."
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $item.FullName
    $signerSubject = if ($null -eq $signature.SignerCertificate) { '' } else { [string]$signature.SignerCertificate.Subject }
    if ([string]$signature.Status -ne 'Valid' -or
        $signerSubject -notmatch '(?i)(?:^|,\s*)O=Microsoft Corporation(?:,|$)') {
        throw "Microsoft Visual C++ runtime signature is not a valid Microsoft signature: $($item.Name) ($($signature.Status))."
    }
    $productVersion = [string]$item.VersionInfo.ProductVersion
    if ([string]::IsNullOrWhiteSpace($productVersion)) {
        throw "Microsoft Visual C++ runtime version metadata is missing: $($item.Name)"
    }
    return [pscustomobject]@{
        Name = $item.Name
        Path = $item.FullName
        Version = $productVersion
        Bytes = [long]$item.Length
        Sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        Architecture = $pe.architecture
        PeMachine = $pe.machine
        SignatureStatus = [string]$signature.Status
        SignerSubject = $signerSubject
    }
}

function Assert-NotWindowsSystemRuntimeSource {
    param([Parameter(Mandatory)][string]$Path)

    $fullPath = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $windowsRoot = [Environment]::GetEnvironmentVariable('SystemRoot')
    if ([string]::IsNullOrWhiteSpace($windowsRoot)) { return }
    foreach ($relative in @('System32', 'SysWOW64')) {
        $forbidden = [IO.Path]::GetFullPath((Join-Path $windowsRoot $relative)).TrimEnd([char[]]@('\', '/'))
        if ($fullPath.Equals($forbidden, [StringComparison]::OrdinalIgnoreCase) -or
            $fullPath.StartsWith($forbidden + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw "VC runtime source must not be Windows $relative. Use an official Visual Studio VC Redist x64 CRT directory."
        }
    }
}

function Get-VcRuntimeDirectoryMetadata {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$SourceKind
    )

    $fullPath = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    if (-not (Test-Path -LiteralPath $fullPath -PathType Container)) {
        throw "Microsoft Visual C++ runtime source directory does not exist: $fullPath"
    }
    Assert-NotWindowsSystemRuntimeSource -Path $fullPath
    $directory = Get-Item -LiteralPath $fullPath -Force
    if ($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "Microsoft Visual C++ runtime source must not be a reparse point: $fullPath"
    }

    $versions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $files = [Collections.Generic.List[object]]::new()
    foreach ($name in $vcRuntimeFiles) {
        $metadata = Get-VcRuntimeBinaryMetadata -Path (Join-Path $fullPath $name)
        [void]$versions.Add($metadata.Version)
        [void]$files.Add($metadata)
    }
    if ($versions.Count -ne 1) {
        throw "Microsoft Visual C++ runtime files are not from one version: $($versions -join ', ')"
    }
    return [pscustomobject]@{
        Path = $fullPath
        SourceKind = $SourceKind
        Version = @($versions)[0]
        Files = @($files)
    }
}

function Resolve-VcRedistCrtDirectory {
    param([string]$OverridePath)

    if (-not [string]::IsNullOrWhiteSpace($OverridePath)) {
        return Get-VcRuntimeDirectoryMetadata -Path $OverridePath -SourceKind 'VC_REDIST_CRT_DIR override'
    }

    $visualStudioRoots = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($programFilesRoot in @(
        [Environment]::GetEnvironmentVariable('ProgramFiles'),
        [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    )) {
        if ([string]::IsNullOrWhiteSpace($programFilesRoot)) { continue }
        $candidateRoot = Join-Path $programFilesRoot 'Microsoft Visual Studio'
        if (Test-Path -LiteralPath $candidateRoot -PathType Container) {
            [void]$visualStudioRoots.Add([IO.Path]::GetFullPath($candidateRoot))
        }
    }

    $candidates = [Collections.Generic.List[object]]::new()
    foreach ($visualStudioRoot in $visualStudioRoots) {
        foreach ($yearDirectory in Get-ChildItem -LiteralPath $visualStudioRoot -Directory -Force -ErrorAction SilentlyContinue) {
            foreach ($editionDirectory in Get-ChildItem -LiteralPath $yearDirectory.FullName -Directory -Force -ErrorAction SilentlyContinue) {
                $msvcRedistRoot = Join-Path $editionDirectory.FullName 'VC\Redist\MSVC'
                if (-not (Test-Path -LiteralPath $msvcRedistRoot -PathType Container)) { continue }
                foreach ($versionDirectory in Get-ChildItem -LiteralPath $msvcRedistRoot -Directory -Force -ErrorAction SilentlyContinue) {
                    try { $parsedVersion = [version]$versionDirectory.Name } catch { continue }
                    $x64Root = Join-Path $versionDirectory.FullName 'x64'
                    if (-not (Test-Path -LiteralPath $x64Root -PathType Container)) { continue }
                    foreach ($crtDirectory in Get-ChildItem -LiteralPath $x64Root -Directory -Force -ErrorAction SilentlyContinue) {
                        if ($crtDirectory.Name -notmatch '^Microsoft\.VC\d+\.CRT$') { continue }
                        [void]$candidates.Add([pscustomobject]@{
                            Version = $parsedVersion
                            Path = $crtDirectory.FullName
                        })
                    }
                }
            }
        }
    }

    foreach ($candidate in @($candidates | Sort-Object -Property @{ Expression = 'Version'; Descending = $true }, @{ Expression = 'Path'; Descending = $true })) {
        try {
            return Get-VcRuntimeDirectoryMetadata -Path $candidate.Path -SourceKind 'Visual Studio VC Redist auto-discovery'
        } catch {
            continue
        }
    }
    throw 'No complete official Visual Studio VC Redist x64 CRT directory was found. Install the Visual Studio C++ Build Tools redistributable files or set VC_REDIST_CRT_DIR to an extracted official x64 Microsoft.VC*.CRT directory.'
}

function Assert-VcRuntimePayload {
    param([Parameter(Mandatory)][string]$Root)

    $versions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $rootHashes = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
    $files = [Collections.Generic.List[object]]::new()
    foreach ($destinationDirectory in $vcRuntimeDestinationDirectories) {
        foreach ($name in $vcRuntimeFiles) {
            $relative = if ([string]::IsNullOrEmpty($destinationDirectory)) { $name } else { "$destinationDirectory\$name" }
            $metadata = Get-VcRuntimeBinaryMetadata -Path (Join-Path $Root $relative)
            [void]$versions.Add($metadata.Version)
            if ([string]::IsNullOrEmpty($destinationDirectory)) {
                $rootHashes.Add($name, $metadata.Sha256)
            } elseif (-not $rootHashes.ContainsKey($name) -or $rootHashes[$name] -ne $metadata.Sha256) {
                throw "Microsoft Visual C++ runtime copies differ between application directories: $name"
            }
            [void]$files.Add([pscustomobject]@{
                Path = $relative.Replace('\', '/')
                Bytes = $metadata.Bytes
                Sha256 = $metadata.Sha256
            })
        }
    }
    if ($versions.Count -ne 1) {
        throw "Packaged Microsoft Visual C++ runtime files are not from one version: $($versions -join ', ')"
    }
    return [pscustomobject]@{
        Version = @($versions)[0]
        Files = @($files)
    }
}

function Get-CargoPackageVersion {
    $cargoToml = [IO.File]::ReadAllText((Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml'))
    $match = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"\s*$')
    if (-not $match.Success) { throw 'Could not read the Codex-Router version from Cargo.toml.' }
    return $match.Groups[1].Value
}

function Set-TermsReleaseMetadata {
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$ReleaseDate
    )

    $versionPattern = '(?m)^([^\r\n]*?v)\d+\.\d+\.\d+\r?$'
    $datePattern = '(?m)^([^\r\n]*?)\d{4}-\d{2}-\d{2}\r?$'
    foreach ($name in @('TERMS.zh-CN.md', 'TERMS.en.md')) {
        $path = Join-Path $routerRoot $name
        $content = [IO.File]::ReadAllText($path)
        if ([regex]::Matches($content, $versionPattern).Count -ne 1 -or
            [regex]::Matches($content, $datePattern).Count -ne 1) {
            throw "Release metadata fields are missing or ambiguous in $name."
        }
        $content = [regex]::Replace($content, $versionPattern, "`${1}$Version")
        $content = [regex]::Replace($content, $datePattern, "`${1}$ReleaseDate")
        [IO.File]::WriteAllText($path, $content, $utf8NoBom)
    }
}

function Assert-TermsReleaseMetadata {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$ExpectedVersion,
        [string]$ExpectedDate
    )

    $versionPattern = '(?m)^[^\r\n]*?v(\d+\.\d+\.\d+)\r?$'
    $datePattern = '(?m)^[^\r\n]*?(\d{4}-\d{2}-\d{2})\r?$'
    $dates = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($name in @('TERMS.zh-CN.md', 'TERMS.en.md')) {
        $path = Join-Path $Root $name
        $content = [IO.File]::ReadAllText($path)
        $versionMatches = [regex]::Matches($content, $versionPattern)
        $dateMatches = [regex]::Matches($content, $datePattern)
        $versionMatch = if ($versionMatches.Count -eq 1) { $versionMatches[0] } else { $null }
        $dateMatch = if ($dateMatches.Count -eq 1) { $dateMatches[0] } else { $null }
        if ($null -eq $versionMatch -or -not $versionMatch.Success -or $versionMatch.Groups[1].Value -ne $ExpectedVersion) {
            throw "Terms software version does not match Cargo.toml: $name"
        }
        if ($null -eq $dateMatch -or -not $dateMatch.Success) {
            throw "Terms release date is missing or invalid: $name"
        }
        [void]$dates.Add($dateMatch.Groups[1].Value)
    }
    if ($dates.Count -ne 1) { throw 'The Chinese and English terms use different release dates.' }
    $actualDate = @($dates)[0]
    if (-not [string]::IsNullOrWhiteSpace($ExpectedDate) -and $actualDate -ne $ExpectedDate) {
        throw "Terms release date mismatch: expected $ExpectedDate, found $actualDate."
    }
    return $actualDate
}

function Write-RustThirdPartyLicenseBundle {
    param([Parameter(Mandatory)][string]$DestinationPath)

    $manifestPath = Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml'
    $lockPath = Join-Path $routerRoot 'codex-router-gui-rust\Cargo.lock'
    $metadataOutput = @(& cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc --manifest-path $manifestPath)
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed with exit code $LASTEXITCODE" }
    try {
        $metadata = ($metadataOutput -join "`n") | ConvertFrom-Json
    }
    catch {
        throw "Could not parse cargo metadata while generating Rust license notices: $($_.Exception.Message)"
    }

    $resolvedIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($node in $metadata.resolve.nodes) { [void]$resolvedIds.Add([string]$node.id) }
    $packagesByKey = [Collections.Generic.SortedDictionary[string, object]]::new([StringComparer]::Ordinal)
    foreach ($package in $metadata.packages) {
        if ($resolvedIds.Contains([string]$package.id) -and
            ([string]$package.source).StartsWith('registry+', [StringComparison]::OrdinalIgnoreCase)) {
            $packageKey = "$($package.name)`0$($package.version)`0$($package.id)"
            $packagesByKey.Add($packageKey, $package)
        }
    }
    $packages = @($packagesByKey.Values)
    if ($packages.Count -eq 0) { throw 'Cargo metadata did not contain any resolved registry packages.' }

    $canonicalTextPath = Join-Path $routerRoot 'licenses\Rust-SPDX-LICENSE-TEXTS.txt'
    $canonicalText = [IO.File]::ReadAllText($canonicalTextPath)
    $canonicalMatch = [regex]::Match($canonicalText, '(?m)^Included SPDX identifiers:\s*(.+?)\s*$')
    if (-not $canonicalMatch.Success) { throw 'Rust SPDX license text inventory is missing.' }
    $canonicalIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($identifier in $canonicalMatch.Groups[1].Value.Split(',')) {
        $canonicalId = $identifier.Trim()
        if (-not [regex]::IsMatch($canonicalText, "(?m)^$([regex]::Escape($canonicalId))\r?$")) {
            throw "Rust SPDX license text section is missing: $canonicalId"
        }
        [void]$canonicalIds.Add($canonicalId)
    }
    $usedLicenseIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($package in $packages) {
        foreach ($match in [regex]::Matches([string]$package.license, '[A-Za-z0-9][A-Za-z0-9.+-]*')) {
            if ($match.Value -notin @('AND', 'OR', 'WITH')) { [void]$usedLicenseIds.Add($match.Value) }
        }
    }
    $missingLicenseIds = @($usedLicenseIds | Where-Object { -not $canonicalIds.Contains($_) } | Sort-Object)
    if ($missingLicenseIds.Count -gt 0) {
        throw "Rust SPDX license texts are missing: $($missingLicenseIds -join ', ')"
    }

    $builder = [Text.StringBuilder]::new()
    [void]$builder.AppendLine('Rust crate third-party license notices')
    [void]$builder.AppendLine('======================================')
    [void]$builder.AppendLine()
    [void]$builder.AppendLine('Target: x86_64-pc-windows-msvc')
    [void]$builder.AppendLine("Cargo.lock SHA-256: $((Get-FileHash -LiteralPath $lockPath -Algorithm SHA256).Hash.ToLowerInvariant())")
    [void]$builder.AppendLine("Resolved crates.io packages: $($packages.Count)")
    [void]$builder.AppendLine()
    [void]$builder.AppendLine('Each section records the exact locked crate version, crate-supplied license/notice files, and applicable bundled-native-code notices from its crates.io archive. Canonical SPDX texts referenced by these declarations are distributed separately in Rust-SPDX-LICENSE-TEXTS.txt.')

    foreach ($package in $packages) {
        $crateRoot = [IO.Path]::GetFullPath((Split-Path -Parent ([string]$package.manifest_path))).TrimEnd([char[]]@('\', '/'))
        $crateRootPrefix = $crateRoot + [IO.Path]::DirectorySeparatorChar
        $licensePaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($candidate in Get-ChildItem -LiteralPath $crateRoot -Recurse -File -Force) {
            if ($candidate.Name -match '(?i)(license|licence|copying|copyright|notice|unlicense|^OFL\.txt$|^UFL\.txt$|^Hack-Regular\.txt$)') {
                [void]$licensePaths.Add($candidate.FullName)
            }
        }
        if (-not [string]::IsNullOrWhiteSpace([string]$package.license_file)) {
            $declaredPath = [IO.Path]::GetFullPath((Join-Path $crateRoot ([string]$package.license_file)))
            if (-not $declaredPath.StartsWith($crateRootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Crate license_file escapes its package root: $($package.name) $($package.version)"
            }
            if (-not (Test-Path -LiteralPath $declaredPath -PathType Leaf)) {
                throw "Crate license_file is missing: $($package.name) $($package.version)"
            }
            [void]$licensePaths.Add($declaredPath)
        }

        [void]$builder.AppendLine()
        [void]$builder.AppendLine('------------------------------------------------------------------------')
        [void]$builder.AppendLine("Crate: $($package.name) $($package.version)")
        [void]$builder.AppendLine("License expression: $($package.license)")
        [void]$builder.AppendLine("Crate source: https://crates.io/crates/$($package.name)/$($package.version)")
        if (-not [string]::IsNullOrWhiteSpace([string]$package.repository)) {
            [void]$builder.AppendLine("Repository: $($package.repository)")
        }
        if (@($package.authors).Count -gt 0) {
            [void]$builder.AppendLine("Authors: $($package.authors -join '; ')")
        }

        [string[]]$sortedLicensePaths = @($licensePaths)
        [Array]::Sort($sortedLicensePaths, [StringComparer]::OrdinalIgnoreCase)
        if ($sortedLicensePaths.Count -eq 0) {
            [void]$builder.AppendLine('Packaged license files: none; use the SPDX declaration and the companion canonical text bundle.')
            continue
        }
        foreach ($licensePath in $sortedLicensePaths) {
            $licenseItem = Get-Item -LiteralPath $licensePath
            if ($licenseItem.Length -gt 1MB) {
                throw "Crate license file is unexpectedly large: $($package.name) $($licenseItem.Name)"
            }
            $relativeLicensePath = $licenseItem.FullName.Substring($crateRootPrefix.Length).Replace('\', '/')
            $licenseText = [IO.File]::ReadAllText($licenseItem.FullName)
            if ($licenseText.IndexOf([char]0) -ge 0) {
                throw "Crate license file is not plain text: $($package.name) $relativeLicensePath"
            }
            [void]$builder.AppendLine()
            [void]$builder.AppendLine("--- $relativeLicensePath ---")
            [void]$builder.AppendLine($licenseText.TrimEnd())
        }

        if ([string]$package.name -eq 'libsqlite3-sys') {
            $sqliteHeaderPath = Join-Path $crateRoot 'sqlite3\sqlite3.h'
            if (-not (Test-Path -LiteralPath $sqliteHeaderPath -PathType Leaf)) {
                throw 'The bundled SQLite public-domain notice is missing from libsqlite3-sys.'
            }
            $sqliteHeader = [IO.File]::ReadAllText($sqliteHeaderPath)
            $sqliteNotice = [regex]::Match($sqliteHeader, '(?s)\A/\*.*?\*/')
            if (-not $sqliteNotice.Success -or $sqliteNotice.Value -notmatch 'disclaims copyright') {
                throw 'Could not identify the bundled SQLite public-domain notice.'
            }
            [void]$builder.AppendLine()
            [void]$builder.AppendLine('--- sqlite3/sqlite3.h (SQLite public-domain notice) ---')
            [void]$builder.AppendLine($sqliteNotice.Value.TrimEnd())
        }
    }

    $destinationDirectory = Split-Path -Parent $DestinationPath
    [IO.Directory]::CreateDirectory($destinationDirectory) | Out-Null
    [IO.File]::WriteAllText($DestinationPath, $builder.ToString(), $utf8NoBom)
}

function Get-Sub2ApiVersion {
    $notices = [IO.File]::ReadAllText((Join-Path $routerRoot 'THIRD_PARTY_NOTICES.md'))
    $match = [regex]::Match($notices, '(?im)^- Bundled release:\s*v?([^\s]+)')
    if (-not $match.Success) { return 'unknown' }
    return $match.Groups[1].Value
}

function New-FileManifestEntries {
    param([Parameter(Mandatory)][string]$Root)
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([char[]]@('\', '/'))
    return @(
        Get-ChildItem -LiteralPath $rootPath -Recurse -File -Force |
            Where-Object { $_.Name -ne 'release-manifest.json' } |
            Sort-Object FullName |
            ForEach-Object {
                [ordered]@{
                    path = (Get-NormalizedRelativePath -Root $rootPath -Path $_.FullName)
                    bytes = $_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                }
            }
    )
}

function Get-ComponentSummary {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$Prefix,
        [Parameter(Mandatory)][string]$ExecutableRelativePath,
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][object[]]$Entries
    )
    $componentEntries = @($Entries | Where-Object {
        $_.path.Equals($Prefix, [StringComparison]::OrdinalIgnoreCase) -or
        $_.path.StartsWith($Prefix.TrimEnd('/') + '/', [StringComparison]::OrdinalIgnoreCase)
    })
    if ($componentEntries.Count -eq 0) { throw "Dependency component is empty: $Name" }
    $canonical = ($componentEntries | ForEach-Object { "$($_.sha256) $($_.bytes) $($_.path)" }) -join "`n"
    $componentBytes = [long]0
    foreach ($entry in $componentEntries) { $componentBytes += [long]$entry['bytes'] }
    $executable = Join-Path $Root ($ExecutableRelativePath.Replace('/', '\'))
    $pe = Get-PeMetadata -Path $executable
    return [ordered]@{
        name = $Name
        version = $Version
        executable = $ExecutableRelativePath
        architecture = $pe.architecture
        peMachine = $pe.machine
        signatureStatus = $pe.signatureStatus
        executableSha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
        fileCount = $componentEntries.Count
        bytes = $componentBytes
        treeSha256 = Get-Sha256String -Value $canonical
    }
}

function Get-VcRuntimeComponentSummary {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][object[]]$Entries
    )

    $payload = Assert-VcRuntimePayload -Root $Root
    $componentEntries = [Collections.Generic.List[object]]::new()
    foreach ($payloadFile in $payload.Files) {
        $matches = @($Entries | Where-Object { $_.path.Equals($payloadFile.Path, [StringComparison]::OrdinalIgnoreCase) })
        if ($matches.Count -ne 1) {
            throw "Microsoft Visual C++ runtime file is missing from the dependency input: $($payloadFile.Path)"
        }
        [void]$componentEntries.Add($matches[0])
    }
    $canonical = ($componentEntries | Sort-Object path | ForEach-Object { "$($_.sha256) $($_.bytes) $($_.path)" }) -join "`n"
    $componentBytes = [long]0
    foreach ($entry in $componentEntries) { $componentBytes += [long]$entry.bytes }
    return [ordered]@{
        name = 'Microsoft Visual C++ Runtime'
        version = $payload.Version
        source = 'Microsoft Visual Studio VC Redist x64 app-local CRT'
        primaryBinary = 'VCRUNTIME140.dll'
        architecture = 'x64'
        peMachine = '0x8664'
        signatureStatus = 'Valid'
        deployments = @('application-root', 'postgres/pgsql/bin')
        files = @($componentEntries | Sort-Object path | ForEach-Object {
            [ordered]@{
                path = $_.path
                bytes = [long]$_.bytes
                sha256 = $_.sha256
            }
        })
        fileCount = $componentEntries.Count
        bytes = $componentBytes
        treeSha256 = Get-Sha256String -Value $canonical
    }
}

function Write-ReleaseManifests {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Version
    )
    $payloadEntries = @(New-FileManifestEntries -Root $Root | Where-Object { $_.path -ne 'dependency-manifest.json' })
    $postgresVersion = (Get-Item -LiteralPath (Join-Path $Root 'postgres\pgsql\bin\postgres.exe')).VersionInfo.ProductVersion
    if ([string]::IsNullOrWhiteSpace($postgresVersion)) { $postgresVersion = 'unknown' }
    $dependencyManifest = [ordered]@{
        schemaVersion = 1
        generatedAt = [DateTime]::UtcNow.ToString('o')
        targetPlatform = 'windows-x64'
        components = @(
            Get-ComponentSummary -Name 'Codex-Router' -Version $Version -Prefix 'Codex-Router.exe' -ExecutableRelativePath 'Codex-Router.exe' -Root $Root -Entries $payloadEntries
            Get-ComponentSummary -Name 'Sub2API' -Version (Get-Sub2ApiVersion) -Prefix 'app' -ExecutableRelativePath 'app/sub2api.exe' -Root $Root -Entries $payloadEntries
            Get-ComponentSummary -Name 'PostgreSQL' -Version $postgresVersion -Prefix 'postgres' -ExecutableRelativePath 'postgres/pgsql/bin/postgres.exe' -Root $Root -Entries $payloadEntries
            Get-ComponentSummary -Name 'Redis' -Version '8.10.0' -Prefix 'redis' -ExecutableRelativePath 'redis/Redis-8.10.0-Windows-x64-msys2/redis-server.exe' -Root $Root -Entries $payloadEntries
            Get-VcRuntimeComponentSummary -Root $Root -Entries $payloadEntries
        )
    }
    foreach ($component in $dependencyManifest.components) {
        if ($component.architecture -ne 'x64') {
            throw "Dependency architecture mismatch: $($component.name) is $($component.architecture)."
        }
    }
    [IO.File]::WriteAllText(
        (Join-Path $Root 'dependency-manifest.json'),
        ($dependencyManifest | ConvertTo-Json -Depth 8),
        $utf8NoBom)

    $manifestEntries = New-FileManifestEntries -Root $Root
    [IO.File]::WriteAllText(
        (Join-Path $Root 'release-manifest.json'),
        ($manifestEntries | ConvertTo-Json -Depth 4),
        $utf8NoBom)
    return @($manifestEntries)
}

function Assert-ReleaseManifest {
    param([Parameter(Mandatory)][string]$Root)
    $manifestPath = Join-Path $Root 'release-manifest.json'
    $parsedManifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    $manifest = if ($parsedManifest -is [Array]) { $parsedManifest } else { @($parsedManifest) }
    $expected = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $manifest) {
        $relative = Get-SafeManifestRelativePath -Root $Root -RelativePath ([string]$entry.path)
        if (-not $expected.Add($relative)) { throw "Duplicate manifest path: $relative" }
        $path = Join-Path $Root ($relative.Replace('/', '\'))
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Manifest file is missing: $relative" }
        $file = Get-Item -LiteralPath $path
        if ($file.Length -ne [long]$entry.bytes) { throw "Manifest size mismatch: $relative" }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne [string]$entry.sha256) { throw "Manifest hash mismatch: $relative" }
    }
    foreach ($file in Get-ChildItem -LiteralPath $Root -Recurse -File -Force) {
        $relative = Get-NormalizedRelativePath -Root $Root -Path $file.FullName
        if ($relative -eq 'release-manifest.json') { continue }
        if (-not $expected.Contains($relative)) { throw "File is absent from release manifest: $relative" }
    }
    return $manifest.Count
}

function Get-Sha256FromStream {
    param([Parameter(Mandatory)][IO.Stream]$Stream)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($Stream))).Replace('-', '').ToLowerInvariant()
    } finally {
        $algorithm.Dispose()
    }
}

function Get-SafeArchiveEntryName {
    param([Parameter(Mandatory)][string]$Name)
    $normalized = $Name.Replace('\', '/')
    if ([string]::IsNullOrWhiteSpace($normalized) -or
        $normalized.StartsWith('/') -or
        $normalized -match '^[a-zA-Z]:' -or
        $normalized.IndexOf([char]0) -ge 0) {
        throw 'Archive contains an absolute or invalid entry path.'
    }
    foreach ($segment in $normalized.TrimEnd('/').Split('/')) {
        if ([string]::IsNullOrEmpty($segment) -or $segment -in @('.', '..')) {
            throw 'Archive contains a non-canonical or traversal entry.'
        }
    }
    return $normalized
}

function Assert-ArchiveMatchesReleaseManifest {
    param(
        [Parameter(Mandatory)][string]$Stage,
        [Parameter(Mandatory)][string]$Archive
    )
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $manifestPath = Join-Path $Stage 'release-manifest.json'
    $parsedManifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    $manifest = if ($parsedManifest -is [Array]) { $parsedManifest } else { @($parsedManifest) }
    $expected = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $manifest) {
        $relative = Get-SafeManifestRelativePath -Root $Stage -RelativePath ([string]$entry.path)
        if ($expected.ContainsKey($relative)) { throw "Duplicate manifest path: $relative" }
        $expected.Add($relative, [pscustomobject]@{
            bytes = [long]$entry.bytes
            sha256 = ([string]$entry.sha256).ToLowerInvariant()
        })
    }
    $manifestFile = Get-Item -LiteralPath $manifestPath
    $expected.Add('release-manifest.json', [pscustomobject]@{
        bytes = [long]$manifestFile.Length
        sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    })

    $prefix = (Split-Path -Leaf $Stage) + '/'
    $allNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $seenFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        foreach ($entry in $zip.Entries) {
            $normalized = Get-SafeArchiveEntryName -Name $entry.FullName
            $canonical = $normalized.TrimEnd('/')
            if (-not $allNames.Add($canonical)) { throw 'Archive contains duplicate case-insensitive entry names.' }
            if (-not $normalized.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Archive entry is outside the expected release root.'
            }
            $relative = $normalized.Substring($prefix.Length)
            if ([string]::IsNullOrEmpty($relative) -or $normalized.EndsWith('/')) { continue }
            if (-not $expected.ContainsKey($relative)) { throw "Archive has an unexpected file: $relative" }
            if (-not $seenFiles.Add($relative)) { throw "Archive repeats a release file: $relative" }
            $expectedEntry = $expected[$relative]
            if ([long]$entry.Length -ne [long]$expectedEntry.bytes) { throw "Archive size mismatch: $relative" }
            $stream = $entry.Open()
            try { $hash = Get-Sha256FromStream -Stream $stream } finally { $stream.Dispose() }
            if ($hash -ne [string]$expectedEntry.sha256) { throw "Archive hash mismatch: $relative" }
        }
    } finally {
        $zip.Dispose()
    }
    if ($seenFiles.Count -ne $expected.Count) {
        $missing = @($expected.Keys | Where-Object { -not $seenFiles.Contains($_) } | Select-Object -First 1)
        throw "Archive is incomplete. First missing file: $($missing[0])"
    }
    return $seenFiles.Count
}

function Assert-VcRuntimeDependencyManifest {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)]$DependencyManifest
    )

    $components = @($DependencyManifest.components | Where-Object { [string]$_.name -eq 'Microsoft Visual C++ Runtime' })
    if ($components.Count -ne 1) {
        throw 'Dependency manifest must contain exactly one Microsoft Visual C++ Runtime component.'
    }
    $component = $components[0]
    if (-not ($component.PSObject.Properties.Name -contains 'files')) {
        throw 'Microsoft Visual C++ Runtime dependency manifest file inventory is missing.'
    }
    $payload = Assert-VcRuntimePayload -Root $Root
    if ([string]$component.version -ne [string]$payload.Version -or
        [string]$component.architecture -ne 'x64' -or
        [string]$component.signatureStatus -ne 'Valid' -or
        [string]$component.source -ne 'Microsoft Visual Studio VC Redist x64 app-local CRT') {
        throw 'Microsoft Visual C++ Runtime dependency metadata does not match the packaged payload.'
    }

    $expected = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $payload.Files) {
        $expected.Add($file.Path, $file)
    }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in @($component.files)) {
        $relative = Get-SafeManifestRelativePath -Root $Root -RelativePath ([string]$entry.path)
        if (-not $seen.Add($relative)) { throw "Duplicate Microsoft Visual C++ runtime dependency path: $relative" }
        if (-not $expected.ContainsKey($relative)) { throw "Unexpected Microsoft Visual C++ runtime dependency path: $relative" }
        $actual = $expected[$relative]
        if ([long]$entry.bytes -ne [long]$actual.Bytes -or
            ([string]$entry.sha256).ToLowerInvariant() -ne [string]$actual.Sha256) {
            throw "Microsoft Visual C++ runtime dependency hash or size mismatch: $relative"
        }
    }
    if ($seen.Count -ne $expected.Count -or [int]$component.fileCount -ne $expected.Count) {
        throw 'Microsoft Visual C++ Runtime dependency manifest is incomplete.'
    }
}

function Assert-CompleteReleaseStage {
    param([Parameter(Mandatory)][string]$Root)
    Assert-ReleaseLayout -Root $Root -RequireManifests
    Assert-NoSensitiveContent -Root $Root
    $count = Assert-ReleaseManifest -Root $Root
    $dependency = [IO.File]::ReadAllText((Join-Path $Root 'dependency-manifest.json')) | ConvertFrom-Json
    $expectedComponentNames = @('Codex-Router', 'Sub2API', 'PostgreSQL', 'Redis', 'Microsoft Visual C++ Runtime')
    if ([int]$dependency.schemaVersion -ne 1 -or
        [string]$dependency.targetPlatform -ne 'windows-x64' -or
        @($dependency.components).Count -ne $expectedComponentNames.Count) {
        throw 'Dependency manifest is incomplete.'
    }
    foreach ($name in $expectedComponentNames) {
        $matches = @($dependency.components | Where-Object { [string]$_.name -eq $name })
        if ($matches.Count -ne 1 -or [string]$matches[0].architecture -ne 'x64') {
            throw "Dependency manifest component is missing or has the wrong architecture: $name"
        }
    }
    Assert-VcRuntimeDependencyManifest -Root $Root -DependencyManifest $dependency
    return $count
}

if (-not [string]::IsNullOrWhiteSpace($ScanOnlyPath)) {
    if (-not [string]::IsNullOrWhiteSpace($ValidateStage)) { throw 'Use either ScanOnlyPath or ValidateStage, not both.' }
    Assert-NoSensitiveContent -Root $ScanOnlyPath
    [ordered]@{ scanPath = [IO.Path]::GetFullPath($ScanOnlyPath); clean = $true } | ConvertTo-Json -Compress
    return
}

if (-not [string]::IsNullOrWhiteSpace($ValidateStage)) {
    $validated = [IO.Path]::GetFullPath($ValidateStage)
    $fileCount = Assert-CompleteReleaseStage -Root $validated
    [ordered]@{ stage = $validated; valid = $true; files = $fileCount + 1 } | ConvertTo-Json -Compress
    return
}

if ([string]::IsNullOrWhiteSpace($OutputRoot)) { $OutputRoot = Join-Path $routerRoot 'dist' }
$outputRootPath = [IO.Path]::GetFullPath($OutputRoot).TrimEnd([char[]]@('\', '/'))
if ($outputRootPath.Equals($routerRootPath, [StringComparison]::OrdinalIgnoreCase) -or
    $routerRootPath.StartsWith($outputRootPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Release output must not be the repository root or one of its parents.'
}

$version = Get-CargoPackageVersion
if (-not $SkipBuild) {
    $releaseDate = Get-Date -Format 'yyyy-MM-dd'
    Set-TermsReleaseMetadata -Version $version -ReleaseDate $releaseDate
    [void](Assert-TermsReleaseMetadata -Root $routerRoot -ExpectedVersion $version -ExpectedDate $releaseDate)
    if ([string]::IsNullOrWhiteSpace($env:CARGO_ENCODED_RUSTFLAGS) -and -not [string]::IsNullOrWhiteSpace($env:RUSTFLAGS)) {
        throw 'RUSTFLAGS is set. Move it to CARGO_ENCODED_RUSTFLAGS so the release builder can append path-remapping flags safely.'
    }
    $previousEncodedRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
    $flags = @()
    if (-not [string]::IsNullOrWhiteSpace($previousEncodedRustFlags)) {
        $flags += $previousEncodedRustFlags.Split([char]0x1f, [StringSplitOptions]::RemoveEmptyEntries)
    }
    $flags += "--remap-path-prefix=$routerRootPath=R:\src\codex-router"
    $userProfile = [Environment]::GetFolderPath('UserProfile')
    if (-not [string]::IsNullOrWhiteSpace($userProfile)) {
        $flags += "--remap-path-prefix=$([IO.Path]::GetFullPath($userProfile))=R:\home\builder"
    }
    try {
        $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
        & cargo build --release --locked --manifest-path (Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml')
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        $env:CARGO_ENCODED_RUSTFLAGS = $previousEncodedRustFlags
    }
} else {
    $releaseDate = Assert-TermsReleaseMetadata -Root $routerRoot -ExpectedVersion $version
}

$releaseExe = Join-Path $routerRoot 'codex-router-gui-rust\target\release\codex-router.exe'
foreach ($required in @(
    $releaseExe,
    (Join-Path $routerRoot 'app\sub2api.exe'),
    (Join-Path $routerRoot 'app\data\model_pricing.json'),
    (Join-Path $routerRoot 'postgres\pgsql\bin\initdb.exe'),
    (Join-Path $routerRoot 'redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe')
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Required release file is missing: $required" }
}
$vcRuntimeSource = Resolve-VcRedistCrtDirectory -OverridePath $VcRedistCrtDir

[IO.Directory]::CreateDirectory($outputRootPath) | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$stage = Join-Path $outputRootPath "Codex-Router-Portable-$version-windows-x64-$stamp"
$staging = Join-Path $outputRootPath ('.codex-router-staging-' + [Guid]::NewGuid().ToString('N'))
if (Test-Path -LiteralPath $stage) { throw "Release stage already exists: $stage" }
[IO.Directory]::CreateDirectory($staging) | Out-Null
$published = $false
$temporaryArchive = $null

function Copy-ReleaseItem {
    param(
        [Parameter(Mandatory)][string]$RelativePath,
        [string]$DestinationRelativePath = $RelativePath
    )
    $source = Join-Path $routerRoot $RelativePath
    if (-not (Test-Path -LiteralPath $source)) { throw "Release input is missing: $RelativePath" }
    Assert-NoReparsePoints -Root $source
    $destination = Join-Path $staging $DestinationRelativePath
    $parent = Split-Path -Parent $destination
    if ($parent) { [IO.Directory]::CreateDirectory($parent) | Out-Null }
    Copy-Item -LiteralPath $source -Destination $destination -Recurse -Force
}

function Copy-VcRuntimePayload {
    param([Parameter(Mandatory)]$SourceMetadata)

    foreach ($destinationDirectory in $vcRuntimeDestinationDirectories) {
        $destinationRoot = if ([string]::IsNullOrEmpty($destinationDirectory)) {
            $staging
        } else {
            Join-Path $staging $destinationDirectory
        }
        [IO.Directory]::CreateDirectory($destinationRoot) | Out-Null
        foreach ($name in $vcRuntimeFiles) {
            Copy-Item -LiteralPath (Join-Path $SourceMetadata.Path $name) -Destination (Join-Path $destinationRoot $name) -Force
        }
    }
    [void](Assert-VcRuntimePayload -Root $staging)
}

function Remove-PostgresDevelopmentPayload {
    param([Parameter(Mandatory)][string]$Root)

    $postgresRoot = Join-Path $Root 'postgres\pgsql'
    $developmentDirectories = @(
        (Join-Path $postgresRoot 'lib\pgxs'),
        (Join-Path $postgresRoot 'lib\pkgconfig')
    )
    $developmentPrefixes = @($developmentDirectories | ForEach-Object {
        [IO.Path]::GetFullPath($_).TrimEnd([char[]]@('\', '/')) + [IO.Path]::DirectorySeparatorChar
    })
    $developmentFiles = @(Get-ChildItem -LiteralPath $postgresRoot -Recurse -Force -File | Where-Object {
        $fullPath = [IO.Path]::GetFullPath($_.FullName)
        $_.Extension.ToLowerInvariant() -in @('.a', '.lib') -or
            @($developmentPrefixes | Where-Object {
                $fullPath.StartsWith($_, [StringComparison]::OrdinalIgnoreCase)
            }).Count -gt 0
    })
    $removedBytes = [long](($developmentFiles | Measure-Object Length -Sum).Sum)
    foreach ($file in $developmentFiles) {
        Remove-Item -LiteralPath $file.FullName -Force
    }
    foreach ($directory in $developmentDirectories) {
        if (Test-Path -LiteralPath $directory) {
            Remove-Item -LiteralPath $directory -Recurse -Force
        }
    }
    return [pscustomobject]@{
        Files = $developmentFiles.Count
        Bytes = $removedBytes
    }
}

function Remove-PostgresOptionalPayload {
    param([Parameter(Mandatory)][string]$Root)

    $postgresRoot = Join-Path $Root 'postgres\pgsql'
    $postgresBin = Join-Path $postgresRoot 'bin'
    $localeRoot = Join-Path $postgresRoot 'share\locale'
    $stackBuilderPath = Join-Path $postgresBin 'stackbuilder.exe'
    if (-not (Test-Path -LiteralPath $localeRoot -PathType Container)) {
        throw 'PostgreSQL locale payload is missing; review the portable trimming rules for this PostgreSQL version.'
    }
    if (-not (Test-Path -LiteralPath $stackBuilderPath -PathType Leaf)) {
        throw 'PostgreSQL StackBuilder is missing; review the portable trimming rules for this PostgreSQL version.'
    }

    $actualWxFiles = @(Get-ChildItem -LiteralPath $postgresBin -File -Force -Filter 'wx*.dll' | Sort-Object Name)
    $actualWxNames = @($actualWxFiles | ForEach-Object { $_.Name })
    $expectedWxNames = @($postgresStackBuilderWxFiles | Sort-Object)
    if (($actualWxNames -join "`n") -ne ($expectedWxNames -join "`n")) {
        throw "PostgreSQL wxWidgets payload changed. Expected: $($expectedWxNames -join ', '); actual: $($actualWxNames -join ', ')."
    }

    $localeFiles = @(Get-ChildItem -LiteralPath $localeRoot -Recurse -File -Force)
    if ($localeFiles.Count -eq 0) {
        throw 'PostgreSQL locale payload is unexpectedly empty; review the portable trimming rules.'
    }
    $stackBuilder = Get-Item -LiteralPath $stackBuilderPath -Force
    $stackBuilderBytes = [long]$stackBuilder.Length
    $localeBytes = [long](($localeFiles | Measure-Object Length -Sum).Sum)
    $wxBytes = [long](($actualWxFiles | Measure-Object Length -Sum).Sum)

    Remove-Item -LiteralPath $stackBuilderPath -Force
    foreach ($file in $actualWxFiles) { Remove-Item -LiteralPath $file.FullName -Force }
    Remove-Item -LiteralPath $localeRoot -Recurse -Force

    return [pscustomobject]@{
        Files = $localeFiles.Count + $actualWxFiles.Count + 1
        Bytes = $localeBytes + $wxBytes + $stackBuilderBytes
        LocaleFiles = $localeFiles.Count
        LocaleBytes = $localeBytes
        StackBuilderFiles = 1
        StackBuilderBytes = $stackBuilderBytes
        WxFiles = $actualWxFiles.Count
        WxBytes = $wxBytes
    }
}

try {
    Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $staging 'Codex-Router.exe')
    foreach ($relative in @(
        'app\sub2api.exe',
        'app\data\model_pricing.json',
        'app\data\model_pricing.sha256',
        'assets\logo.ico',
        'assets\logo.png',
        'postgres\pgsql\bin',
        'postgres\pgsql\lib',
        'postgres\pgsql\share',
        'postgres\pgsql\server_license.txt',
        'postgres\pgsql\commandlinetools_3rd_party_licenses.txt'
    )) { Copy-ReleaseItem -RelativePath $relative }
    Copy-ReleaseItem `
        -RelativePath 'app\data\model_pricing.json' `
        -DestinationRelativePath 'app\resources\model-pricing\model_prices_and_context_window.json'
    $postgresDevelopmentTrim = Remove-PostgresDevelopmentPayload -Root $staging
    $postgresOptionalTrim = Remove-PostgresOptionalPayload -Root $staging
    Copy-VcRuntimePayload -SourceMetadata $vcRuntimeSource
    Copy-ReleaseItem -RelativePath 'app\LICENSE' -DestinationRelativePath 'licenses\Sub2API-LICENSE.txt'
    foreach ($name in $staticLicenseFiles) { Copy-ReleaseItem -RelativePath "licenses\$name" }
    Write-RustThirdPartyLicenseBundle -DestinationPath (Join-Path $staging 'licenses\Rust-Crates-LICENSES.txt')

    $redisRoot = 'redis\Redis-8.10.0-Windows-x64-msys2'
    foreach ($name in $redisRuntimeFiles) { Copy-ReleaseItem -RelativePath "$redisRoot\$name" }
    foreach ($relative in @('LICENSE', 'README.md', 'README.zh-CN.md', 'TERMS.en.md', 'TERMS.zh-CN.md', 'THIRD_PARTY_NOTICES.md')) {
        Copy-ReleaseItem -RelativePath $relative
    }
    [void](Assert-TermsReleaseMetadata -Root $staging -ExpectedVersion $version -ExpectedDate $releaseDate)
    foreach ($name in $runtimeScripts) { Copy-ReleaseItem -RelativePath "scripts\$name" }
    foreach ($name in $configFiles) { Copy-ReleaseItem -RelativePath "config\$name" }

    Assert-ReleaseLayout -Root $staging
    [void](Write-ReleaseManifests -Root $staging -Version $version)
    $fileCount = Assert-CompleteReleaseStage -Root $staging

    Move-Item -LiteralPath $staging -Destination $stage
    $published = $true

    $archive = $null
    if (-not $SkipArchive) {
        $archive = "$stage.zip"
        if (Test-Path -LiteralPath $archive) { throw "Release archive already exists: $archive" }
        $temporaryArchive = Join-Path $outputRootPath ('.codex-router-archive-' + [Guid]::NewGuid().ToString('N') + '.zip')
        Compress-Archive -LiteralPath $stage -DestinationPath $temporaryArchive -CompressionLevel Optimal
        [void](Assert-ArchiveMatchesReleaseManifest -Stage $stage -Archive $temporaryArchive)
        Move-Item -LiteralPath $temporaryArchive -Destination $archive
        $temporaryArchive = $null
    }

    [ordered]@{
        stage = $stage
        archive = $archive
        version = $version
        platform = 'windows-x64'
        files = $fileCount + 1
        bytes = [long]((Get-ChildItem -LiteralPath $stage -Recurse -File | Measure-Object Length -Sum).Sum)
        trimmedPostgresDevelopmentFiles = [int]$postgresDevelopmentTrim.Files
        trimmedPostgresDevelopmentBytes = [long]$postgresDevelopmentTrim.Bytes
        trimmedPostgresOptionalFiles = [int]$postgresOptionalTrim.Files
        trimmedPostgresOptionalBytes = [long]$postgresOptionalTrim.Bytes
        trimmedPostgresLocaleFiles = [int]$postgresOptionalTrim.LocaleFiles
        trimmedPostgresLocaleBytes = [long]$postgresOptionalTrim.LocaleBytes
        trimmedPostgresStackBuilderFiles = [int]$postgresOptionalTrim.StackBuilderFiles
        trimmedPostgresStackBuilderBytes = [long]$postgresOptionalTrim.StackBuilderBytes
        trimmedPostgresWxFiles = [int]$postgresOptionalTrim.WxFiles
        trimmedPostgresWxBytes = [long]$postgresOptionalTrim.WxBytes
        vcRuntimeVersion = $vcRuntimeSource.Version
        vcRuntimeSourceKind = $vcRuntimeSource.SourceKind
        vcRuntimeFiles = $vcRuntimeFiles.Count * $vcRuntimeDestinationDirectories.Count
        executableSha256 = (Get-FileHash -LiteralPath (Join-Path $stage 'Codex-Router.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        privateRuntimeStateIncluded = $false
        fullTreeSecretScan = $true
    } | ConvertTo-Json -Compress
} finally {
    if ($null -ne $temporaryArchive -and (Test-Path -LiteralPath $temporaryArchive)) {
        $resolvedTemporaryArchive = [IO.Path]::GetFullPath($temporaryArchive)
        if ($resolvedTemporaryArchive.StartsWith($outputRootPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedTemporaryArchive).StartsWith('.codex-router-archive-', [StringComparison]::Ordinal) -and
            $resolvedTemporaryArchive.EndsWith('.zip', [StringComparison]::OrdinalIgnoreCase)) {
            Remove-Item -LiteralPath $resolvedTemporaryArchive -Force
        }
    }
    if (-not $published -and (Test-Path -LiteralPath $staging)) {
        $resolvedStaging = [IO.Path]::GetFullPath($staging)
        if ($resolvedStaging.StartsWith($outputRootPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedStaging).StartsWith('.codex-router-staging-', [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedStaging -Recurse -Force
        }
    }
}
