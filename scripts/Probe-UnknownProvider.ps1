Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force

function Get-LocalKey {
    foreach ($target in @('CodexRouter/75c4c05a/LocalApiKey', 'CodexRouter/LocalApiKey')) {
        $value = [CodexRouter.CredentialNative]::Read($target)
        if (-not [string]::IsNullOrWhiteSpace($value)) { return $value }
    }
    throw 'LocalApiKey missing'
}

function Summarize-Body([string]$text) {
    if ([string]::IsNullOrWhiteSpace($text)) { return '' }
    $safe = $text
    if ($safe.Length -gt 240) { $safe = $safe.Substring(0, 240) }
    $safe = [regex]::Replace($safe, '(?i)(sk-|Bearer |api[_-]?key|token)[^\s"]*', '[REDACTED]')
    $safe = $safe -replace '\s+', ' '
    return $safe.Trim()
}

function Classify-Result([int]$status, [string]$body) {
    $lower = $body.ToLowerInvariant()
    if ($lower.Contains('unknown provider for model')) { return 'unknown_provider' }
    if ($status -ge 200 -and $status -lt 300) { return 'ok' }
    if ($lower.Contains('quota') -or $lower.Contains('rate') -or $status -eq 429 -or $status -eq 402) { return 'quota_or_rate' }
    if ($status -eq 401 -or $lower.Contains('auth')) { return 'auth' }
    if ($status -eq 0) { return 'timeout_or_connect' }
    return 'other_error'
}

$key = Get-LocalKey
$headers = @{ Authorization = "Bearer $key"; 'Content-Type' = 'application/json' }
$base = 'http://127.0.0.1:28080'
$models = Invoke-RestMethod -Uri "$base/v1/models" -Headers $headers -TimeoutSec 20
$ids = @($models.data | ForEach-Object { $_.id } | Where-Object { $_ } | Sort-Object -Unique)
$results = @()

foreach ($id in $ids) {
    $started = Get-Date
    $status = 0
    $body = ''
    $path = '/v1/chat/completions'
    $payload = @{
        model = $id
        messages = @(@{ role = 'user'; content = 'Reply with the single word ok.' })
        max_tokens = 16
        stream = $false
    } | ConvertTo-Json -Compress
    function Invoke-Probe([string]$url, [string]$json) {
        try {
            $resp = Invoke-WebRequest -Uri $url -Headers $headers -Method POST -Body $json -UseBasicParsing -TimeoutSec 25
            return @{ Status = [int]$resp.StatusCode; Body = [string]$resp.Content }
        } catch {
            $status = 0
            $text = ''
            if ($null -ne $_.ErrorDetails -and $_.ErrorDetails.PSObject.Properties['Message']) {
                $text = [string]$_.ErrorDetails.Message
            }
            if ([string]::IsNullOrWhiteSpace($text)) { $text = [string]$_.Exception.Message }
            if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
                $status = [int]$_.Exception.Response.StatusCode
            }
            return @{ Status = $status; Body = $text }
        }
    }
    $first = Invoke-Probe "$base$path" $payload
    $status = [int]$first.Status
    $body = [string]$first.Body
    $lower = $body.ToLowerInvariant()
    if ($lower -notmatch 'unknown provider' -and ($status -eq 0 -or ($status -ge 400 -and $status -lt 500))) {
        $path = '/v1/responses'
        $payload2 = (@{
            model = $id
            input = 'Reply with the single word ok.'
            max_output_tokens = 16
        } | ConvertTo-Json -Compress)
        $second = Invoke-Probe "$base$path" $payload2
        $status = [int]$second.Status
        $body = [string]$second.Body
    }
    $ms = [int]((Get-Date) - $started).TotalMilliseconds
    $class = Classify-Result $status $body
    $row = [pscustomobject]@{
        model = $id
        http = $status
        ms = $ms
        class = $class
        path = $path
        detail = (Summarize-Body $body)
    }
    $results += $row
    Write-Host ("{0,-36} http={1,-3} {2,-18} {3}ms {4}" -f $id, $status, $class, $ms, $row.detail)
}

$unknown = @($results | Where-Object { $_.class -eq 'unknown_provider' })
$ok = @($results | Where-Object { $_.class -eq 'ok' })
Write-Host ("SUMMARY total={0} ok={1} unknown_provider={2} other={3}" -f $results.Count, $ok.Count, $unknown.Count, ($results.Count - $ok.Count - $unknown.Count))
if ($unknown.Count -gt 0) {
    Write-Host 'UNKNOWN_PROVIDER_MODELS:'
    $unknown | ForEach-Object { Write-Host (" - {0}" -f $_.model) }
    exit 2
}
exit 0
