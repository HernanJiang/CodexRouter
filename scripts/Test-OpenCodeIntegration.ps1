Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-opencode-' + [Guid]::NewGuid().ToString('N'))
$configDir = Join-Path $testRoot 'opencode'
$routerConfigPath = Join-Path $testRoot 'router.json'
[IO.Directory]::CreateDirectory($configDir) | Out-Null
Import-Module (Join-Path $routerRoot 'scripts\CodexIntegration.psm1') -Force

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

try {
    @'
{
  "$schema": "https://opencode.ai/config.json",
  "permission": "allow",
  "provider": {
    "existing": {
      "name": "Keep Me",
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "https://example.invalid/v1" },
      "models": { "existing-model": { "name": "Existing" } }
    }
  }
}
'@ | Set-Content -LiteralPath (Join-Path $configDir 'opencode.json') -Encoding utf8
    @'
{
  "models": [
    { "model": "gpt-5.6-sol", "alias": "GPT 5.6 Sol" },
    { "model": "deepseek-v4-flash", "alias": "DeepSeek V4 Flash" }
  ]
}
'@ | Set-Content -LiteralPath $routerConfigPath -Encoding utf8

    & (Join-Path $routerRoot 'scripts\Install-OpenCodeIntegration.ps1') `
        -RouterConfigPath $routerConfigPath `
        -OpenCodeConfigDir $configDir `
        -BaseUrl 'http://127.0.0.1:18080' | Out-Null

    $result = Get-Content -LiteralPath (Join-Path $configDir 'opencode.json') -Raw | ConvertFrom-Json
    Assert-True ($result.permission -eq 'allow') 'Existing top-level OpenCode settings were overwritten.'
    Assert-True ($result.provider.existing.name -eq 'Keep Me') 'Existing OpenCode provider was overwritten.'
    Assert-True ($result.provider.'codex-router'.options.baseURL -eq 'http://127.0.0.1:18080/v1') 'Router Base URL is incorrect.'
    Assert-True ($result.provider.'codex-router'.options.apiKey -eq '{env:CODEX_ROUTER_API_KEY}') 'Router key was not referenced through the environment.'
    Assert-True (@($result.provider.'codex-router'.models.PSObject.Properties).Count -eq 2) 'Router model list is incomplete.'

    $backupCount = @(Get-ChildItem -LiteralPath $configDir -Filter 'opencode.json.codex-router-*.bak').Count
    & (Join-Path $routerRoot 'scripts\Install-OpenCodeIntegration.ps1') `
        -RouterConfigPath $routerConfigPath `
        -OpenCodeConfigDir $configDir `
        -BaseUrl 'http://127.0.0.1:18080' | Out-Null
    $backupCountAgain = @(Get-ChildItem -LiteralPath $configDir -Filter 'opencode.json.codex-router-*.bak').Count
    Assert-True ($backupCountAgain -eq $backupCount) 'Unchanged integration created an unnecessary backup.'

    foreach ($index in 1..5) {
        [IO.File]::WriteAllText(
            (Join-Path $configDir "opencode.json.codex-router-test-$index.bak"),
            "backup-$index",
            [Text.UTF8Encoding]::new($false)
        )
    }
    Limit-CodexRouterBackups `
        -Directory $configDir `
        -Filter 'opencode.json.codex-router-*.bak' `
        -Keep 3
    $limitedBackupCount = @(Get-ChildItem -LiteralPath $configDir -Filter 'opencode.json.codex-router-*.bak').Count
    Assert-True ($limitedBackupCount -eq 3) 'OpenCode backup retention did not stop at three files.'

    Write-Output 'OpenCode integration tests passed.'
} finally {
    $resolvedTestRoot = [IO.Path]::GetFullPath($testRoot)
    $resolvedTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ($resolvedTestRoot.StartsWith($resolvedTempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        (Test-Path -LiteralPath $resolvedTestRoot)) {
        Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
    }
}
