param(
    [string]$Model = 'gpt-5.6-sol',
    [long]$OAuthAccountId = 0,
    [switch]$ExerciseOAuth429
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force

if ($Model -notmatch '^[A-Za-z0-9._:/-]{1,160}$') {
    throw 'Model contains unsupported characters.'
}

function Invoke-RouterSseRequest {
    param(
        [Parameter(Mandatory)][string]$BaseUri,
        [Parameter(Mandatory)][string]$ApiKey,
        [Parameter(Mandatory)]$Payload,
        [ValidateRange(1, 300)][int]$TimeoutSeconds = 90
    )

    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.UseProxy = $false
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds($TimeoutSeconds)
    $request = [Net.Http.HttpRequestMessage]::new(
        [Net.Http.HttpMethod]::Post,
        "$($BaseUri.TrimEnd('/'))/v1/responses")
    $request.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $ApiKey)
    [void]$request.Headers.TryAddWithoutValidation('x-request-id', [Guid]::NewGuid().ToString())
    $json = $Payload | ConvertTo-Json -Depth 100 -Compress
    $request.Content = [Net.Http.StringContent]::new(
        $json,
        [Text.UTF8Encoding]::new($false),
        'application/json')

    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    $response = $null
    $stream = $null
    $reader = $null
    try {
        $response = $client.SendAsync(
            $request,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            $errorBody = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            $errorBody = ($errorBody -replace '(?i)Bearer\s+[A-Za-z0-9._~+/-]{8,}={0,2}', '<redacted>')
            $errorBody = ($errorBody -replace '\s+', ' ').Trim()
            if ($errorBody.Length -gt 600) { $errorBody = $errorBody.Substring(0, 600) }
            throw "Router SSE request failed ($([int]$response.StatusCode)): $errorBody"
        }

        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false))
        $firstEventMilliseconds = 0L
        $events = [Collections.Generic.List[string]]::new()
        $text = [Text.StringBuilder]::new()
        $toolCall = $null
        $sawCompleted = $false
        $sawDone = $false

        while (-not $reader.EndOfStream) {
            $line = $reader.ReadLine()
            if (-not $line.StartsWith('data:')) { continue }
            $data = $line.Substring(5).Trim()
            if ($firstEventMilliseconds -eq 0) {
                $firstEventMilliseconds = [Math]::Max(1L, $stopwatch.ElapsedMilliseconds)
            }
            if ($data -eq '[DONE]') {
                $sawDone = $true
                break
            }
            try { $event = $data | ConvertFrom-Json } catch { continue }
            $eventType = [string]$event.type
            if ($eventType -and -not $events.Contains($eventType)) { $events.Add($eventType) }
            if ($eventType -eq 'response.output_text.delta') {
                [void]$text.Append([string]$event.delta)
            }
            if ($eventType -eq 'response.completed') { $sawCompleted = $true }
            $itemProperty = $event.PSObject.Properties['item']
            if ($null -ne $itemProperty -and [string]$itemProperty.Value.type -eq 'function_call') {
                $toolCall = $itemProperty.Value
            }
        }

        return [pscustomobject][ordered]@{
            StatusCode = [int]$response.StatusCode
            FirstEventMilliseconds = $firstEventMilliseconds
            TotalMilliseconds = $stopwatch.ElapsedMilliseconds
            SawCompleted = $sawCompleted
            SawDone = $sawDone
            Events = @($events)
            Text = $text.ToString()
            ToolCall = $toolCall
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

function Invoke-PostgresRows {
    param([Parameter(Mandatory)][string]$Query)
    $password = Get-RouterCredential -Name 'PostgresPassword'
    $previousPassword = [Environment]::GetEnvironmentVariable('PGPASSWORD', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $password, 'Process')
        $rows = @(& (Join-Path $routerRoot 'postgres\pgsql\bin\psql.exe') `
            -X -w -h 127.0.0.1 -p 15432 -U sub2api -d sub2api `
            -v ON_ERROR_STOP=1 -tA -F '|' -c $Query)
        if ($LASTEXITCODE -ne 0) { throw "PostgreSQL verification failed with exit code $LASTEXITCODE." }
        return @($rows | Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_) })
    } finally {
        [Environment]::SetEnvironmentVariable('PGPASSWORD', $previousPassword, 'Process')
        $password = $null
    }
}

$session = New-RouterAdminSession
$localApiKey = Get-RouterCredential -Name 'LocalApiKey'
$testStartedAt = [DateTimeOffset]::UtcNow
try {
    $accounts = @(Get-RouterAccounts -Session $session)
    if ($OAuthAccountId -le 0) {
        $configured = Get-Content -LiteralPath (Get-RouterConfigPath -RouterRoot $routerRoot) -Raw | ConvertFrom-Json
        $OAuthAccountId = @($configured.oauthAccountIds | ForEach-Object { [long]$_ }) | Select-Object -First 1
    }
    if ($OAuthAccountId -le 0) { throw 'No selected OAuth account is available for the fallback test.' }

    $oauth = Get-RouterResponseData (Invoke-RouterApi `
        -Session $session -Method GET -Path "/api/v1/admin/accounts/$OAuthAccountId")
    if ([string]$oauth.type -ne 'oauth') { throw "Account $OAuthAccountId is not an OAuth account." }

    $fallbackAccounts = @($accounts | Where-Object {
        [long]$_.id -ne $OAuthAccountId -and
        [string]$_.type -eq 'apikey' -and
        [string]$_.name -like 'Codex-Router / *'
    })
    if ($fallbackAccounts.Count -eq 0) { throw 'No managed API fallback account is configured.' }

    if ($ExerciseOAuth429) {
        [void](Invoke-RouterApi `
            -Session $session `
            -Method POST `
            -Path "/api/v1/admin/accounts/$OAuthAccountId/recover-state" `
            -Body @{})
    }

    $tool = [ordered]@{
        type = 'function'
        name = 'lookup_test_value'
        description = 'Return the deterministic test value for a key.'
        parameters = [ordered]@{
            type = 'object'
            properties = [ordered]@{ key = [ordered]@{ type = 'string' } }
            required = @('key')
            additionalProperties = $false
        }
    }
    $testRunId = [Guid]::NewGuid().ToString('N')
    $prompt = "Call lookup_test_value with key alpha. After its result arrives, reply exactly TOOL_OK. Test run: $testRunId"
    $first = Invoke-RouterSseRequest `
        -BaseUri (Get-RouterBaseUri) `
        -ApiKey $localApiKey `
        -Payload ([ordered]@{
            model = $Model
            input = $prompt
            tools = @($tool)
            tool_choice = [ordered]@{ type = 'function'; name = 'lookup_test_value' }
            parallel_tool_calls = $true
            reasoning = [ordered]@{ effort = 'high'; summary = 'auto' }
            include = @('reasoning.encrypted_content')
            prompt_cache_key = $testRunId
            store = $false
            stream = $true
            max_output_tokens = 128
        })
    # Native Responses streams terminate after response.completed and may omit
    # the Chat Completions-style data: [DONE] sentinel.
    if (-not $first.SawCompleted) {
        throw 'The fallback tool-call stream did not complete cleanly.'
    }
    if ($null -eq $first.ToolCall -or [string]$first.ToolCall.name -ne 'lookup_test_value') {
        throw 'The fallback stream did not return the expected function call.'
    }
    $callId = [string]$first.ToolCall.call_id
    if ([string]::IsNullOrWhiteSpace($callId)) { $callId = [string]$first.ToolCall.id }
    if ([string]::IsNullOrWhiteSpace($callId)) { throw 'The fallback tool call did not include a call ID.' }

    $second = Invoke-RouterSseRequest `
        -BaseUri (Get-RouterBaseUri) `
        -ApiKey $localApiKey `
        -Payload ([ordered]@{
            model = $Model
            input = @(
                [ordered]@{ role = 'user'; content = $prompt },
                [ordered]@{
                    type = 'function_call'
                    call_id = $callId
                    name = 'lookup_test_value'
                    arguments = [string]$first.ToolCall.arguments
                },
                [ordered]@{
                    type = 'function_call_output'
                    call_id = $callId
                    output = '{"value":"TOOL_OK"}'
                }
            )
            tools = @($tool)
            tool_choice = 'none'
            reasoning = [ordered]@{ effort = 'high'; summary = 'auto' }
            prompt_cache_key = $testRunId
            store = $false
            stream = $true
            max_output_tokens = 128
        })
    if (-not $second.SawCompleted -or $second.Text -notmatch '(?i)TOOL_OK') {
        throw 'The fallback tool-result round trip did not return TOOL_OK in a completed stream.'
    }

    $refreshedOAuth = Get-RouterResponseData (Invoke-RouterApi `
        -Session $session -Method GET -Path "/api/v1/admin/accounts/$OAuthAccountId")
    $resetAt = ConvertTo-RouterResetAtUtc -Value $refreshedOAuth.rate_limit_reset_at
    if ($ExerciseOAuth429 -and ($null -eq $resetAt -or $resetAt -le [DateTimeOffset]::UtcNow)) {
        throw 'The real OAuth 429 did not persist a future recovery time.'
    }

    $startedSql = $testStartedAt.UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss.ffffff+00')
    $usageRows = @(Invoke-PostgresRows -Query @"
SELECT account_id || '|' || COALESCE(upstream_endpoint, '') || '|' || COALESCE(first_token_ms, 0) || '|' || duration_ms
FROM usage_logs
WHERE created_at >= TIMESTAMPTZ '$startedSql'
  AND model = '$Model'
ORDER BY id;
"@)
    if ($usageRows.Count -lt 2) { throw 'The two fallback requests were not recorded in usage logs.' }
    $fallbackIds = @($fallbackAccounts | ForEach-Object { [long]$_.id })
    foreach ($row in $usageRows[-2..-1]) {
        $fields = $row.Split('|')
        if ([long]$fields[0] -notin $fallbackIds) {
            throw "A fallback request used unexpected account $($fields[0])."
        }
        if ($fields[1] -ne '/v1/responses') {
            throw "A Chiral fallback request did not use native Responses: $($fields[1])"
        }
    }

    if ($ExerciseOAuth429) {
        $failoverRows = @(Invoke-PostgresRows -Query @"
SELECT account_id || '|' || COALESCE(extra->>'upstream_status', '')
FROM ops_system_logs
WHERE created_at >= TIMESTAMPTZ '$startedSql'
  AND message = 'openai.upstream_failover_switching'
  AND account_id = $OAuthAccountId
  AND extra->>'upstream_status' = '429'
ORDER BY id;
"@)
        if ($failoverRows.Count -eq 0) { throw 'The real request did not record OAuth 429 before fallback.' }
    }

    [pscustomobject][ordered]@{
        Passed = $true
        Model = $Model
        OAuthAccountId = $OAuthAccountId
        OAuth429Observed = [bool]$ExerciseOAuth429
        RecoveryAtUtc = if ($null -eq $resetAt) { '' } else { $resetAt.UtcDateTime.ToString('o') }
        FallbackAccountIds = @($usageRows[-2..-1] | ForEach-Object { [long]$_.Split('|')[0] })
        UpstreamEndpoint = '/v1/responses'
        FirstToolEventMilliseconds = $first.FirstEventMilliseconds
        FinalTextEventMilliseconds = $second.FirstEventMilliseconds
        ToolRoundTrip = $true
        CompletedSse = $true
        DoneSentinelOptional = $true
    } | ConvertTo-Json -Depth 6
} finally {
    $localApiKey = $null
    if ($null -ne $session -and $null -ne $session.Headers) { $session.Headers.Clear() }
}
