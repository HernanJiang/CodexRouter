param(
    [string]$RouterRoot,
    [switch]$LiveModels,
    [string[]]$OnlyModel = @(),
    [switch]$SseOnly,
    [ValidateRange(15, 900)][int]$RequestTimeoutSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Net.Http

if ([string]::IsNullOrWhiteSpace($RouterRoot)) {
    $RouterRoot = Split-Path -Parent $PSScriptRoot
}
$RouterRoot = [IO.Path]::GetFullPath($RouterRoot)
$credentialModule = Join-Path $RouterRoot 'scripts\CredentialStore.psm1'
Import-Module $credentialModule -Force
Import-Module (Join-Path $RouterRoot 'scripts\ProxyDiscovery.psm1') -Force
Import-Module (Join-Path $RouterRoot 'scripts\UserData.psm1') -Force

$results = [Collections.Generic.List[object]]::new()
$failed = $false
$localKey = $null
$httpClient = $null

function Protect-DiagnosticText {
    param([AllowNull()][string]$Text)
    if ([string]::IsNullOrWhiteSpace($Text)) { return '' }
    $value = $Text -replace '(?i)\bBearer\s+[a-z0-9._~+/-]{8,}={0,2}', 'Bearer <redacted>'
    $value = $value -replace '(?i)(?<![a-z0-9])sk-[a-z0-9_-]{12,}', '<redacted-key>'
    $value = $value -replace '\beyJ[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\b', '<redacted-jwt>'
    $value = $value -replace '(?i)(://[^:/\s]+:)[^@/\s]+@', '$1<redacted>@'
    if ($value.Length -gt 400) { $value = $value.Substring(0, 400) }
    return ($value -replace '\s+', ' ').Trim()
}

function Add-Result {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][ValidateSet('passed', 'failed', 'skipped')][string]$Status,
        [Parameter(Mandatory)][long]$Milliseconds,
        [string]$Stage = '',
        [string]$RequestId = '',
        [string]$Detail = ''
    )
    if ($Status -eq 'failed') { $script:failed = $true }
    $results.Add([pscustomobject][ordered]@{
        name = $Name
        status = $Status
        milliseconds = $Milliseconds
        stage = $Stage
        requestId = Protect-DiagnosticText $RequestId
        detail = Protect-DiagnosticText $Detail
    })
}

function Invoke-Check {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][scriptblock]$Action)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $detail = & $Action
        $watch.Stop()
        Add-Result -Name $Name -Status passed -Milliseconds $watch.ElapsedMilliseconds -Detail ([string]$detail)
    } catch {
        $watch.Stop()
        Add-Result -Name $Name -Status failed -Milliseconds $watch.ElapsedMilliseconds -Stage 'local-check' -Detail $_.Exception.Message
    }
}

function Invoke-CriticalCheck {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][scriptblock]$Action)
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try {
        $detail = & $Action
        $watch.Stop()
        Add-Result -Name $Name -Status passed -Milliseconds $watch.ElapsedMilliseconds -Detail ([string]$detail)
    } catch {
        $watch.Stop()
        Add-Result -Name $Name -Status failed -Milliseconds $watch.ElapsedMilliseconds -Stage 'identity-gate' -Detail $_.Exception.Message
        throw
    }
}

function Get-RequestId {
    param([Parameter(Mandatory)][Net.Http.HttpResponseMessage]$Response)
    foreach ($name in @('x-request-id', 'request-id', 'x-correlation-id', 'trace-id')) {
        $values = $null
        if ($Response.Headers.TryGetValues($name, [ref]$values)) {
            return [string](@($values) | Select-Object -First 1)
        }
    }
    return ''
}

function New-RequestMessage {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][hashtable]$Body
    )
    $json = $Body | ConvertTo-Json -Depth 30 -Compress
    $message = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Post, $Path)
    $message.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $localKey)
    $message.Headers.Add('X-Client-Request-Id', [Guid]::NewGuid().ToString())
    $message.Content = [Net.Http.StringContent]::new($json, [Text.Encoding]::UTF8, 'application/json')
    return $message
}

function Invoke-JsonModelRequest {
    param([Parameter(Mandatory)][string]$Model, [string]$Path = '/v1/responses')
    $body = if ($Path -eq '/v1/chat/completions') {
        @{ model = $Model; messages = @(@{ role = 'user'; content = 'Reply exactly OK.' }); max_tokens = 128; stream = $false }
    } else {
        @{ model = $Model; input = 'Reply exactly OK.'; max_output_tokens = 128; stream = $false }
    }
    $message = New-RequestMessage -Path $Path -Body $body
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $response = $null
    try {
        $response = $httpClient.SendAsync($message).GetAwaiter().GetResult()
        $requestId = Get-RequestId $response
        $content = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            throw "HTTP $([int]$response.StatusCode)"
        }
        if ([Text.Encoding]::UTF8.GetByteCount($content) -gt 4MB) { throw 'Response exceeded the 4 MiB acceptance limit.' }
        $parsed = $content | ConvertFrom-Json
        if ($null -eq $parsed) { throw 'Response body was not JSON.' }
        $watch.Stop()
        return [pscustomobject]@{ Milliseconds = $watch.ElapsedMilliseconds; RequestId = $requestId }
    } finally {
        if ($null -ne $response) { $response.Dispose() }
        $message.Dispose()
    }
}

function Invoke-StreamModelRequest {
    param([Parameter(Mandatory)][string]$Model)
    $message = New-RequestMessage -Path '/v1/responses' -Body @{
        model = $Model
        input = 'Reply exactly OK.'
        max_output_tokens = 128
        stream = $true
    }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $response = $null
    $reader = $null
    try {
        $response = $httpClient.SendAsync(
            $message,
            [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $requestId = Get-RequestId $response
        if (-not $response.IsSuccessStatusCode) { throw "HTTP $([int]$response.StatusCode) before SSE body" }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true, 4096, $false)
        $dataEvents = 0
        $complete = $false
        $eventName = ''
        $readTask = $null
        while ($true) {
            if ($watch.Elapsed.TotalSeconds -gt $RequestTimeoutSeconds) { throw 'SSE total timeout expired.' }
            if ($null -eq $readTask) { $readTask = $reader.ReadLineAsync() }
            if (-not $readTask.Wait(1000)) { continue }
            $line = $readTask.Result
            $readTask = $null
            if ($null -eq $line) { break }
            if ($line.StartsWith('event:', [StringComparison]::OrdinalIgnoreCase)) {
                $eventName = $line.Substring(6).Trim()
                if ($eventName -match '(?i)(?:^|\.)(?:error|failed)$') {
                    throw "SSE terminal error event: $eventName"
                }
                if ($eventName -eq 'response.completed') {
                    $complete = $true
                    break
                }
                continue
            }
            if ($line.StartsWith('data:', [StringComparison]::OrdinalIgnoreCase)) {
                $dataEvents++
                $payload = $line.Substring(5).Trim()
                if ($payload -eq '[DONE]') {
                    $complete = $true
                    break
                }
                if ($eventName -match '(?i)(?:^|\.)(?:error|failed)$') {
                    throw "SSE terminal error event: $eventName"
                }
                if ($payload.StartsWith('{')) {
                    try {
                        $eventObject = $payload | ConvertFrom-Json
                        $typeProperty = $eventObject.PSObject.Properties['type']
                        $eventType = if ($null -eq $typeProperty) { '' } else { [string]$typeProperty.Value }
                        if ($eventType -match '(?i)(?:^|\.)(?:error|failed)$') {
                            throw "SSE terminal error payload: $eventType"
                        }
                        if ($eventType -eq 'response.completed') {
                            $complete = $true
                            break
                        }
                    } catch {
                        if ($_.Exception.Message -like 'SSE terminal error payload:*') { throw }
                    }
                }
                $eventName = ''
            }
        }
        if ($dataEvents -eq 0) { throw 'SSE stream contained no data events.' }
        if (-not $complete) { throw 'SSE stream ended without an explicit completion event.' }
        $watch.Stop()
        return [pscustomobject]@{ Milliseconds = $watch.ElapsedMilliseconds; RequestId = $requestId; Events = $dataEvents }
    } finally {
        if ($null -ne $reader) { $reader.Dispose() }
        if ($null -ne $response) { $response.Dispose() }
        $message.Dispose()
    }
}

try {
    $configPath = Get-RouterConfigPath -RouterRoot $RouterRoot
    if (-not (Test-Path -LiteralPath $configPath)) { throw "Router configuration is missing: $configPath" }
    $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
    $baseUriText = if ($config.deploy.sub2apiHost) { [string]$config.deploy.sub2apiHost } else { 'http://127.0.0.1:18080' }
    $baseUri = [Uri]$baseUriText.TrimEnd('/')
    if ($baseUri.Scheme -ne 'http' -or
        $baseUri.Host -notin @('127.0.0.1', 'localhost') -or
        $baseUri.AbsolutePath -notin @('', '/') -or
        -not [string]::IsNullOrEmpty($baseUri.Query) -or
        -not [string]::IsNullOrEmpty($baseUri.Fragment) -or
        -not [string]::IsNullOrEmpty($baseUri.UserInfo)) {
        throw 'Router base URI is not a local HTTP endpoint.'
    }
    $baseUriBuilder = [UriBuilder]::new($baseUri)
    $baseUriBuilder.Host = '127.0.0.1'
    $baseUriBuilder.Path = ''
    $baseUri = $baseUriBuilder.Uri

    Invoke-CriticalCheck -Name 'processes-and-listeners' -Action {
        $expected = @(
            @{ Name = 'Sub2API'; Port = $baseUri.Port; Path = (Join-Path $RouterRoot 'app\sub2api.exe') },
            @{ Name = 'PostgreSQL'; Port = 15432; Path = (Join-Path $RouterRoot 'postgres\pgsql\bin\postgres.exe') },
            @{ Name = 'Redis'; Port = 16379; Path = (Join-Path $RouterRoot 'redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe') }
        )
        foreach ($service in $expected) {
            $listener = @(Get-NetTCPConnection -LocalPort $service.Port -State Listen -ErrorAction Stop)
            if ($listener.Count -ne 1 -or $listener[0].LocalAddress -ne '127.0.0.1') {
                throw "$($service.Name) is not bound exclusively to 127.0.0.1:$($service.Port)."
            }
            $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($listener[0].OwningProcess)" -ErrorAction Stop
            if ($null -eq $process -or
                [string]::IsNullOrWhiteSpace([string]$process.ExecutablePath) -or
                -not [IO.Path]::GetFullPath([string]$process.ExecutablePath).Equals(
                [IO.Path]::GetFullPath($service.Path),
                [StringComparison]::OrdinalIgnoreCase)) {
                throw "$($service.Name) listener belongs to an unexpected executable."
            }
        }
        '3 verified loopback services'
    }

    $localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
    Invoke-CriticalCheck -Name 'local-key-visibility' -Action {
        if ([string]::IsNullOrWhiteSpace($localKey)) { throw 'LocalApiKey is missing from Windows Credential Manager.' }
        'present in Credential Manager; value not printed'
    }

    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.UseProxy = $false
    $httpClient = [Net.Http.HttpClient]::new($handler)
    $httpClient.BaseAddress = $baseUri
    $httpClient.Timeout = [TimeSpan]::FromSeconds($RequestTimeoutSeconds)
    $httpClient.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $localKey)

    Invoke-Check -Name 'sub2api-health-and-auth' -Action {
        $health = $httpClient.GetAsync('/health').GetAwaiter().GetResult()
        try { if (-not $health.IsSuccessStatusCode) { throw "health HTTP $([int]$health.StatusCode)" } } finally { $health.Dispose() }
        $modelsResponse = $httpClient.GetAsync('/v1/models').GetAwaiter().GetResult()
        try { if (-not $modelsResponse.IsSuccessStatusCode) { throw "models HTTP $([int]$modelsResponse.StatusCode)" } } finally { $modelsResponse.Dispose() }
        'health and protected models endpoint returned 2xx'
    }

    Invoke-Check -Name 'codex-provider' -Action {
        $codexHome = if ($config.deploy.codexHome) { [string]$config.deploy.codexHome } elseif ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex' }
        $text = [IO.File]::ReadAllText((Join-Path $codexHome 'config.toml'))
        if ($text -notmatch '(?m)^model_provider\s*=\s*"codex_router"\s*$' -or
            $text -notmatch [regex]::Escape($baseUri.GetLeftPart([UriPartial]::Authority) + '/v1')) {
            throw 'Codex does not point to this local Router.'
        }
        'local provider and Router base URL verified'
    }

    Invoke-Check -Name 'configured-fallback-pairs' -Action {
        $pairs = @($config.models | Group-Object model | Where-Object {
            @($_.Group | Where-Object source -eq 'oauth').Count -gt 0 -and
            @($_.Group | Where-Object source -eq 'apikey').Count -gt 0
        })
        $selectedAccountIds = @($config.oauthAccountIds | Where-Object { [long]$_ -gt 0 })
        $apiModels = @($config.models | Where-Object source -eq 'apikey')
        if ($config.oauthFallback.enabled -and $pairs.Count -eq 0 -and
            ($selectedAccountIds.Count -eq 0 -or $apiModels.Count -eq 0)) {
            throw 'OAuth fallback is enabled without both a selected OAuth account and an API-key channel.'
        }
        "explicit pairs=$($pairs.Count); selected OAuth accounts=$($selectedAccountIds.Count); API channels=$($apiModels.Count)"
    }

    Invoke-Check -Name 'configured-proxy-path' -Action {
        $proxyProperty = $config.PSObject.Properties['proxy']
        $proxyConfig = if ($null -eq $proxyProperty) { $null } else { $proxyProperty.Value }
        $proxySettings = Resolve-RouterProxySettings `
            -ProxyConfig $proxyConfig `
            -ProxyPassword $null
        if ($null -eq $proxySettings.ProxyUrl) { return 'direct mode' }
        $proxyUri = [Uri]$proxySettings.ProxyUrl
        $client = [Net.Sockets.TcpClient]::new()
        try {
            $connect = $client.ConnectAsync($proxyUri.Host, $proxyUri.Port)
            if (-not $connect.Wait(3000) -or -not $client.Connected) { throw 'Configured proxy does not accept TCP connections.' }
        } finally { $client.Dispose() }
        "$($proxySettings.Source) proxy reachable; endpoint and credentials not printed"
    }

    $publicModelsResponse = $httpClient.GetAsync('/v1/models').GetAwaiter().GetResult()
    try {
        if (-not $publicModelsResponse.IsSuccessStatusCode) {
            throw "models HTTP $([int]$publicModelsResponse.StatusCode)"
        }
        $publicModelsPayload = $publicModelsResponse.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json
        $configuredModels = @($publicModelsPayload.data | ForEach-Object { [string]$_.id } | Where-Object { $_ } | Select-Object -Unique)
    } finally {
        $publicModelsResponse.Dispose()
    }
    $models = if (@($OnlyModel).Count -gt 0) {
        @($OnlyModel | Where-Object { $configuredModels -contains $_ } | Select-Object -Unique)
    } else {
        $configuredModels
    }
    if (@($OnlyModel).Count -gt 0 -and @($models).Count -ne @($OnlyModel | Select-Object -Unique).Count) {
        throw 'At least one requested live-test model is not configured.'
    }
    if ($LiveModels) {
        foreach ($model in $models) {
            if (-not $SseOnly) {
                try {
                    $responseResult = Invoke-JsonModelRequest -Model $model
                    Add-Result -Name "responses-json:$model" -Status passed -Milliseconds $responseResult.Milliseconds -Stage 'complete' -RequestId $responseResult.RequestId
                } catch {
                    Add-Result -Name "responses-json:$model" -Status failed -Milliseconds 0 -Stage 'response' -Detail $_.Exception.Message
                }
            }
            try {
                $streamResult = Invoke-StreamModelRequest -Model $model
                Add-Result -Name "responses-sse:$model" -Status passed -Milliseconds $streamResult.Milliseconds -Stage 'stream-complete' -RequestId $streamResult.RequestId -Detail "events=$($streamResult.Events)"
            } catch {
                Add-Result -Name "responses-sse:$model" -Status failed -Milliseconds 0 -Stage 'stream' -Detail $_.Exception.Message
            }
        }
        if (-not $SseOnly) {
            $defaultModel = if ($config.defaultModel) { [string]$config.defaultModel } else { [string]$models[0] }
            try {
                $chatResult = Invoke-JsonModelRequest -Model $defaultModel -Path '/v1/chat/completions'
                Add-Result -Name "chat-completions:$defaultModel" -Status passed -Milliseconds $chatResult.Milliseconds -Stage 'complete' -RequestId $chatResult.RequestId
            } catch {
                Add-Result -Name "chat-completions:$defaultModel" -Status failed -Milliseconds 0 -Stage 'chat-completions' -Detail $_.Exception.Message
            }
        }
    } else {
        Add-Result -Name 'live-model-protocols' -Status skipped -Milliseconds 0 -Stage 'not-requested' -Detail 'Run with -LiveModels to send minimal paid/nonlocal requests.'
    }

    [ordered]@{
        schemaVersion = 1
        routerRoot = Split-Path -Leaf $RouterRoot
        liveModels = [bool]$LiveModels
        passed = @($results | Where-Object status -eq 'passed').Count
        failed = @($results | Where-Object status -eq 'failed').Count
        skipped = @($results | Where-Object status -eq 'skipped').Count
        results = @($results)
    } | ConvertTo-Json -Depth 6
    if ($failed) { throw 'Router capability acceptance failed; see the redacted JSON above.' }
} finally {
    if ($null -ne $httpClient) { $httpClient.Dispose() }
    $localKey = $null
}
