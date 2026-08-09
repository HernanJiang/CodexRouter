param(
    [string]$Providers = '',
    [string]$Checks = '',
    [switch]$AgenticOnly,
    [string]$OutputPath = (Join-Path $env:TEMP 'codex-router-provider-protocol-matrix.json')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force

$credentialMap = [ordered]@{
    PROBE_KEY_CHIRAL_SOL = 'ModelApiKey-1-gpt-5-6-sol'
    PROBE_KEY_OPENROUTER = 'ModelApiKey-2-deepseek-deepseek-v4-flash'
    PROBE_KEY_KIMI = 'ModelApiKey-3-k3-256k'
    PROBE_KEY_CHIRAL_LUNA = 'ModelApiKey-4-gpt-5-6-luna'
}
$providerCredentials = @{
    'chiral-sol' = @('PROBE_KEY_CHIRAL_SOL')
    'chiral-luna' = @('PROBE_KEY_CHIRAL_LUNA')
    'openrouter-deepseek' = @('PROBE_KEY_OPENROUTER')
    'openrouter-grok' = @('PROBE_KEY_OPENROUTER')
    'openrouter-gemini' = @('PROBE_KEY_OPENROUTER')
    'kimi-coding' = @('PROBE_KEY_KIMI')
}
$requiredEnvironmentNames = if ([string]::IsNullOrWhiteSpace($Providers)) {
    @($credentialMap.Keys)
} else {
    @($Providers.Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ } | ForEach-Object {
        if (-not $providerCredentials.ContainsKey($_)) { throw "Unknown provider: $_" }
        $providerCredentials[$_]
    } | Select-Object -Unique)
}
$previous = @{}
try {
    foreach ($entry in $credentialMap.GetEnumerator() | Where-Object { $requiredEnvironmentNames -contains $_.Key }) {
        $previous[$entry.Key] = [Environment]::GetEnvironmentVariable($entry.Key, 'Process')
        $secret = Get-RouterCredential -Name $entry.Value -AllowMissing
        if ([string]::IsNullOrWhiteSpace($secret)) {
            throw "Required credential is missing: $($entry.Value)"
        }
        [Environment]::SetEnvironmentVariable($entry.Key, $secret, 'Process')
        $secret = $null
    }
    $python = (Get-Command python.exe -ErrorAction Stop).Source
    $probeArguments = @(
        (Join-Path $PSScriptRoot 'probe_provider_protocols.py'),
        '--proxy', 'http://127.0.0.1:7897',
        '--timeout', '45',
        '--extended',
        '--output', $OutputPath
    )
    if (-not [string]::IsNullOrWhiteSpace($Providers)) {
        $probeArguments += @('--providers', $Providers)
    }
    if (-not [string]::IsNullOrWhiteSpace($Checks)) {
        $probeArguments += @('--checks', $Checks)
    }
    if ($AgenticOnly) {
        $probeArguments += '--agentic-only'
    }
    & $python @probeArguments
    if ($LASTEXITCODE -ne 0) {
        throw "Provider protocol probe failed with exit code $LASTEXITCODE."
    }
} finally {
    foreach ($entry in $credentialMap.GetEnumerator() | Where-Object { $previous.ContainsKey($_.Key) }) {
        [Environment]::SetEnvironmentVariable($entry.Key, $previous[$entry.Key], 'Process')
    }
    $previous.Clear()
}
