param(
    [string]$StagePath,
    [string]$ArchivePath,
    [switch]$FaultInjection,
    [switch]$Stress,
    [ValidateRange(1, 32)][int]$StressIterations = 4,
    [ValidateRange(1, 8)][int]$StressWorkers = 2,
    [switch]$SkipToolchainTests
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Initialize-StandardPowerShellModulePath {
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        $desktopModuleRoot = Join-Path $PSHOME 'Modules'
        $modulePaths = @($env:PSModulePath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($modulePaths -notcontains $desktopModuleRoot) {
            $env:PSModulePath = $desktopModuleRoot + ';' + ($modulePaths -join ';')
        }
        Import-Module (Join-Path $desktopModuleRoot 'Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1') -ErrorAction Stop
    } else {
        Import-Module Microsoft.PowerShell.Utility -ErrorAction Stop
    }
}

Initialize-StandardPowerShellModulePath

$routerRoot = Split-Path -Parent $PSScriptRoot
$releaseBuilder = Join-Path $PSScriptRoot 'Build-PortableRelease.ps1'
$acceptanceRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-acceptance-' + [Guid]::NewGuid().ToString('N'))
$utf8NoBom = [Text.UTF8Encoding]::new($false)
$results = [Collections.Generic.List[object]]::new()
$hadFailure = $false
$currentDetail = ''
$suiteWatch = [Diagnostics.Stopwatch]::StartNew()

function Protect-AcceptanceText {
    param([AllowNull()][string]$Text)
    if ([string]::IsNullOrWhiteSpace($Text)) { return '' }
    $value = $Text
    $value = [regex]::Replace($value, '(?i)\bBearer\s+[a-z0-9._~+/-]{8,}={0,2}', 'Bearer <redacted>')
    $value = [regex]::Replace($value, '(?i)(?<![a-z0-9])sk-(?:ant-|proj-|svcacct-|admin-|local-)?[a-z0-9_-]{12,}', '<redacted-key>')
    $value = [regex]::Replace($value, '(?i)\b(?:ghp|github_pat)_[a-z0-9_]{12,}\b', '<redacted-token>')
    $value = [regex]::Replace($value, '\beyJ[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\b', '<redacted-jwt>')
    $value = [regex]::Replace($value, '(?i)(://[^:/\s]+:)[^@/\s]+@', '$1<redacted>@')
    $value = [regex]::Replace(
        $value,
        '(?i)((?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)\s*[:=]\s*["'']?)[^\s,"'']{8,}',
        '$1<redacted>')
    $value = [regex]::Replace($value, '(?i)[a-z]:\\Users\\[^\\\s"''<>|]{1,96}', '<user-path>')
    return $value.Trim()
}

function Add-AcceptanceResult {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][ValidateSet('passed', 'failed', 'skipped')][string]$Status,
        [Parameter(Mandatory)][long]$Milliseconds,
        [string]$Detail = ''
    )
    $results.Add([pscustomobject][ordered]@{
        name = $Name
        status = $Status
        milliseconds = $Milliseconds
        detail = Protect-AcceptanceText -Text $Detail
    })
}

function Set-AcceptanceDetail {
    param([string]$Detail)
    $script:currentDetail = $Detail
}

function Invoke-AcceptanceCheck {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
    )
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $script:currentDetail = ''
    try {
        & $Action | Out-Null
        $watch.Stop()
        Add-AcceptanceResult -Name $Name -Status passed -Milliseconds $watch.ElapsedMilliseconds -Detail $script:currentDetail
    } catch {
        $watch.Stop()
        $script:hadFailure = $true
        Add-AcceptanceResult `
            -Name $Name `
            -Status failed `
            -Milliseconds $watch.ElapsedMilliseconds `
            -Detail $_.Exception.Message
    }
}

function Invoke-CapturedCommand {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$Label
    )
    $logPath = Join-Path $acceptanceRoot ('command-' + [Guid]::NewGuid().ToString('N') + '.log')
    try {
        # Windows PowerShell 5 converts a native program's stderr records into
        # PowerShell errors. Cargo progress and unittest dots use stderr even
        # on success, so evaluate the native exit code instead of treating
        # ordinary stderr output as a terminating failure.
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = 'Continue'
            & $Executable @Arguments *> $logPath
            $exitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        if ($exitCode -ne 0) {
            $tail = if (Test-Path -LiteralPath $logPath) {
                (Get-Content -LiteralPath $logPath -Tail 60 | Out-String)
            } else { '' }
            throw "$Label failed with exit code $exitCode. $(Protect-AcceptanceText -Text $tail)"
        }
    } finally {
        if (Test-Path -LiteralPath $logPath) { Remove-Item -LiteralPath $logPath -Force }
    }
}

function Assert-PowerShellSyntax {
    $errors = [Collections.Generic.List[string]]::new()
    $files = @(
        Get-ChildItem -LiteralPath $PSScriptRoot -File |
            Where-Object { $_.Extension -in @('.ps1', '.psm1') } |
            Sort-Object Name
    )
    foreach ($file in $files) {
        $tokens = $null
        $parseErrors = $null
        [void][Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$parseErrors)
        foreach ($parseError in @($parseErrors)) {
            $errors.Add("$($file.Name):$($parseError.Extent.StartLineNumber): $($parseError.Message)")
        }
    }
    if ($errors.Count -gt 0) { throw ($errors -join [Environment]::NewLine) }
    Set-AcceptanceDetail -Detail "$($files.Count) PowerShell files"
}

function Assert-CleanScanFixture {
    $fixture = Join-Path $acceptanceRoot 'clean-scan'
    [IO.Directory]::CreateDirectory((Join-Path $fixture 'nested')) | Out-Null
    [IO.File]::WriteAllText(
        (Join-Path $fixture 'nested\example.json'),
        '{"api_key":"{env:CODEX_ROUTER_API_KEY}","endpoint":"http://127.0.0.1:18080"}',
        $utf8NoBom)
    [IO.File]::WriteAllText(
        (Join-Path $fixture 'README.txt'),
        'Portable acceptance fixture with no credentials.',
        $utf8NoBom)
    [IO.File]::WriteAllText(
        (Join-Path $fixture 'nested\example.ps1'),
        '$apiKey = Get-RouterCredential -Name ''LocalApiKey''',
        $utf8NoBom)
    & $releaseBuilder -ScanOnlyPath $fixture | Out-Null
    Set-AcceptanceDetail -Detail 'ASCII, placeholder, and nonliteral script fixtures accepted'
}

function Assert-FaultInjectionCoverage {
    $faultRoot = Join-Path $acceptanceRoot 'fault-injection'
    [IO.Directory]::CreateDirectory($faultRoot) | Out-Null
    $token = 'sk-' + 'local-' + ('A' * 32)
    $privateKeyHeader = '-----BEGIN ' + 'PRIVATE KEY-----'
    $fakeUserPath = 'C:' + '\Users\' + 'PortableLeakFixture' + '\AppData\Local'
    $fakeForwardUserPath = 'C:' + '/Users/' + 'PortableLeakFixture' + '/AppData/Local'
    $structuredSecret = '{"password":"' + ('P' * 32) + '"}'
    $scriptSecret = '$apiKey = ''portable-literal-secret-1234567890'''
    $moduleSecret = '$client_secret = ''portable-module-secret-1234567890'''
    $pythonSecret = 'password = "portable-python-secret-1234567890"'
    $placeholderSubstringSecret = '{"password":"real-sample-secret-1234567890"}'
    $boundaryText = ('x' * ((1024 * 1024) - 6)) + "`n" + $token
    $fixtures = @(
        [pscustomobject]@{ Name = 'ascii-token'; File = 'token.txt'; Text = $token; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'utf16-token'; File = 'token-wide.txt'; Text = $token; Encoding = [Text.Encoding]::Unicode },
        [pscustomobject]@{ Name = 'chunk-boundary'; File = 'boundary.bin'; Text = $boundaryText; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'private-key'; File = 'private.pem'; Text = $privateKeyHeader; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'user-path'; File = 'path.txt'; Text = $fakeUserPath; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'forward-user-path'; File = 'path-forward.txt'; Text = $fakeForwardUserPath; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'structured-secret'; File = 'settings.json'; Text = $structuredSecret; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'powershell-secret'; File = 'settings.ps1'; Text = $scriptSecret; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'module-secret'; File = 'settings.psm1'; Text = $moduleSecret; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'python-secret'; File = 'settings.py'; Text = $pythonSecret; Encoding = $utf8NoBom },
        [pscustomobject]@{ Name = 'placeholder-substring'; File = 'substring.json'; Text = $placeholderSubstringSecret; Encoding = $utf8NoBom }
    )
    foreach ($fixture in $fixtures) {
        $fixtureRoot = Join-Path $faultRoot $fixture.Name
        [IO.Directory]::CreateDirectory($fixtureRoot) | Out-Null
        [IO.File]::WriteAllText((Join-Path $fixtureRoot $fixture.File), $fixture.Text, $fixture.Encoding)
        $rejected = $false
        try {
            & $releaseBuilder -ScanOnlyPath $fixtureRoot | Out-Null
        } catch {
            if ($_.Exception.Message -notlike "Release scan rejected '*") {
                throw "Fault fixture '$($fixture.Name)' failed for an unexpected reason."
            }
            $rejected = $true
        }
        if (-not $rejected) { throw "Secret scanner accepted fault fixture '$($fixture.Name)'." }
    }
    $token = $null
    $structuredSecret = $null
    $scriptSecret = $null
    $moduleSecret = $null
    $pythonSecret = $null
    $placeholderSubstringSecret = $null
    $boundaryText = $null
    Set-AcceptanceDetail -Detail "$($fixtures.Count) leak fixtures rejected without echoing content"
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

function Assert-SafeArchiveEntryName {
    param([Parameter(Mandatory)][string]$Name)
    $normalized = $Name.Replace('\', '/')
    if ([string]::IsNullOrWhiteSpace($normalized) -or
        $normalized.StartsWith('/') -or
        $normalized -match '^[a-zA-Z]:' -or
        $normalized.IndexOf([char]0) -ge 0) {
        throw 'Archive contains an absolute or invalid entry path.'
    }
    foreach ($segment in $normalized.Split('/')) {
        if ($segment -in @('.', '..')) { throw 'Archive contains a traversal entry.' }
    }
    return $normalized
}

function Assert-ArchiveMatchesStage {
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
        $relative = ([string]$entry.path).Replace('\', '/')
        if ($expected.ContainsKey($relative)) { throw "Duplicate manifest path: $relative" }
        $expected.Add($relative, [pscustomobject]@{
            bytes = [long]$entry.bytes
            sha256 = ([string]$entry.sha256).ToLowerInvariant()
        })
    }
    if ($expected.ContainsKey('release-manifest.json')) {
        throw 'The release manifest must not hash itself.'
    }
    $manifestFile = Get-Item -LiteralPath $manifestPath
    $expected.Add('release-manifest.json', [pscustomobject]@{
        bytes = [long]$manifestFile.Length
        sha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    })

    $stageLeaf = Split-Path -Leaf $Stage
    $prefix = $stageLeaf + '/'
    $allNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $seenFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        foreach ($entry in $zip.Entries) {
            $normalized = Assert-SafeArchiveEntryName -Name $entry.FullName
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
            if ([long]$entry.Length -ne [long]$expectedEntry.bytes) {
                throw "Archive size mismatch: $relative"
            }
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
    Set-AcceptanceDetail -Detail "$($seenFiles.Count) archive files matched stage hashes"
}

function Invoke-StageValidationStress {
    param(
        [Parameter(Mandatory)][string]$Stage,
        [Parameter(Mandatory)][int]$Iterations,
        [Parameter(Mandatory)][int]$Workers
    )
    for ($offset = 0; $offset -lt $Iterations; $offset += $Workers) {
        $batchSize = [Math]::Min($Workers, $Iterations - $offset)
        $jobs = @()
        try {
            foreach ($index in 1..$batchSize) {
                $jobs += Start-Job -ScriptBlock {
                    param($BuilderPath, $ValidatedStage)
                    Set-StrictMode -Version Latest
                    $ErrorActionPreference = 'Stop'
                    & $BuilderPath -ValidateStage $ValidatedStage | Out-Null
                } -ArgumentList $releaseBuilder, $Stage
            }
            Wait-Job -Job $jobs | Out-Null
            foreach ($job in $jobs) {
                Receive-Job -Job $job -ErrorAction Stop | Out-Null
                if ($job.State -ne 'Completed') {
                    $reason = if ($job.ChildJobs.Count -gt 0) { $job.ChildJobs[0].JobStateInfo.Reason } else { $null }
                    throw "Concurrent validation failed. $reason"
                }
            }
        } finally {
            foreach ($job in $jobs) {
                if ($job.State -eq 'Running') { Stop-Job -Job $job -ErrorAction SilentlyContinue }
                Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
            }
        }
    }
    Set-AcceptanceDetail -Detail "$Iterations complete validations with up to $Workers workers"
}

function Resolve-OptionalStage {
    if (-not [string]::IsNullOrWhiteSpace($StagePath)) {
        return [IO.Path]::GetFullPath($StagePath)
    }
    if (-not [string]::IsNullOrWhiteSpace($ArchivePath)) {
        $archiveFullPath = [IO.Path]::GetFullPath($ArchivePath)
        if ($archiveFullPath.EndsWith('.zip', [StringComparison]::OrdinalIgnoreCase)) {
            $candidate = $archiveFullPath.Substring(0, $archiveFullPath.Length - 4)
            if (Test-Path -LiteralPath $candidate -PathType Container) { return $candidate }
        }
    }
    $dist = Join-Path $routerRoot 'dist'
    if (-not (Test-Path -LiteralPath $dist -PathType Container)) { return $null }
    $candidate = Get-ChildItem -LiteralPath $dist -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'dependency-manifest.json') -PathType Leaf } |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1
    if ($null -eq $candidate) { return $null }
    return $candidate.FullName
}

[IO.Directory]::CreateDirectory($acceptanceRoot) | Out-Null
try {
    if (-not (Test-Path -LiteralPath $releaseBuilder -PathType Leaf)) {
        throw 'Build-PortableRelease.ps1 is required for local acceptance.'
    }

    $resolvedStage = Resolve-OptionalStage
    if ($null -ne $resolvedStage -and -not (Test-Path -LiteralPath $resolvedStage -PathType Container)) {
        throw "Release stage does not exist: $resolvedStage"
    }

    $resolvedArchive = $null
    if (-not [string]::IsNullOrWhiteSpace($ArchivePath)) {
        $resolvedArchive = [IO.Path]::GetFullPath($ArchivePath)
        if (-not (Test-Path -LiteralPath $resolvedArchive -PathType Leaf)) {
            throw "Release archive does not exist: $resolvedArchive"
        }
    } elseif ($null -ne $resolvedStage -and (Test-Path -LiteralPath ($resolvedStage + '.zip') -PathType Leaf)) {
        $resolvedArchive = $resolvedStage + '.zip'
    }

    Invoke-AcceptanceCheck -Name 'powershell-syntax' -Action { Assert-PowerShellSyntax }
    Invoke-AcceptanceCheck -Name 'clean-secret-scan' -Action { Assert-CleanScanFixture }

    if ($FaultInjection) {
        Invoke-AcceptanceCheck -Name 'secret-scan-fault-injection' -Action { Assert-FaultInjectionCoverage }
    } else {
        Add-AcceptanceResult -Name 'secret-scan-fault-injection' -Status skipped -Milliseconds 0 -Detail 'Enable with -FaultInjection.'
    }

    if (-not $SkipToolchainTests) {
        $cargo = (Get-Command cargo.exe -ErrorAction Stop).Source
        $pythonCommand = Get-Command python.exe -ErrorAction Stop
        $python = $pythonCommand.Source
        $manifestPath = Join-Path $routerRoot 'codex-router-gui-rust\Cargo.toml'

        Invoke-AcceptanceCheck -Name 'rust-format' -Action {
            Invoke-CapturedCommand -Executable $cargo -Arguments @('fmt', '--manifest-path', $manifestPath, '--', '--check') -Label 'cargo fmt'
        }
        Invoke-AcceptanceCheck -Name 'rust-clippy' -Action {
            Invoke-CapturedCommand -Executable $cargo -Arguments @('clippy', '--locked', '--manifest-path', $manifestPath, '--all-targets', '--', '-D', 'warnings') -Label 'cargo clippy'
        }
        Invoke-AcceptanceCheck -Name 'rust-tests' -Action {
            Invoke-CapturedCommand -Executable $cargo -Arguments @('test', '--locked', '--manifest-path', $manifestPath) -Label 'cargo test'
        }
        Invoke-AcceptanceCheck -Name 'python-tests' -Action {
            Push-Location -LiteralPath $routerRoot
            try {
                Invoke-CapturedCommand `
                    -Executable $python `
                    -Arguments @('-m', 'unittest', 'discover', '-s', 'scripts', '-p', 'test_*.py') `
                    -Label 'Python unit tests'
            } finally {
                Pop-Location
            }
        }
        foreach ($testName in @(
            'Test-CodexIntegration.ps1',
            'Test-CredentialStore.ps1',
            'Test-OAuthRouting.ps1',
            'Test-OpenAIChannelPolicy.ps1',
            'Test-ManagedProxy.ps1',
            'Test-ProxyDiscovery.ps1',
            'Test-RouterBaseUri.ps1'
        )) {
            $testPath = Join-Path $PSScriptRoot $testName
            Invoke-AcceptanceCheck -Name ('powershell-' + [IO.Path]::GetFileNameWithoutExtension($testName).ToLowerInvariant()) -Action {
                & $testPath | Out-Null
            }
        }
    } else {
        Add-AcceptanceResult -Name 'toolchain-and-source-tests' -Status skipped -Milliseconds 0 -Detail 'Skipped by -SkipToolchainTests.'
    }

    if ($null -ne $resolvedStage) {
        Invoke-AcceptanceCheck -Name 'release-stage' -Action {
            & $releaseBuilder -ValidateStage $resolvedStage | Out-Null
            $stageFiles = @(Get-ChildItem -LiteralPath $resolvedStage -Recurse -File -Force)
            $stageBytes = [long](($stageFiles | Measure-Object Length -Sum).Sum)
            Set-AcceptanceDetail -Detail ("{0} files, {1:N1} MiB" -f $stageFiles.Count, ($stageBytes / 1MB))
        }
    } else {
        Add-AcceptanceResult -Name 'release-stage' -Status skipped -Milliseconds 0 -Detail 'No completed dependency-manifest stage was found.'
    }

    if ($null -ne $resolvedArchive -and $null -ne $resolvedStage) {
        Invoke-AcceptanceCheck -Name 'release-archive' -Action {
            Assert-ArchiveMatchesStage -Stage $resolvedStage -Archive $resolvedArchive
        }
    } else {
        Add-AcceptanceResult -Name 'release-archive' -Status skipped -Milliseconds 0 -Detail 'No archive and matching stage were selected.'
    }

    if ($Stress) {
        if ($null -eq $resolvedStage) {
            $hadFailure = $true
            Add-AcceptanceResult -Name 'concurrent-stage-validation' -Status failed -Milliseconds 0 -Detail '-Stress requires a completed release stage.'
        } else {
            Invoke-AcceptanceCheck -Name 'concurrent-stage-validation' -Action {
                Invoke-StageValidationStress -Stage $resolvedStage -Iterations $StressIterations -Workers $StressWorkers
            }
        }
    } else {
        Add-AcceptanceResult -Name 'concurrent-stage-validation' -Status skipped -Milliseconds 0 -Detail 'Enable with -Stress.'
    }

    $suiteWatch.Stop()
    $passed = @($results | Where-Object status -eq 'passed').Count
    $failed = @($results | Where-Object status -eq 'failed').Count
    $skipped = @($results | Where-Object status -eq 'skipped').Count
    [ordered]@{
        schemaVersion = 1
        passed = $passed
        failed = $failed
        skipped = $skipped
        milliseconds = $suiteWatch.ElapsedMilliseconds
        stage = if ($null -eq $resolvedStage) { $null } else { Split-Path -Leaf $resolvedStage }
        archive = if ($null -eq $resolvedArchive) { $null } else { Split-Path -Leaf $resolvedArchive }
        networkOrPaidApiCalls = $false
        results = @($results)
    } | ConvertTo-Json -Depth 6

    if ($hadFailure) { throw 'Local acceptance failed; see the redacted result summary above.' }
} finally {
    if (Test-Path -LiteralPath $acceptanceRoot) {
        $resolvedAcceptanceRoot = [IO.Path]::GetFullPath($acceptanceRoot)
        $resolvedTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
        if ($resolvedAcceptanceRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedAcceptanceRoot).StartsWith('codex-router-acceptance-', [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedAcceptanceRoot -Recurse -Force
        }
    }
}
