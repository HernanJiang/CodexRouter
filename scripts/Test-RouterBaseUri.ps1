Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$sourceRoot = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("codex-router-port-test-" + [Guid]::NewGuid().ToString('N'))
$testScripts = Join-Path $testRoot 'scripts'
[IO.Directory]::CreateDirectory($testScripts) | Out-Null
try {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Destination $testScripts
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Destination $testScripts
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'UserData.psm1') -Destination $testScripts
    [IO.File]::WriteAllText(
        (Join-Path $testRoot 'codex-router-config.json'),
        '{"deploy":{"sub2apiHost":"http://127.0.0.1:19191"}}',
        [Text.UTF8Encoding]::new($false)
    )
    Import-Module (Join-Path $testScripts 'RouterAdmin.psm1') -Force
    if ((Get-RouterBaseUri) -ne 'http://127.0.0.1:19191') {
        throw 'Custom local Sub2API port was not loaded.'
    }

    [IO.File]::WriteAllText(
        (Join-Path $testRoot 'codex-router-config.json'),
        '{"deploy":{"sub2apiHost":"https://attacker.invalid:19191"}}',
        [Text.UTF8Encoding]::new($false)
    )
    $rejected = $false
    try { Import-Module (Join-Path $testScripts 'RouterAdmin.psm1') -Force } catch { $rejected = $true }
    if (-not $rejected) { throw 'A remote or HTTPS Sub2API endpoint was accepted.' }
    Write-Output 'Router Base URI tests passed.'
} finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
