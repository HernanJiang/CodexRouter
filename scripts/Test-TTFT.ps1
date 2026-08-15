param(
    [string]$RouterRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$BaseUri = 'http://127.0.0.1:18080',
    [string[]]$Models = @('gpt-5.6-sol'),
    [ValidateRange(1, 20)][int]$SamplesPerModel = 1,
    [ValidateRange(64, 4096)][int]$MaxOutputTokens = 64,
    [ValidateSet('none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra')]
    [string]$ReasoningEffort = 'medium',
    [ValidateRange(1, 300)][int]$TimeoutSeconds = 120,
    [ValidateRange(1, 65535)][int]$PostgresPort = 15432,
    [switch]$AllowFailures
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRootPath = [IO.Path]::GetFullPath($RouterRoot)
$credentialModule = Join-Path $PSScriptRoot 'CredentialStore.psm1'
$psql = Join-Path (Join-Path (Join-Path (Join-Path $routerRootPath 'postgres') 'pgsql') 'bin') 'psql.exe'
if (-not (Test-Path -LiteralPath $credentialModule -PathType Leaf)) {
    throw "Credential module is missing: $credentialModule"
}
if (-not (Test-Path -LiteralPath $psql -PathType Leaf)) {
    throw "PostgreSQL client is missing: $psql"
}
Import-Module $credentialModule -Force

$routerUri = $null
if (-not [Uri]::TryCreate($BaseUri.TrimEnd('/'), [UriKind]::Absolute, [ref]$routerUri) -or
    $routerUri.Scheme -ne 'http' -or
    $routerUri.Host -notin @('127.0.0.1', 'localhost') -or
    -not [string]::IsNullOrEmpty($routerUri.Query) -or
    -not [string]::IsNullOrEmpty($routerUri.Fragment)) {
    throw 'BaseUri must be a loopback HTTP URL.'
}

foreach ($model in $Models) {
    if ($model -notmatch '^[A-Za-z0-9._:/~-]{1,160}$') {
        throw "Model contains unsupported characters: $model"
    }
}

function ConvertTo-SqlLiteral {
    param([Parameter(Mandatory)][string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}

function Invoke-PostgresRows {
    param([Parameter(Mandatory)][string]$Query)

    $password = Get-RouterCredential -Name 'PostgresPassword'
    $previousPassword = [Environment]::GetEnvironmentVariable('PGPASSWORD', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $password, 'Process')
        $arguments = @(
            '-X', '-w', '-h', '127.0.0.1', '-p', $PostgresPort,
            '-U', 'sub2api', '-d', 'sub2api', '-v', 'ON_ERROR_STOP=1',
            '-tA', '-F', '|', '-c', $Query
        )
        $rows = @(& $psql @arguments)
        if ($LASTEXITCODE -ne 0) {
            throw "PostgreSQL query failed with exit code $LASTEXITCODE."
        }
        return @($rows | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_) })
    } finally {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $previousPassword, 'Process')
        $password = $null
    }
}

function Get-UsageMeasurement {
    param(
        [Parameter(Mandatory)][string]$UserAgent,
        [ValidateRange(1, 30)][int]$WaitSeconds = 10
    )

    $userAgentLiteral = ConvertTo-SqlLiteral -Value $UserAgent
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($WaitSeconds)
    do {
        $rows = @(Invoke-PostgresRows -Query @"
SELECT COALESCE(account_id, 0),
       COALESCE(requested_model, ''),
       COALESCE(upstream_model, ''),
       COALESCE(inbound_endpoint, ''),
       COALESCE(upstream_endpoint, ''),
       COALESCE(first_token_ms, -1),
       COALESCE(duration_ms, -1)
FROM usage_logs
WHERE user_agent = $userAgentLiteral
ORDER BY id DESC
LIMIT 2;
"@)
        if ($rows.Count -eq 1) {
            $fields = $rows[0].Split('|')
            if ($fields.Count -ne 7) {
                throw 'The TTFT usage row has an unexpected shape.'
            }
            return [pscustomobject][ordered]@{
                AccountId = [long]$fields[0]
                RequestedModel = $fields[1]
                UpstreamModel = $fields[2]
                InboundEndpoint = $fields[3]
                UpstreamEndpoint = $fields[4]
                FirstTokenMilliseconds = [long]$fields[5]
                DurationMilliseconds = [long]$fields[6]
            }
        }
        if ($rows.Count -gt 1) {
            throw 'The TTFT request marker matched more than one usage row.'
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTimeOffset]::UtcNow -lt $deadline)

    throw "No usage row was recorded for TTFT marker $UserAgent."
}

function Test-SemanticOutputEvent {
    param([AllowEmptyString()][string]$EventType)
    return $EventType -notin @('', 'response.created', 'response.in_progress', 'response.failed')
}

function Invoke-TTFTSample {
    param(
        [Parameter(Mandatory)][string]$Model,
        [Parameter(Mandatory)][int]$Sample,
        [Parameter(Mandatory)][string]$ApiKey
    )

    $marker = 'CodexRouter-TTFT/1.0 ttft-' + [Guid]::NewGuid().ToString('N')
    $requestUri = "$($routerUri.GetLeftPart([UriPartial]::Authority).TrimEnd('/'))/v1/responses"
    $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Post, $requestUri)
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.UseProxy = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
    $response = $null
    $stream = $null
    $reader = $null
    $json = $null
    try {
        $request.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $ApiKey)
        [void]$request.Headers.TryAddWithoutValidation('User-Agent', $marker)
        $payload = [ordered]@{
            model = $Model
            input = 'Reply exactly OK.'
            reasoning = [ordered]@{ effort = $ReasoningEffort; summary = 'auto' }
            store = $false
            stream = $true
            max_output_tokens = $MaxOutputTokens
        }
        $json = $payload | ConvertTo-Json -Depth 20 -Compress
        $request.Content = [Net.Http.StringContent]::new(
            $json,
            [Text.UTF8Encoding]::new($false),
            'application/json')

        $stopwatch = [Diagnostics.Stopwatch]::StartNew()
        $response = $client.SendAsync(
            $request,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $headersMilliseconds = [Math]::Max(1L, $stopwatch.ElapsedMilliseconds)
        if (-not $response.IsSuccessStatusCode) {
            $body = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            $body = ($body -replace '(?i)Bearer\s+[A-Za-z0-9._~+/-]{8,}={0,2}', '<redacted>')
            $body = ($body -replace '\s+', ' ').Trim()
            if ($body.Length -gt 600) { $body = $body.Substring(0, 600) }
            throw "TTFT request failed ($([int]$response.StatusCode)): $body"
        }

        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false))
        $firstDataMilliseconds = 0L
        $firstSemanticMilliseconds = 0L
        $firstTextDeltaMilliseconds = 0L
        $firstSemanticEvent = ''
        $lastEventType = ''
        $sawCompleted = $false
        while (-not $reader.EndOfStream) {
            $line = $reader.ReadLine()
            if (-not $line.StartsWith('data:')) { continue }
            $data = $line.Substring(5).Trim()
            if ($firstDataMilliseconds -eq 0) {
                $firstDataMilliseconds = [Math]::Max(1L, $stopwatch.ElapsedMilliseconds)
            }
            if ($data -eq '[DONE]') { break }
            try { $event = $data | ConvertFrom-Json } catch { continue }
            $eventType = [string]$event.type
            $lastEventType = $eventType
            if ($firstSemanticMilliseconds -eq 0 -and (Test-SemanticOutputEvent -EventType $eventType)) {
                $firstSemanticMilliseconds = [Math]::Max(1L, $stopwatch.ElapsedMilliseconds)
                $firstSemanticEvent = $eventType
            }
            if ($firstTextDeltaMilliseconds -eq 0 -and $eventType -eq 'response.output_text.delta') {
                $firstTextDeltaMilliseconds = [Math]::Max(1L, $stopwatch.ElapsedMilliseconds)
            }
            if ($eventType -eq 'response.completed') { $sawCompleted = $true }
        }
        $totalMilliseconds = $stopwatch.ElapsedMilliseconds
        if (-not $sawCompleted) {
            if ($lastEventType -eq 'response.failed') {
                throw 'Upstream returned response.failed before response.completed.'
            }
            throw 'TTFT stream did not produce response.completed.'
        }
        if ($firstSemanticMilliseconds -le 0) { throw 'TTFT stream did not produce a semantic output event.' }

        $usage = Get-UsageMeasurement -UserAgent $marker
        if ($usage.FirstTokenMilliseconds -lt 0) {
            throw 'The usage row did not record first_token_ms.'
        }
        if ($usage.RequestedModel -ne $Model) {
            throw "Usage row requested model '$($usage.RequestedModel)' does not match '$Model'."
        }

        return [pscustomobject][ordered]@{
            Model = $Model
            Sample = $Sample
            Passed = $true
            StatusCode = [int]$response.StatusCode
            HeadersMilliseconds = $headersMilliseconds
            FirstDataMilliseconds = $firstDataMilliseconds
            FirstSemanticMilliseconds = $firstSemanticMilliseconds
            FirstTextDeltaMilliseconds = $firstTextDeltaMilliseconds
            TotalMilliseconds = $totalMilliseconds
            RouterFirstTokenMilliseconds = $usage.FirstTokenMilliseconds
            ClientAfterRouterMilliseconds = $firstSemanticMilliseconds - $usage.FirstTokenMilliseconds
            RouterDurationMilliseconds = $usage.DurationMilliseconds
            FirstSemanticEvent = $firstSemanticEvent
            AccountId = $usage.AccountId
            RequestedModel = $usage.RequestedModel
            UpstreamModel = $usage.UpstreamModel
            InboundEndpoint = $usage.InboundEndpoint
            UpstreamEndpoint = $usage.UpstreamEndpoint
            CompletedSse = $sawCompleted
        }
    } finally {
        if ($null -ne $reader) { $reader.Dispose() }
        elseif ($null -ne $stream) { $stream.Dispose() }
        if ($null -ne $response) { $response.Dispose() }
        $request.Dispose()
        $client.Dispose()
        $handler.Dispose()
        $json = $null
    }
}

$localApiKey = Get-RouterCredential -Name 'LocalApiKey'
try {
    $measurements = [Collections.Generic.List[object]]::new()
    foreach ($model in $Models) {
        for ($sample = 1; $sample -le $SamplesPerModel; $sample++) {
            try {
                $measurements.Add((Invoke-TTFTSample -Model $model -Sample $sample -ApiKey $localApiKey))
            } catch {
                $message = [string]$_.Exception.Message
                $message = ($message -replace '(?i)Bearer\s+[A-Za-z0-9._~+/-]{8,}={0,2}', '<redacted>')
                $message = ($message -replace '\s+', ' ').Trim()
                $measurements.Add([pscustomobject][ordered]@{
                    Model = $model
                    Sample = $sample
                    Passed = $false
                    Error = $message
                })
                if (-not $AllowFailures) { throw }
            }
        }
    }
    @($measurements) | ConvertTo-Json -Depth 5
} finally {
    $localApiKey = $null
}
