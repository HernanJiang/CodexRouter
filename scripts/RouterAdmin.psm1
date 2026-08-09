Set-StrictMode -Version Latest

$script:RouterRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$script:RouterRoot\scripts\CredentialStore.psm1"
Import-Module "$script:RouterRoot\scripts\UserData.psm1" -Force

function Get-RouterBaseUri {
    $configured = 'http://127.0.0.1:18080'
    $configPath = Get-RouterConfigPath -RouterRoot $script:RouterRoot
    if (Test-Path -LiteralPath $configPath) {
        $routerConfig = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
        $hostProperty = $routerConfig.deploy.PSObject.Properties['sub2apiHost']
        if ($null -ne $hostProperty -and -not [string]::IsNullOrWhiteSpace([string]$hostProperty.Value)) {
            $configured = [string]$hostProperty.Value
        }
    }
    $uri = $null
    if (-not [Uri]::TryCreate($configured.TrimEnd('/'), [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -ne 'http' -or
        $uri.Host -notin @('127.0.0.1', 'localhost') -or
        $uri.Port -lt 1 -or $uri.Port -gt 65535 -or
        -not [string]::IsNullOrEmpty($uri.Query) -or
        -not [string]::IsNullOrEmpty($uri.Fragment)) {
        throw 'Sub2API host must be a local HTTP URL such as http://127.0.0.1:18080.'
    }
    return $uri.GetLeftPart([UriPartial]::Authority).TrimEnd('/')
}

$script:BaseUri = Get-RouterBaseUri

function Invoke-LoopbackRestMethod {
    param(
        [Parameter(Mandatory)][ValidateSet('GET', 'POST', 'PUT', 'DELETE')][string]$Method,
        [Parameter(Mandatory)][Uri]$Uri,
        [Collections.IDictionary]$Headers = @{},
        [AllowNull()][byte[]]$BodyBytes,
        [ValidateRange(1, 300)][int]$TimeoutSec = 30
    )
    if ($Uri.Scheme -ne 'http' -or $Uri.Host -notin @('127.0.0.1', 'localhost')) {
        throw 'Router admin requests must target a loopback HTTP endpoint.'
    }
    $request = [Net.HttpWebRequest]::Create($Uri)
    $response = $null
    try {
        $request.Method = $Method
        $request.Proxy = $null
        $request.Timeout = $TimeoutSec * 1000
        $request.ReadWriteTimeout = $TimeoutSec * 1000
        $request.KeepAlive = $false
        $request.Accept = 'application/json'
        foreach ($entry in $Headers.GetEnumerator()) {
            $request.Headers[[string]$entry.Key] = [string]$entry.Value
        }
        if ($null -ne $BodyBytes) {
            $request.ContentType = 'application/json'
            $request.ContentLength = $BodyBytes.Length
            $requestStream = $request.GetRequestStream()
            try {
                $requestStream.Write($BodyBytes, 0, $BodyBytes.Length)
                $requestStream.Flush()
            } finally {
                $requestStream.Dispose()
            }
        }
        try {
            $response = [Net.HttpWebResponse]$request.GetResponse()
        } catch [Net.WebException] {
            $webException = [Net.WebException]$_.Exception
            if ($null -ne $webException.Response) {
                $errorResponse = [Net.HttpWebResponse]$webException.Response
                try {
                    $errorStream = $errorResponse.GetResponseStream()
                    $errorText = ''
                    if ($null -ne $errorStream) {
                        $errorReader = [IO.StreamReader]::new($errorStream, [Text.Encoding]::UTF8)
                        try { $errorText = $errorReader.ReadToEnd() } finally { $errorReader.Dispose(); $errorStream.Dispose() }
                    }
                    $statusCode = [int]$errorResponse.StatusCode
                    $detail = ($errorText -replace '\s+', ' ').Trim()
                    if ($detail.Length -gt 600) { $detail = $detail.Substring(0, 600) }
                    $suffix = if ($detail) { ": $detail" } else { '' }
                    throw "Sub2API request failed ($statusCode): $Method $($Uri.AbsolutePath)$suffix"
                } finally {
                    $errorResponse.Dispose()
                }
            }
            throw
        }
        $stream = $response.GetResponseStream()
        if ($null -eq $stream) { return $null }
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8)
        try {
            $text = $reader.ReadToEnd()
        } finally {
            $reader.Dispose()
            $stream.Dispose()
        }
        if ([string]::IsNullOrWhiteSpace($text)) { return $null }
        return $text | ConvertFrom-Json
    } finally {
        if ($null -ne $response) { $response.Dispose() }
        $request.Abort()
    }
}

function Get-RouterAdminSessionCachePath {
    $stateDir = Join-Path (Get-RouterDataRoot -RouterRoot $script:RouterRoot) 'state'
    [IO.Directory]::CreateDirectory($stateDir) | Out-Null
    return Join-Path $stateDir 'admin-session.cache.json'
}

function Read-RouterAdminSessionCache {
    $path = Get-RouterAdminSessionCachePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    try {
        $raw = Get-Content -LiteralPath $path -Raw -ErrorAction Stop
        $doc = ConvertFrom-Json -InputObject $raw
        $token = [string]$doc.accessToken
        $expiresAtText = [string]$doc.expiresAtUtc
        if ([string]::IsNullOrWhiteSpace($token) -or [string]::IsNullOrWhiteSpace($expiresAtText)) {
            return $null
        }
        $expiresAt = [DateTimeOffset]::Parse($expiresAtText, [Globalization.CultureInfo]::InvariantCulture)
        # Refresh one minute early so UI loads do not race expiry.
        if ($expiresAt -le [DateTimeOffset]::UtcNow.AddMinutes(1)) { return $null }
        return [pscustomobject]@{
            BaseUri = $script:BaseUri
            Headers = @{ Authorization = "Bearer $token" }
            ExpiresAtUtc = $expiresAt
        }
    } catch {
        return $null
    }
}

function Write-RouterAdminSessionCache {
    param(
        [Parameter(Mandatory)][string]$AccessToken,
        [Parameter(Mandatory)][DateTimeOffset]$ExpiresAtUtc
    )
    $path = Get-RouterAdminSessionCachePath
    $doc = [ordered]@{
        accessToken = $AccessToken
        expiresAtUtc = $ExpiresAtUtc.UtcDateTime.ToString('o')
        baseUri = $script:BaseUri
    } | ConvertTo-Json -Compress
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($doc)
    try {
        Write-RouterFileAtomic -Path $path -Bytes $bytes
    } finally {
        [Array]::Clear($bytes, 0, $bytes.Length)
    }
}

function Test-RouterAdminSession {
    param([Parameter(Mandatory)]$Session)
    try {
        $null = Invoke-LoopbackRestMethod `
            -Method GET `
            -Uri "$($Session.BaseUri)/api/v1/admin/groups/all?include_inactive=true" `
            -Headers $Session.Headers `
            -TimeoutSec 8
        return $true
    } catch {
        return $false
    }
}

function New-RouterAdminSession {
    $cached = Read-RouterAdminSessionCache
    if ($null -ne $cached -and (Test-RouterAdminSession -Session $cached)) {
        return $cached
    }

    $password = Get-RouterCredential -Name 'AdminPassword'
    try {
        # Prefer the stored admin password only. Legacy defaults are a single
        # fallback pair so failed UI loads cannot burn the login rate limit.
        $candidates = [System.Collections.Generic.List[object]]::new()
        if (-not [string]::IsNullOrWhiteSpace([string]$password)) {
            [void]$candidates.Add(@{ email = 'admin@admin.com'; password = $password })
            [void]$candidates.Add(@{ email = 'admin@sub2api.local'; password = $password })
        }
        [void]$candidates.Add(@{ email = 'admin@admin.com'; password = 'adminadmin' })

        $token = $null
        $attempted = @{}
        $rateLimited = $false
        foreach ($candidate in $candidates) {
            $candidateKey = "$($candidate.email)`0$($candidate.password)"
            if ($attempted.ContainsKey($candidateKey)) { continue }
            $attempted[$candidateKey] = $true
            try {
                $loginBody = [Text.UTF8Encoding]::new($false).GetBytes(
                    ($candidate | ConvertTo-Json -Compress)
                )
                try {
                    $login = Invoke-LoopbackRestMethod `
                        -Method POST `
                        -Uri "$script:BaseUri/api/v1/auth/login" `
                        -BodyBytes $loginBody `
                        -TimeoutSec 15
                } finally {
                    [Array]::Clear($loginBody, 0, $loginBody.Length)
                    $loginBody = $null
                }
                $token = $login.data.access_token
                if (-not $token) { $token = $login.access_token }
                if ($token) { break }
            } catch {
                $message = [string]$_.Exception.Message
                if ($message -match '(?i)\(429\)|too many requests|rate.?limit') {
                    $rateLimited = $true
                    break
                }
                $login = $null
            }
        }
        if (-not $token) {
            if ($rateLimited) {
                throw 'Sub2API admin login is rate-limited. Wait a few seconds and retry.'
            }
            throw 'Sub2API admin login returned no access token.'
        }

        $expiresAt = [DateTimeOffset]::UtcNow.AddHours(12)
        try {
            Write-RouterAdminSessionCache -AccessToken ([string]$token) -ExpiresAtUtc $expiresAt
        } catch {
            # Cache is best-effort; a valid session is still returned.
        }

        return [pscustomobject]@{
            BaseUri = $script:BaseUri
            Headers = @{ Authorization = "Bearer $token" }
        }
    } finally {
        $password = $null
        $token = $null
        $login = $null
        if ($attempted) { $attempted.Clear() }
    }
}

function Get-RouterResponseData {
    param([Parameter(Mandatory)][AllowNull()]$Response)
    if ($null -eq $Response) { return }
    if ($null -ne $Response.PSObject.Properties['data']) { return $Response.data }
    return $Response
}

function Invoke-RouterApi {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][ValidateSet('GET', 'POST', 'PUT', 'DELETE')][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [object]$Body,
        [string]$IdempotencyKey,
        [ValidateRange(1, 300)][int]$TimeoutSec = 30
    )

    $headers = @{}
    foreach ($entry in $Session.Headers.GetEnumerator()) { $headers[$entry.Key] = $entry.Value }
    if ($IdempotencyKey) { $headers['Idempotency-Key'] = $IdempotencyKey }

    $arguments = @{
        Method = $Method
        Uri = "$($Session.BaseUri)$Path"
        Headers = $headers
        TimeoutSec = $TimeoutSec
    }
    $bodyBytes = $null
    if ($PSBoundParameters.ContainsKey('Body')) {
        $jsonBody = $Body | ConvertTo-Json -Depth 100 -Compress
        $bodyBytes = [Text.UTF8Encoding]::new($false).GetBytes($jsonBody)
        $arguments.BodyBytes = $bodyBytes
    }

    try {
        return Invoke-LoopbackRestMethod @arguments
    } catch {
        $statusCode = 'transport'
        $responseProperty = $_.Exception.PSObject.Properties['Response']
        if ($null -ne $responseProperty -and $null -ne $responseProperty.Value) {
            $statusProperty = $responseProperty.Value.PSObject.Properties['StatusCode']
            if ($null -ne $statusProperty -and $null -ne $statusProperty.Value) {
                $statusCode = [int]$statusProperty.Value
            }
        }
        $detail = ''
        if ($null -ne $_.ErrorDetails -and -not [string]::IsNullOrWhiteSpace([string]$_.ErrorDetails.Message)) {
            $detail = [string]$_.ErrorDetails.Message
            try {
                $errorBody = $detail | ConvertFrom-Json
                if ($null -ne $errorBody.PSObject.Properties['detail']) {
                    $detail = [string]$errorBody.detail
                } elseif ($null -ne $errorBody.PSObject.Properties['message']) {
                    $detail = [string]$errorBody.message
                } elseif ($null -ne $errorBody.PSObject.Properties['error']) {
                    $detail = [string]$errorBody.error
                }
            } catch {
                $errorBody = $null
            }
        }
        $detail = ($detail -replace '\s+', ' ').Trim()
        if ($detail.Length -gt 600) { $detail = $detail.Substring(0, 600) }
        $suffix = if ($detail) { ": $detail" } else { '' }
        throw "Sub2API request failed ($statusCode): $Method $Path$suffix"
    } finally {
        if ($null -ne $bodyBytes) { [Array]::Clear($bodyBytes, 0, $bodyBytes.Length) }
        $jsonBody = $null
        $headers.Clear()
        $arguments.Clear()
    }
}

function Get-RouterGroups {
    param([Parameter(Mandatory)]$Session)
    $response = Invoke-RouterApi -Session $Session -Method GET -Path '/api/v1/admin/groups/all?include_inactive=true'
    return @(Get-RouterResponseData -Response $response)
}

function Get-RouterAccounts {
    param([Parameter(Mandatory)]$Session)
    $response = Invoke-RouterApi -Session $Session -Method GET -Path '/api/v1/admin/accounts?page=1&page_size=200'
    $data = Get-RouterResponseData -Response $response
    if ($null -ne $data.PSObject.Properties['items']) { return @($data.items) }
    return @($data)
}

function ConvertTo-RouterResetAtUtc {
    param([AllowNull()]$Value)

    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) { return $null }
    if ($Value -is [DateTimeOffset]) { return $Value.ToUniversalTime() }
    if ($Value -is [DateTime]) {
        $dateTime = [DateTime]$Value
        if ($dateTime.Kind -eq [DateTimeKind]::Unspecified) {
            $dateTime = [DateTime]::SpecifyKind($dateTime, [DateTimeKind]::Utc)
        }
        return [DateTimeOffset]$dateTime.ToUniversalTime()
    }
    $parsed = [DateTimeOffset]::MinValue
    if ([DateTimeOffset]::TryParse(
            [string]$Value,
            [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::AssumeUniversal,
            [ref]$parsed)) {
        return $parsed.ToUniversalTime()
    }
    return $null
}

function Get-RouterObjectValue {
    param(
        [AllowNull()]$Object,
        [Parameter(Mandatory)][string]$Name
    )
    if ($null -eq $Object) { return $null }
    if ($Object -is [Collections.IDictionary]) {
        if ($Object.Contains($Name)) { return $Object[$Name] }
        return $null
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function ConvertTo-RouterNumber {
    param([AllowNull()]$Value)
    if ($null -eq $Value) { return $null }
    $parsed = 0.0
    if ([double]::TryParse(
            [string]$Value,
            [Globalization.NumberStyles]::Float,
            [Globalization.CultureInfo]::InvariantCulture,
            [ref]$parsed)) {
        return $parsed
    }
    return $null
}

function Get-RouterOAuthRecoveryState {
    param(
        [Parameter(Mandatory)]$Account,
        [AllowNull()]$ObservedResetAt,
        [switch]$ObservedExhausted,
        [DateTimeOffset]$NowUtc = [DateTimeOffset]::UtcNow
    )

    $resetAt = ConvertTo-RouterResetAtUtc -Value $ObservedResetAt
    if ($null -eq $resetAt) {
        $resetProperty = $Account.PSObject.Properties['rate_limit_reset_at']
        if ($null -ne $resetProperty) {
            $resetAt = ConvertTo-RouterResetAtUtc -Value $resetProperty.Value
        }
    }

    # Reuse the passive quota snapshot already collected by the usage monitor.
    # If either active quota window is exhausted, the account stays outside the
    # request path until every exhausted window has reset. No extra quota call is
    # made here; missing reset data falls back to one recovery probe per five hours.
    $extra = Get-RouterObjectValue -Object $Account -Name 'extra'
    $usageUpdatedAt = ConvertTo-RouterResetAtUtc -Value (
        Get-RouterObjectValue -Object $extra -Name 'codex_usage_updated_at')
    $usageExhausted = $false
    $quotaResetCandidates = [Collections.Generic.List[DateTimeOffset]]::new()
    foreach ($window in @(
        [pscustomobject]@{
            Used = @('codex_7d_used_percent', 'codex_primary_used_percent')
            ResetAt = @('codex_7d_reset_at', 'codex_primary_reset_at')
            ResetAfter = @('codex_7d_reset_after_seconds', 'codex_primary_reset_after_seconds')
        },
        [pscustomobject]@{
            Used = @('codex_5h_used_percent', 'codex_secondary_used_percent')
            ResetAt = @('codex_5h_reset_at', 'codex_secondary_reset_at')
            ResetAfter = @('codex_5h_reset_after_seconds', 'codex_secondary_reset_after_seconds')
        }
    )) {
        $usedPercent = $null
        foreach ($key in $window.Used) {
            $usedPercent = ConvertTo-RouterNumber -Value (Get-RouterObjectValue -Object $extra -Name $key)
            if ($null -ne $usedPercent) { break }
        }
        if ($null -eq $usedPercent -or $usedPercent -lt 100.0) { continue }
        $usageExhausted = $true

        $windowResetAt = $null
        foreach ($key in $window.ResetAt) {
            $windowResetAt = ConvertTo-RouterResetAtUtc -Value (Get-RouterObjectValue -Object $extra -Name $key)
            if ($null -ne $windowResetAt) { break }
        }
        if ($null -eq $windowResetAt -and $null -ne $usageUpdatedAt) {
            foreach ($key in $window.ResetAfter) {
                $resetAfterSeconds = ConvertTo-RouterNumber -Value (Get-RouterObjectValue -Object $extra -Name $key)
                if ($null -ne $resetAfterSeconds -and $resetAfterSeconds -gt 0) {
                    $windowResetAt = $usageUpdatedAt.AddSeconds($resetAfterSeconds)
                    break
                }
            }
        }
        if ($null -ne $windowResetAt) { $quotaResetCandidates.Add($windowResetAt) }
    }
    if ($quotaResetCandidates.Count -gt 0) {
        $quotaResetAt = $quotaResetCandidates | Sort-Object -Descending | Select-Object -First 1
        if ($null -eq $resetAt -or $quotaResetAt -gt $resetAt) { $resetAt = $quotaResetAt }
    }
    $schedulableProperty = $Account.PSObject.Properties['schedulable']
    $schedulable = $null -eq $schedulableProperty -or [bool]$schedulableProperty.Value
    $reason = ''
    foreach ($name in @('temp_unschedulable_reason', 'error_message')) {
        $property = $Account.PSObject.Properties[$name]
        if ($null -ne $property -and -not [string]::IsNullOrWhiteSpace([string]$property.Value)) {
            $reason = [string]$property.Value
            break
        }
    }
    $looksExhausted = $ObservedExhausted.IsPresent -or $usageExhausted -or
        $reason -match '(?i)quota|usage limit|rate.?limit|billing|payment|insufficient|exhaust|402|credit'

    if ($null -ne $resetAt -and $resetAt -gt $NowUtc) {
        $seconds = [Math]::Max(1L, [long][Math]::Ceiling(($resetAt - $NowUtc).TotalSeconds))
        return [pscustomobject][ordered]@{
            Action = 'defer'
            ShouldIsolate = $true
            NextCheckSeconds = $seconds
            ResetAt = $resetAt.UtcDateTime.ToString('o')
            Reason = if ($reason) { $reason } elseif ($usageExhausted) { 'OAuth quota exhausted until reset' } else { 'quota reset is in the future' }
        }
    }

    $resetReached = $null -ne $resetAt -and $resetAt -le $NowUtc
    if ($resetReached -or -not $schedulable -or $looksExhausted) {
        return [pscustomobject][ordered]@{
            Action = 'probe'
            ShouldIsolate = $true
            NextCheckSeconds = 18000L
            ResetAt = if ($null -eq $resetAt) { '' } else { $resetAt.UtcDateTime.ToString('o') }
            Reason = if ($reason) { $reason } elseif ($resetReached) { 'quota reset time reached' } else { 'quota exhausted without a reset time' }
        }
    }

    return [pscustomobject][ordered]@{
        Action = 'healthy'
        ShouldIsolate = $false
        NextCheckSeconds = 18000L
        ResetAt = ''
        Reason = ''
    }
}

function Set-RouterAccountGroupMembership {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId,
        [Parameter(Mandatory)][long]$GroupId,
        [Parameter(Mandatory)][bool]$Enabled,
        [AllowNull()]$Account
    )

    $detail = if ($null -eq $Account) {
        Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$AccountId")
    } else { $Account }
    $groupsProperty = $detail.PSObject.Properties['group_ids']
    $groupIds = if ($null -eq $groupsProperty) { @() } else {
        @($groupsProperty.Value | ForEach-Object { [long]$_ })
    }
    $currentlyEnabled = $groupIds -contains $GroupId
    if ($currentlyEnabled -eq $Enabled) { return $false }
    $nextGroups = @($groupIds | Where-Object { $_ -ne $GroupId })
    if ($Enabled) { $nextGroups += $GroupId }
    [void](Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/accounts/$AccountId" -Body @{
        group_ids = @($nextGroups | Select-Object -Unique)
        confirm_mixed_channel_risk = $true
    })
    return $true
}

function Get-RouterOAuthRoutingPriorities {
    param([AllowNull()]$OAuthFallback)

    $enabled = $false
    $preferOAuth = $true
    $configuredOAuthPriority = 1
    $configuredApiPriority = 100
    if ($null -ne $OAuthFallback) {
        $enabledProperty = $OAuthFallback.PSObject.Properties['enabled']
        if ($null -ne $enabledProperty) { $enabled = [bool]$enabledProperty.Value }
        $preferenceProperty = $OAuthFallback.PSObject.Properties['preferOAuth']
        if ($null -ne $preferenceProperty) { $preferOAuth = [bool]$preferenceProperty.Value }
        $oauthPriorityProperty = $OAuthFallback.PSObject.Properties['officialPriority']
        if ($null -ne $oauthPriorityProperty -and [int]$oauthPriorityProperty.Value -gt 0) {
            $configuredOAuthPriority = [int]$oauthPriorityProperty.Value
        }
        $apiPriorityProperty = $OAuthFallback.PSObject.Properties['fallbackPriority']
        if ($null -ne $apiPriorityProperty -and [int]$apiPriorityProperty.Value -gt 0) {
            $configuredApiPriority = [int]$apiPriorityProperty.Value
        }
    }

    if (-not $enabled) {
        return [pscustomobject][ordered]@{
            Enabled = $false
            PreferOAuth = $true
            OAuthPriority = 1
            ApiPriority = 10
        }
    }
    return [pscustomobject][ordered]@{
        Enabled = $true
        PreferOAuth = $preferOAuth
        OAuthPriority = if ($preferOAuth) { $configuredOAuthPriority } else { $configuredApiPriority }
        ApiPriority = if ($preferOAuth) { $configuredApiPriority } else { $configuredOAuthPriority }
    }
}

function Get-RouterCanonicalModelId {
    param([AllowEmptyString()][string]$ModelId)
    $value = $ModelId.Trim().ToLowerInvariant()
    if ($value.Contains('/')) { $value = $value.Substring($value.LastIndexOf('/') + 1) }
    if ($value -eq 'gpt-5.6') { return 'gpt-5.6-sol' }
    return $value
}

function Get-RouterModelIdentity {
    param([Parameter(Mandatory)][string]$ModelId)
    $raw = $ModelId.Trim().ToLowerInvariant()
    $leaf = Get-RouterCanonicalModelId -ModelId $raw
    $provider = switch -Regex ($raw) {
        '^(openai|chatgpt)/' { 'openai'; break }
        '^(anthropic|claude)/' { 'anthropic'; break }
        '^(google|gemini)/' { 'google'; break }
        '^(x-ai|xai|grok)/' { 'x-ai'; break }
        '^deepseek/' { 'deepseek'; break }
        '^(moonshotai|moonshot|kimi)/' { 'moonshot'; break }
        default {
            switch -Regex ($leaf) {
                '^(gpt-|chatgpt-|codex-)' { 'openai'; break }
                '^claude-' { 'anthropic'; break }
                '^gemini-' { 'google'; break }
                '^grok-' { 'x-ai'; break }
                '^deepseek-' { 'deepseek'; break }
                '^(kimi-|k3(?:-|$))' { 'moonshot'; break }
                default {
                    if ($raw.Contains('/')) { 'unknown-' + $raw.Substring(0, $raw.IndexOf('/')) }
                    else { 'unknown' }
                }
            }
        }
    }
    # These separator aliases are documented provider spellings, not fuzzy matching.
    if ($provider -eq 'google') { $leaf = $leaf -replace '^gemini-(\d+)-(\d+)-', 'gemini-$1.$2-' }
    if ($provider -eq 'anthropic') { $leaf = $leaf -replace '^(claude-[a-z]+-\d+)-(\d+)(-|$)', '$1.$2$3' }
    $display = Get-RouterRecommendedDisplayName -ModelId $ModelId
    $displayKey = [Text.RegularExpressions.Regex]::Replace($display.ToLowerInvariant(), '[^a-z0-9]+', '')
    return [pscustomobject][ordered]@{
        Provider = $provider
        RealId = $leaf
        IdentityKey = "$provider`:$leaf"
        DisplayCandidate = $display
        DisplayKey = $displayKey
    }
}

function Test-RouterSameModel {
    param([Parameter(Mandatory)][string]$LeftModelId, [Parameter(Mandatory)][string]$RightModelId)
    $left = Get-RouterModelIdentity -ModelId $LeftModelId
    $right = Get-RouterModelIdentity -ModelId $RightModelId
    return $left.DisplayKey -eq $right.DisplayKey -and $left.IdentityKey -eq $right.IdentityKey
}

function Test-RouterCodingPlanChannel {
    param(
        [AllowEmptyString()][string]$BaseUrl,
        [AllowEmptyString()][string]$ModelId,
        [AllowEmptyString()][string]$Extra = '{}'
    )
    try {
        $extraObject = $Extra | ConvertFrom-Json
        $kindProperty = $extraObject.PSObject.Properties['codex_router_channel_kind']
        if ($null -ne $kindProperty) {
            return ([string]$kindProperty.Value).Trim().ToLowerInvariant() -eq 'coding_plan'
        }
    } catch { }
    $uri = $null
    if (-not [Uri]::TryCreate($BaseUrl.Trim(), [UriKind]::Absolute, [ref]$uri)) { return $false }
    if ($uri.Scheme -ne 'https') { return $false }
    $host = $uri.DnsSafeHost.ToLowerInvariant()
    $path = $uri.AbsolutePath.TrimEnd('/').ToLowerInvariant()
    $model = $ModelId.Trim().ToLowerInvariant()
    return ($host -eq 'api.kimi.com' -and $path.StartsWith('/coding')) -or
        ($host -eq 'api.moonshot.ai' -and $path.StartsWith('/coding')) -or
        ($host -eq 'ark.cn-beijing.volces.com' -and
            ($path.StartsWith('/api/coding') -or $path.StartsWith('/api/plan'))) -or
        ($path.Contains('/coding') -and $model -match 'coding|code')
}

function Get-RouterChannelTier {
    param([Parameter(Mandatory)]$Model)
    if ((Get-RouterModelSource -Model $Model) -eq 'oauth') { return 0 }
    $extraProperty = $Model.PSObject.Properties['extra']
    $extra = if ($null -eq $extraProperty) { '{}' } else { [string]$extraProperty.Value }
    if (Test-RouterCodingPlanChannel -BaseUrl ([string]$Model.baseURL) -ModelId ([string]$Model.model) -Extra $extra) {
        return 1
    }
    return 2
}

function Get-RouterRecommendedDisplayName {
    param([Parameter(Mandatory)][string]$ModelId)
    $canonical = Get-RouterCanonicalModelId -ModelId $ModelId

    if ($canonical -match '^gpt-(5(?:\.\d+)?)(?:-(.+))?$') {
        $segments = @($Matches[1])
        if (-not [string]::IsNullOrWhiteSpace($Matches[2])) {
            $segments += @($Matches[2] -split '[-_]' | ForEach-Object {
                switch ($_.ToLowerInvariant()) {
                    'codex' { 'Codex'; break }
                    'fast' { 'Fast'; break }
                    'high' { 'High'; break }
                    'low' { 'Low'; break }
                    'max' { 'Max'; break }
                    'mini' { 'Mini'; break }
                    'nano' { 'Nano'; break }
                    default {
                        if ($_.Length -le 1) { $_.ToUpperInvariant() }
                        else { $_.Substring(0, 1).ToUpperInvariant() + $_.Substring(1).ToLowerInvariant() }
                    }
                }
            })
        }
        return 'ChatGPT-' + ($segments -join '-')
    }

    switch -Regex ($canonical) {
        '^claude-opus-5-fast(?:-|$)' { return 'Claude-Opus-5-Fast' }
        '^claude-opus-5' { return 'Claude-Opus-5' }
        '^claude-sonnet-5' { return 'Claude-Sonnet-5' }
        '^claude-fable-5' { return 'Claude-Fable-5' }
        '^claude-opus-4(?:[.-]8)-fast(?:-|$)' { return 'Claude-Opus-4.8-Fast' }
        '^claude-opus-4(?:[.-]8)(?:-|$)' { return 'Claude-Opus-4.8' }
        '^claude-opus-4(?:[.-]7)-fast(?:-|$)' { return 'Claude-Opus-4.7-Fast' }
        '^claude-opus-4(?:[.-]7)(?:-|$)' { return 'Claude-Opus-4.7' }
        '^claude-(opus|sonnet)-4(?:[.-]6)(?:-|$)' {
            return 'Claude-' + $Matches[1].Substring(0, 1).ToUpperInvariant() + $Matches[1].Substring(1) + '-4.6'
        }
        '^claude-(opus|sonnet|haiku)-4(?:[.-]5)(?:-|$)' {
            return 'Claude-' + $Matches[1].Substring(0, 1).ToUpperInvariant() + $Matches[1].Substring(1) + '-4.5'
        }
        '^claude-(opus|sonnet|haiku)-4(?:-|$)' {
            return 'Claude-' + $Matches[1].Substring(0, 1).ToUpperInvariant() + $Matches[1].Substring(1) + '-4'
        }
        '^claude-4-(opus|sonnet|haiku)(?:-|$)' {
            return 'Claude-' + $Matches[1].Substring(0, 1).ToUpperInvariant() + $Matches[1].Substring(1) + '-4'
        }
        '^gemini-3(?:[.-]6)-flash' { return 'Gemini-3.6-Flash' }
        '^gemini-3(?:[.-]6)-pro' { return 'Gemini-3.6-Pro' }
        '^gemini-3(?:[.-]5)-flash' { return 'Gemini-3.5-Flash' }
        '^gemini-3(?:[.-]1)-pro' { return 'Gemini-3.1-Pro' }
        '^gemini-3-pro-image-preview' { return 'Gemini-3-Pro-Image-Preview' }
        '^gemini-3-pro' { return 'Gemini-3-Pro' }
        '^gemini-3-flash' { return 'Gemini-3-Flash' }
        '^gemini-2(?:[.-]5)-pro' { return 'Gemini-2.5-Pro' }
        '^gemini-2(?:[.-]5)-flash' { return 'Gemini-2.5-Flash' }
        '^(kimi-k3|k3)(-|$)' { return 'Kimi-K3' }
        '^kimi-for-coding' { return 'Kimi-For-Coding' }
        '^kimi-k2\.7' { return 'Kimi-K2.7-Code' }
        '^mimo-v2\.5-pro' { return 'MiMo-V2.5-Pro' }
        '^deepseek-v4-pro' { return 'DeepSeek-V4-Pro' }
        '^deepseek-v4-flash' { return 'DeepSeek-V4-Flash' }
        '^deepseek-v3\.2' { return 'DeepSeek-V3.2' }
        '^deepseek-v3\.1' { return 'DeepSeek-V3.1' }
        '^deepseek-v3(?:-|$)' { return 'DeepSeek-V3' }
        '^deepseek-r1' { return 'DeepSeek-R1' }
        '^deepseek-reasoner' { return 'DeepSeek-Reasoner' }
        '^deepseek-chat' { return 'DeepSeek-Chat' }
        '^grok-4\.5' { return 'Grok-4.5' }
        '^(cursor-)?composer-2\.5' { return 'Composer-2.5' }
        '^glm-5(?:[.-]2)' { return 'GLM-5.2' }
    }
    $leaf = $ModelId.Trim()
    if ($leaf.Contains('/')) { $leaf = $leaf.Substring($leaf.LastIndexOf('/') + 1) }
    return $leaf.Replace('_', '-')
}

function Test-RouterModelAliasCustomized {
    param([Parameter(Mandatory)]$Model)
    $customProperty = $Model.PSObject.Properties['aliasCustomized']
    if ($null -ne $customProperty) { return [bool]$customProperty.Value }
    $aliasProperty = $Model.PSObject.Properties['alias']
    $alias = if ($null -eq $aliasProperty) { '' } else { ([string]$aliasProperty.Value).Trim() }
    if ([string]::IsNullOrWhiteSpace($alias)) { return $false }
    $modelId = ([string]$Model.model).Trim()
    $recommended = Get-RouterRecommendedDisplayName -ModelId $modelId
    $normalize = {
        param([string]$Value)
        return [Text.RegularExpressions.Regex]::Replace($Value.ToLowerInvariant(), '[^a-z0-9]+', '')
    }
    $normalizedAlias = & $normalize $alias
    $automaticNames = @(
        $modelId,
        (Get-RouterCanonicalModelId -ModelId $modelId),
        $recommended,
        ($recommended + '(OAuth)')
    )
    if ($recommended.StartsWith('ChatGPT-', [StringComparison]::OrdinalIgnoreCase)) {
        $automaticNames += $recommended.Substring(4)
        $automaticNames += $recommended.Substring(4) + '(OAuth)'
    }
    if ((Get-RouterCanonicalModelId -ModelId $modelId) -eq 'deepseek-v4-pro') {
        # Include the previous incorrect Flash recommendation so Apply can refresh it.
        $automaticNames += @('DeepSeek V4 Pro', 'DeepSeek-V4-Pro', 'DeepSeek-V4-Flash', 'DeepSeek V4 Flash')
    }
    $normalizedAutomaticNames = @($automaticNames | ForEach-Object { & $normalize ([string]$_) })
    return $normalizedAutomaticNames -notcontains $normalizedAlias
}

function Get-RouterModelDisplayName {
    param(
        [Parameter(Mandatory)]$Model,
        [AllowNull()]$Route
    )
    $aliasProperty = $Model.PSObject.Properties['alias']
    $alias = if ($null -eq $aliasProperty) { '' } else { ([string]$aliasProperty.Value).Trim() }
    if ((Test-RouterModelAliasCustomized -Model $Model) -and -not [string]::IsNullOrWhiteSpace($alias)) {
        return $alias
    }
    $displayName = Get-RouterRecommendedDisplayName -ModelId ([string]$Model.model)
    $source = Get-RouterModelSource -Model $Model
    # The suffix reflects which quota actually serves this model after Apply, so
    # Codex shows "(OAuth)" only while the subscription is the live route.
    $servedByProperty = if ($null -eq $Route) { $null } else { $Route.PSObject.Properties['ServedBy'] }
    if ($null -ne $servedByProperty) {
        $servedBy = ([string]$servedByProperty.Value).Trim().ToLowerInvariant()
        if ($servedBy -eq 'oauth') { return $displayName + '(OAuth)' }
        if ($servedBy -eq 'api') { return $displayName }
    }
    $mergedOAuth = $null -ne $Route -and
        $null -ne $Route.PSObject.Properties['IsMergedOAuthRoute'] -and
        [bool]$Route.IsMergedOAuthRoute
    if ($source -eq 'oauth' -and -not $mergedOAuth) { return $displayName + '(OAuth)' }
    return $displayName
}

function Get-RouterModelSource {
    param([Parameter(Mandatory)]$Model)
    $property = $Model.PSObject.Properties['source']
    if ($null -eq $property -or [string]::IsNullOrWhiteSpace([string]$property.Value)) {
        return 'apikey'
    }
    return ([string]$property.Value).Trim().ToLowerInvariant()
}

function Get-RouterSplitPublicModelId {
    param([Parameter(Mandatory)]$Model)
    $canonical = Get-RouterCanonicalModelId -ModelId ([string]$Model.model)
    $slug = [Text.RegularExpressions.Regex]::Replace($canonical, '[^a-z0-9._-]+', '-')
    $credentialProperty = $Model.PSObject.Properties['credentialName']
    $credentialName = if ($null -eq $credentialProperty) { '' } else { [string]$credentialProperty.Value }
    $baseUrlProperty = $Model.PSObject.Properties['baseURL']
    $baseUrl = if ($null -eq $baseUrlProperty) { '' } else { [string]$baseUrlProperty.Value }
    $seed = @(
        ([string]$Model.model).Trim().ToLowerInvariant(),
        $baseUrl.Trim().TrimEnd('/').ToLowerInvariant(),
        $credentialName.Trim().ToLowerInvariant()
    ) -join "`n"
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha256.ComputeHash([Text.Encoding]::UTF8.GetBytes($seed))
        $token = ([BitConverter]::ToString($digest, 0, 6)).Replace('-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
    return "$slug--api-$token"
}

function Get-RouterDiscoveredOAuthModelsForAccount {
    param(
        [AllowNull()]$DiscoveredOAuthModelsByAccount,
        [Parameter(Mandatory)][long]$AccountId
    )

    if ($null -eq $DiscoveredOAuthModelsByAccount) { return @() }
    $value = $null
    if ($DiscoveredOAuthModelsByAccount -is [Collections.IDictionary]) {
        foreach ($key in @($AccountId, [string]$AccountId)) {
            if ($DiscoveredOAuthModelsByAccount.Contains($key)) {
                $value = $DiscoveredOAuthModelsByAccount[$key]
                break
            }
        }
    } else {
        $property = $DiscoveredOAuthModelsByAccount.PSObject.Properties[[string]$AccountId]
        if ($null -ne $property) { $value = $property.Value }
    }
    if ($null -eq $value) { return @() }

    return @($value | ForEach-Object {
        $modelId = if ($_ -is [string]) {
            [string]$_
        } else {
            $idProperty = $_.PSObject.Properties['id']
            if ($null -eq $idProperty) { '' } else { [string]$idProperty.Value }
        }
        $modelId = $modelId.Trim()
        if (-not [string]::IsNullOrWhiteSpace($modelId)) { $modelId }
    } | Select-Object -Unique)
}

function Get-RouterModelRoutePlan {
    param(
        [Parameter(Mandatory)]$RouterConfig,
        [AllowNull()]$DiscoveredOAuthModelsByAccount
    )

    $modelsProperty = $RouterConfig.PSObject.Properties['models']
    $models = @(if ($null -ne $modelsProperty) { $modelsProperty.Value })
    $oauthIdsProperty = $RouterConfig.PSObject.Properties['oauthAccountIds']
    $oauthSelectionInitialized = $null -ne $oauthIdsProperty
    $oauthAccountIds = if ($oauthSelectionInitialized) {
        @($oauthIdsProperty.Value | ForEach-Object { [long]$_ })
    } else { @() }
    $fallbackProperty = $RouterConfig.PSObject.Properties['oauthFallback']
    $fallbackEnabled = $false
    if ($null -ne $fallbackProperty -and $null -ne $fallbackProperty.Value) {
        $enabledProperty = $fallbackProperty.Value.PSObject.Properties['enabled']
        if ($null -ne $enabledProperty) { $fallbackEnabled = [bool]$enabledProperty.Value }
    }
    $selectionsProperty = $RouterConfig.PSObject.Properties['fallbackChannelSelections']
    $selections = if ($null -eq $selectionsProperty) { $null } else { $selectionsProperty.Value }

    $descriptors = @(
        for ($index = 0; $index -lt $models.Count; $index++) {
            $model = $models[$index]
            $modelId = ([string]$model.model).Trim()
            if ([string]::IsNullOrWhiteSpace($modelId)) {
                throw "Model entry #$($index + 1) has an empty model ID."
            }
            $source = Get-RouterModelSource -Model $model
            $selected = $source -ne 'oauth' -or -not $oauthSelectionInitialized -or
                $oauthAccountIds -contains [long]$model.oauthAccountId
            [pscustomobject][ordered]@{
                Index = $index
                Model = $model
                ModelId = $modelId
                Source = $source
                CanonicalModelId = Get-RouterCanonicalModelId -ModelId $modelId
                Identity = Get-RouterModelIdentity -ModelId $modelId
                Selected = $selected
                Discovered = $false
            }
        }
    )
    # Only OAuth model rows the user explicitly added participate in routing.
    # Enrolling an OAuth account alone must not invent primary OAuth routes or
    # force same-name third-party channels into fallback-only mode.
    $selectedOAuth = @($descriptors | Where-Object { $_.Source -eq 'oauth' -and $_.Selected })
    $null = $DiscoveredOAuthModelsByAccount
    $catalogIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $plan = @(
        foreach ($descriptor in $descriptors) {
            $matchingOAuth = @($selectedOAuth | Where-Object {
                (Test-RouterSameModel -LeftModelId $_.ModelId -RightModelId $descriptor.ModelId)
            })
            $isOAuth = $descriptor.Source -eq 'oauth'
            $matchingSelectedApiFallbacks = if ($isOAuth -and $fallbackEnabled) {
                @($descriptors | Where-Object {
                    $_.Source -ne 'oauth' -and
                    (Test-RouterSameModel -LeftModelId $_.ModelId -RightModelId $descriptor.ModelId) -and
                    (Test-RouterFallbackChannelSelected `
                        -Selections $selections `
                        -ModelId $_.ModelId `
                        -BaseUrl ([string]$_.Model.baseURL))
                })
            } else { @() }
            $isFallback = $false
            $joinRouter = $descriptor.Selected
            $publicModelId = $descriptor.ModelId
            $requestModelIds = @($publicModelId)
            $includeInCatalog = $descriptor.Selected

            if ($isOAuth) {
                $includeInCatalog = $descriptor.Selected -and $catalogIds.Add($publicModelId)
            } elseif ($fallbackEnabled -and $matchingOAuth.Count -gt 0) {
                $isFallback = Test-RouterFallbackChannelSelected `
                    -Selections $selections `
                    -ModelId $descriptor.ModelId `
                    -BaseUrl ([string]$descriptor.Model.baseURL)
                $joinRouter = $isFallback
                $publicModelId = [string]$matchingOAuth[0].ModelId
                $hasExplicitMatchingOAuth = @($matchingOAuth | Where-Object {
                    -not [bool]$_.Discovered
                }).Count -gt 0
                if ($hasExplicitMatchingOAuth) {
                    $includeInCatalog = $false
                } else {
                    # With an implicit OAuth binding there is no OAuth model row
                    # to represent the merged route in the Codex catalog. Reuse
                    # the first API row's metadata while exposing the OAuth ID.
                    $includeInCatalog = $catalogIds.Add($publicModelId)
                }
                if ($isFallback) {
                    $requestModelIds = @($matchingOAuth | ForEach-Object { $_.ModelId } | Select-Object -Unique)
                }
            } elseif (-not $fallbackEnabled) {
                $sameCanonicalCount = @($descriptors | Where-Object {
                    $_.Selected -and (Test-RouterSameModel -LeftModelId $_.ModelId -RightModelId $descriptor.ModelId)
                }).Count
                if ($sameCanonicalCount -gt 1) {
                    $publicModelId = Get-RouterSplitPublicModelId -Model $descriptor.Model
                    $requestModelIds = @($publicModelId)
                }
                $includeInCatalog = $catalogIds.Add($publicModelId)
            } else {
                $includeInCatalog = $catalogIds.Add($publicModelId)
            }

            [pscustomobject][ordered]@{
                Index = [int]$descriptor.Index
                Model = $descriptor.Model
                Source = $descriptor.Source
                CanonicalModelId = $descriptor.CanonicalModelId
                IdentityKey = [string]$descriptor.Identity.IdentityKey
                IdentityDisplayCandidate = [string]$descriptor.Identity.DisplayCandidate
                PublicModelId = $publicModelId
                RequestModelIds = @($requestModelIds)
                IncludeInCatalog = [bool]$includeInCatalog
                JoinRouter = [bool]$joinRouter
                IsOAuthFallback = [bool]$isFallback
                IsMergedOAuthRoute = [bool]($isOAuth -and $fallbackEnabled -and @($matchingSelectedApiFallbacks).Count -gt 0)
            }
        }
    )
    return $plan
}

function Get-RouterDefaultPublicModelId {
    param(
        [Parameter(Mandatory)]$RouterConfig,
        [AllowNull()][object[]]$RoutePlan
    )
    $plan = if ($null -eq $RoutePlan) {
        @(Get-RouterModelRoutePlan -RouterConfig $RouterConfig)
    } else { @($RoutePlan) }
    $visible = @($plan | Where-Object { $_.IncludeInCatalog })
    if ($visible.Count -eq 0) { throw 'No routed model is available for the Codex catalog.' }

    $defaultProperty = $RouterConfig.PSObject.Properties['defaultModel']
    $requested = if ($null -eq $defaultProperty) { '' } else { ([string]$defaultProperty.Value).Trim() }
    if (-not [string]::IsNullOrWhiteSpace($requested)) {
        $exact = @($visible | Where-Object { [string]$_.Model.model -ieq $requested } | Select-Object -First 1)
        if ($exact.Count -gt 0) { return [string]$exact[0].PublicModelId }
        $canonical = Get-RouterCanonicalModelId -ModelId $requested
        $canonicalMatch = @($visible | Where-Object { $_.CanonicalModelId -eq $canonical } | Select-Object -First 1)
        if ($canonicalMatch.Count -gt 0) { return [string]$canonicalMatch[0].PublicModelId }
    }
    return [string]$visible[0].PublicModelId
}

function Get-RouterFallbackChannelKey {
    param(
        [Parameter(Mandatory)][string]$ModelId,
        [Parameter(Mandatory)][AllowEmptyString()][string]$BaseUrl
    )
    $canonical = Get-RouterCanonicalModelId -ModelId $ModelId
    $normalizedUrl = $BaseUrl.Trim().TrimEnd('/').ToLowerInvariant()
    return $canonical + '|' + $normalizedUrl
}

function Test-RouterFallbackChannelSelected {
    param(
        [AllowNull()]$Selections,
        [Parameter(Mandatory)][string]$ModelId,
        [Parameter(Mandatory)][AllowEmptyString()][string]$BaseUrl
    )
    if ($null -eq $Selections) { return $true }
    $canonical = Get-RouterCanonicalModelId -ModelId $ModelId
    $property = if ($Selections -is [Collections.IDictionary]) {
        if ($Selections.Contains($canonical)) {
            [pscustomobject]@{ Value = $Selections[$canonical] }
        } else { $null }
    } else {
        $Selections.PSObject.Properties[$canonical]
    }
    if ($null -eq $property) { return $true }
    $channelKey = Get-RouterFallbackChannelKey -ModelId $ModelId -BaseUrl $BaseUrl
    return @($property.Value) -icontains $channelKey
}

function Get-RouterEffectiveApiPriority {
    param(
        [Parameter(Mandatory)][int]$ConfiguredPriority,
        [Parameter(Mandatory)][int]$MinimumMatchingPriority,
        [Parameter(Mandatory)][int]$ApiBasePriority,
        [Parameter(Mandatory)][int]$OAuthPriority,
        [Parameter(Mandatory)][bool]$PreferOAuth
    )
    $offset = [Math]::Max(0, $ConfiguredPriority - $MinimumMatchingPriority)
    $effective = [Math]::Max(1, $ApiBasePriority + $offset)
    if (-not $PreferOAuth -and $OAuthPriority -gt 1) {
        $effective = [Math]::Min($effective, $OAuthPriority - 1)
    }
    return [int]$effective
}

function Get-RouterOAuthModelSuggestions {
    param([Parameter(Mandatory)][string]$Platform)
    switch ($Platform.Trim().ToLowerInvariant()) {
        'openai' {
            return @(
                [pscustomobject][ordered]@{ id = 'gpt-5.6-sol'; displayName = 'ChatGPT-5.6-Sol' }
                [pscustomobject][ordered]@{ id = 'gpt-5.6-terra'; displayName = 'ChatGPT-5.6-Terra' }
                [pscustomobject][ordered]@{ id = 'gpt-5.6-luna'; displayName = 'ChatGPT-5.6-Luna' }
                [pscustomobject][ordered]@{ id = 'gpt-5.5'; displayName = 'ChatGPT-5.5' }
                [pscustomobject][ordered]@{ id = 'gpt-5.4'; displayName = 'ChatGPT-5.4' }
                [pscustomobject][ordered]@{ id = 'gpt-5.4-mini'; displayName = 'ChatGPT-5.4-mini' }
                [pscustomobject][ordered]@{ id = 'gpt-5.3-codex-spark'; displayName = 'ChatGPT-5.3-Codex-Spark' }
                [pscustomobject][ordered]@{ id = 'codex-auto-review'; displayName = 'Codex Auto Review' }
                [pscustomobject][ordered]@{ id = 'gpt-5.2'; displayName = 'ChatGPT-5.2' }
            )
        }
        'antigravity' {
            return @(
                [pscustomobject][ordered]@{ id = 'gemini-3-flash'; displayName = 'Gemini-3-Flash' }
                [pscustomobject][ordered]@{ id = 'gemini-3.1-pro-high'; displayName = 'Gemini-3.1-Pro-High' }
                [pscustomobject][ordered]@{ id = 'gemini-3.1-pro-low'; displayName = 'Gemini-3.1-Pro-Low' }
                [pscustomobject][ordered]@{ id = 'gemini-3-pro-high'; displayName = 'Gemini-3-Pro-High' }
                [pscustomobject][ordered]@{ id = 'claude-sonnet-4-5'; displayName = 'Claude-Sonnet-4.5' }
                [pscustomobject][ordered]@{ id = 'claude-opus-4-6'; displayName = 'Claude-Opus-4.6' }
            )
        }
        'grok' {
            return @(
                [pscustomobject][ordered]@{ id = 'grok-4.5'; displayName = 'Grok-4.5' }
                [pscustomobject][ordered]@{ id = 'grok-4.3'; displayName = 'Grok-4.3' }
            )
        }
        default { return @() }
    }
}

function Get-RouterAccountPlatformMap {
    param([Parameter(Mandatory)]$Session)
    $map = @{}
    foreach ($account in @(Get-RouterAccounts -Session $Session)) {
        $accountId = 0
        try { $accountId = [long]$account.id } catch { continue }
        if ($accountId -le 0) { continue }
        $platform = ''
        $platformProperty = $account.PSObject.Properties['platform']
        if ($null -ne $platformProperty) {
            $platform = ([string]$platformProperty.Value).Trim().ToLowerInvariant()
        }
        if ([string]::IsNullOrWhiteSpace($platform)) { continue }
        $map[[string]$accountId] = $platform
    }
    return $map
}

function Get-RouterCompositeTargetPlatform {
    param(
        [Parameter(Mandatory)]$Model,
        [AllowNull()]$AccountPlatformById
    )

    $source = Get-RouterModelSource -Model $Model
    if ($source -ne 'oauth') { return 'openai' }

    $platformProperty = $Model.PSObject.Properties['oauthPlatform']
    if ($null -eq $platformProperty) {
        $platformProperty = $Model.PSObject.Properties['oauth_platform']
    }
    $platform = if ($null -eq $platformProperty) {
        ''
    } else {
        ([string]$platformProperty.Value).Trim().ToLowerInvariant()
    }
    if (-not [string]::IsNullOrWhiteSpace($platform)) {
        if ($platform -eq 'google_one' -or $platform -eq 'gemini') { return 'gemini' }
        return $platform
    }

    $accountIdProperty = $Model.PSObject.Properties['oauthAccountId']
    if ($null -eq $accountIdProperty) {
        $accountIdProperty = $Model.PSObject.Properties['oauth_account_id']
    }
    if ($null -ne $accountIdProperty -and $null -ne $AccountPlatformById) {
        try {
            $accountId = [long]$accountIdProperty.Value
            $key = [string]$accountId
            if ($AccountPlatformById -is [Collections.IDictionary]) {
                if ($AccountPlatformById.Contains($key)) { return [string]$AccountPlatformById[$key] }
                if ($AccountPlatformById.Contains($accountId)) { return [string]$AccountPlatformById[$accountId] }
            } else {
                $lookup = $AccountPlatformById.PSObject.Properties[$key]
                if ($null -ne $lookup -and -not [string]::IsNullOrWhiteSpace([string]$lookup.Value)) {
                    return ([string]$lookup.Value).Trim().ToLowerInvariant()
                }
            }
        } catch { }
    }
    return 'openai'
}

function Get-RouterOpenRouterUpstreamModelId {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$ModelId)
    $value = $ModelId.Trim()
    if ([string]::IsNullOrWhiteSpace($value)) { return $value }
    if ($value -match '^(?i)claude/') {
        return 'anthropic/' + $value.Substring(7)
    }
    if ($value -match '^(?i)claude-' -and $value -notmatch '/') {
        return 'anthropic/' + $value
    }
    # OpenRouter exposes Gemini 3.1 Pro as preview variants, not the
    # Antigravity subscription alias "high". The regular preview supports tools
    # and has broader regional availability than the custom-tools variant.
    if ($value -ieq 'google/gemini-3.1-pro-high') {
        return 'google/gemini-3.1-pro-preview'
    }
    return $value
}

function Get-RouterUpstreamModelId {
    param(
        [Parameter(Mandatory)][AllowEmptyString()][string]$ModelId,
        [AllowEmptyString()][string]$BaseUrl = ''
    )
    $value = $ModelId.Trim()
    if ([string]::IsNullOrWhiteSpace($value)) { return $value }
    $hostName = ''
    try {
        $uri = $null
        if ([Uri]::TryCreate($BaseUrl.Trim(), [UriKind]::Absolute, [ref]$uri)) {
            $hostName = $uri.Host.ToLowerInvariant()
        }
    } catch { }
    if ($hostName -eq 'openrouter.ai' -or $hostName.EndsWith('.openrouter.ai')) {
        return Get-RouterOpenRouterUpstreamModelId -ModelId $value
    }
    return $value
}

function Get-RouterServableCatalogRoutes {
    param(
        [Parameter(Mandatory)][AllowNull()][object[]]$RoutePlan,
        [AllowNull()][Collections.IDictionary]$IsolatedOAuthAccountIds,
        [AllowNull()][object]$OAuthAccountIds,
        [bool]$OAuthSelectionInitialized = $false
    )

    $selectedOAuthIds = @()
    if ($OAuthSelectionInitialized -and $null -ne $OAuthAccountIds) {
        $selectedOAuthIds = @($OAuthAccountIds | ForEach-Object { [long]$_ } | Where-Object { $_ -gt 0 })
    }
    $isolated = @{}
    if ($null -ne $IsolatedOAuthAccountIds) {
        foreach ($key in @($IsolatedOAuthAccountIds.Keys)) {
            try { $isolated[[long]$key] = $true } catch {
                try { $isolated[[long][string]$key] = $true } catch { }
            }
        }
    }

    $joinedApiPublicIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($route in @($RoutePlan)) {
        if ((Get-RouterModelSource -Model $route.Model) -eq 'oauth') { continue }
        if (-not [bool]$route.JoinRouter) { continue }
        $joinedPublicId = ([string]$route.PublicModelId).Trim()
        if (-not [string]::IsNullOrWhiteSpace($joinedPublicId)) {
            [void]$joinedApiPublicIds.Add($joinedPublicId)
        }
    }

    $catalogIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $servable = @(
        foreach ($route in @($RoutePlan)) {
            if (-not [bool]$route.IncludeInCatalog) { continue }
            $source = Get-RouterModelSource -Model $route.Model
            $publicModelId = ([string]$route.PublicModelId).Trim()
            if ([string]::IsNullOrWhiteSpace($publicModelId)) { continue }

            if ($source -eq 'oauth') {
                $oauthId = 0
                try { $oauthId = [long]$route.Model.oauthAccountId } catch { $oauthId = 0 }
                $accountSelected = -not $OAuthSelectionInitialized -or (
                    $oauthId -gt 0 -and $selectedOAuthIds -contains $oauthId)
                $accountIsolated = $oauthId -gt 0 -and $isolated.ContainsKey($oauthId)
                # A fallback must serve the exact public ID. In manual split
                # mode the API row has its own hashed ID and cannot serve the
                # OAuth canonical ID after that account is isolated.
                $hasApiFallback = $joinedApiPublicIds.Contains($publicModelId)
                if (-not $accountSelected) { continue }
                if ($accountIsolated -and -not $hasApiFallback) { continue }
            }

            if (-not $catalogIds.Add($publicModelId)) { continue }
            $servedBy = if ($source -ne 'oauth') {
                'api'
            } else {
                $oauthId = 0
                try { $oauthId = [long]$route.Model.oauthAccountId } catch { $oauthId = 0 }
                if ($oauthId -gt 0 -and $isolated.ContainsKey($oauthId)) { 'api' } else { 'oauth' }
            }
            [pscustomobject][ordered]@{
                Index = [int]$route.Index
                Model = $route.Model
                Source = $source
                CanonicalModelId = [string]$route.CanonicalModelId
                IdentityKey = if ($null -eq $route.PSObject.Properties['IdentityKey']) {
                    [string](Get-RouterModelIdentity -ModelId ([string]$route.Model.model)).IdentityKey
                } else { [string]$route.IdentityKey }
                PublicModelId = $publicModelId
                RequestModelIds = @($route.RequestModelIds)
                IncludeInCatalog = $true
                JoinRouter = [bool]$route.JoinRouter
                IsOAuthFallback = [bool]$route.IsOAuthFallback
                IsMergedOAuthRoute = [bool]$route.IsMergedOAuthRoute
                ServedBy = $servedBy
            }
        }
    )
    return @($servable)
}

# Routing (not catalog) view of the same plan. Composite routes must keep every
# platform that can still serve a public model, including API fallback rows that
# are deliberately hidden from the Codex menu. Filtering the composite sync by
# catalog-only routes used to delete the cross-platform fallback route (for
# example gemini-3.1-pro-high|openai) right after Apply created it.
function Get-RouterServableRoutingRoutes {
    param(
        [Parameter(Mandatory)][AllowNull()][object[]]$RoutePlan,
        [AllowNull()][Collections.IDictionary]$IsolatedOAuthAccountIds,
        [AllowNull()][object]$OAuthAccountIds,
        [bool]$OAuthSelectionInitialized = $false
    )

    $catalog = @(Get-RouterServableCatalogRoutes `
        -RoutePlan $RoutePlan `
        -IsolatedOAuthAccountIds $IsolatedOAuthAccountIds `
        -OAuthAccountIds $OAuthAccountIds `
        -OAuthSelectionInitialized:$OAuthSelectionInitialized)
    $servablePublicIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($route in $catalog) {
        [void]$servablePublicIds.Add([string]$route.PublicModelId)
    }

    $isolated = @{}
    if ($null -ne $IsolatedOAuthAccountIds) {
        foreach ($key in @($IsolatedOAuthAccountIds.Keys)) {
            try { $isolated[[long]$key] = $true } catch {
                try { $isolated[[long][string]$key] = $true } catch { }
            }
        }
    }
    $selectedOAuthIds = @()
    if ($OAuthSelectionInitialized -and $null -ne $OAuthAccountIds) {
        $selectedOAuthIds = @($OAuthAccountIds | ForEach-Object { [long]$_ } | Where-Object { $_ -gt 0 })
    }

    $extra = @(
        foreach ($route in @($RoutePlan)) {
            if ([bool]$route.IncludeInCatalog) { continue }
            if (-not [bool]$route.JoinRouter) { continue }
            $publicModelId = ([string]$route.PublicModelId).Trim()
            if ([string]::IsNullOrWhiteSpace($publicModelId)) { continue }
            if (-not $servablePublicIds.Contains($publicModelId)) { continue }
            $source = Get-RouterModelSource -Model $route.Model
            if ($source -eq 'oauth') {
                $oauthId = 0
                try { $oauthId = [long]$route.Model.oauthAccountId } catch { $oauthId = 0 }
                if ($OAuthSelectionInitialized -and ($oauthId -le 0 -or $selectedOAuthIds -notcontains $oauthId)) { continue }
                if ($oauthId -gt 0 -and $isolated.ContainsKey($oauthId)) { continue }
            }
            $route
        }
    )
    return @(@($catalog) + @($extra))
}

function Get-RouterCompositeRoutePlan {
    param(
        [Parameter(Mandatory)][object[]]$RoutePlan,
        [AllowNull()]$AccountPlatformById,
        [AllowNull()][object]$ExcludedOAuthAccountIds
    )

    $excluded = @{}
    foreach ($id in @($ExcludedOAuthAccountIds)) {
        if ($null -eq $id) { continue }
        try { $excluded[[long]$id] = $true } catch { }
    }
    $byRouteTarget = [ordered]@{}
    foreach ($route in @($RoutePlan)) {
        if (-not [bool]$route.IncludeInCatalog -and -not [bool]$route.JoinRouter) { continue }
        $publicModelId = ([string]$route.PublicModelId).Trim()
        if ([string]::IsNullOrWhiteSpace($publicModelId)) { continue }
        $upstreamModelId = ([string]$route.Model.model).Trim()
        if ([string]::IsNullOrWhiteSpace($upstreamModelId)) { $upstreamModelId = $publicModelId }
        $routeSource = Get-RouterModelSource -Model $route.Model
        # An OAuth account that is out of quota is not in the Router group, so its
        # platform route would only produce "no available accounts" 503 responses.
        if ($routeSource -eq 'oauth' -and $excluded.Count -gt 0) {
            $routeAccountId = 0
            try { $routeAccountId = [long]$route.Model.oauthAccountId } catch { $routeAccountId = 0 }
            if ($routeAccountId -gt 0 -and $excluded.ContainsKey($routeAccountId)) { continue }
        }
        $targetPlatform = Get-RouterCompositeTargetPlatform `
            -Model $route.Model `
            -AccountPlatformById $AccountPlatformById
        # OpenAI-compatible API channels always schedule through the openai
        # platform, even when the public id looks like another vendor.
        if ($routeSource -ne 'oauth') {
            $targetPlatform = 'openai'
        }

        $routeKey = $publicModelId.ToLowerInvariant() + '|' + $targetPlatform.ToLowerInvariant()
        $priority = if ($routeSource -eq 'oauth') { 1 } else { 100 }
        if ($byRouteTarget.Contains($routeKey)) {
            # Same public model + platform (e.g. ChatGPT OAuth + Chiral API) must
            # keep the higher-priority OAuth entry when both are present.
            $existing = $byRouteTarget[$routeKey]
            if ([int]$existing.Priority -le $priority) { continue }
        }

        $byRouteTarget[$routeKey] = [pscustomobject][ordered]@{
            PublicModelId = $publicModelId
            UpstreamModelId = $upstreamModelId
            TargetPlatform = $targetPlatform
            Priority = $priority
        }
    }
    return @($byRouteTarget.Values)
}

function Sync-RouterCompositeRoutes {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$GroupId,
        [Parameter(Mandatory)][object[]]$CompositeRoutes
    )

    $desired = @{}
    foreach ($route in @($CompositeRoutes)) {
        $publicModelId = ([string]$route.PublicModelId).Trim()
        $upstreamModelId = ([string]$route.UpstreamModelId).Trim()
        $targetPlatform = ([string]$route.TargetPlatform).Trim().ToLowerInvariant()
        if ([string]::IsNullOrWhiteSpace($publicModelId) -or
            [string]::IsNullOrWhiteSpace($targetPlatform)) {
            continue
        }
        if ([string]::IsNullOrWhiteSpace($upstreamModelId)) {
            $upstreamModelId = $publicModelId
        }
        $routeKey = $publicModelId.ToLowerInvariant() + '|' + $targetPlatform
        $priorityProperty = $route.PSObject.Properties['Priority']
        $desired[$routeKey] = [pscustomobject][ordered]@{
            PublicModelId = $publicModelId
            UpstreamModelId = $upstreamModelId
            TargetPlatform = $targetPlatform
            Priority = if ($null -eq $priorityProperty) { 100 } else { [int]$priorityProperty.Value }
        }
    }

    $existing = @()
    try {
        $existingData = Get-RouterResponseData (Invoke-RouterApi `
            -Session $Session `
            -Method GET `
            -Path "/api/v1/admin/groups/$GroupId/composite-routes" `
            -TimeoutSec 15)
        if ($null -eq $existingData) {
            $existing = @()
        } elseif ($existingData -is [System.Array]) {
            $existing = @($existingData)
        } elseif ($null -ne $existingData.PSObject.Properties['items']) {
            $existing = @($existingData.items)
        } elseif ($null -ne $existingData.PSObject.Properties['id']) {
            $existing = @($existingData)
        } else {
            $existing = @($existingData)
        }
    } catch {
        Write-Warning "Could not list composite routes for group ${GroupId}: $($_.Exception.Message)"
        $existing = @()
    }

    $existingByTarget = @{}
    foreach ($route in $existing) {
        $publicModelId = ''
        $publicProperty = $route.PSObject.Properties['public_model']
        if ($null -ne $publicProperty) { $publicModelId = ([string]$publicProperty.Value).Trim() }
        if ([string]::IsNullOrWhiteSpace($publicModelId)) { continue }
        $existingPlatform = ([string]$route.target_platform).Trim().ToLowerInvariant()
        $routeKey = $publicModelId.ToLowerInvariant() + '|' + $existingPlatform
        if (-not $existingByTarget.ContainsKey($routeKey)) {
            $existingByTarget[$routeKey] = [System.Collections.ArrayList]::new()
        }
        [void]$existingByTarget[$routeKey].Add($route)
    }

    $created = 0
    $updated = 0
    $removed = 0
    foreach ($routeKey in @($desired.Keys)) {
        $want = $desired[$routeKey]
        $matches = if ($existingByTarget.ContainsKey($routeKey)) {
            @($existingByTarget[$routeKey])
        } else {
            @()
        }
        $primary = $matches | Select-Object -First 1
        $body = @{
            public_model = [string]$want.PublicModelId
            upstream_model = [string]$want.UpstreamModelId
            target_platform = [string]$want.TargetPlatform
            match_type = 'exact'
            endpoint = 'any'
            priority = [int]$want.Priority
            enabled = $true
        }
        if ($null -eq $primary) {
            [void](Invoke-RouterApi `
                -Session $Session `
                -Method POST `
                -Path "/api/v1/admin/groups/$GroupId/composite-routes" `
                -Body $body)
            $created++
        } else {
            $routeId = [long]$primary.id
            $currentUpstream = ''
            $currentPlatform = ''
            $currentMatch = ''
            $currentEnabled = $true
            $currentPriority = 0
            $upstreamProperty = $primary.PSObject.Properties['upstream_model']
            if ($null -ne $upstreamProperty) { $currentUpstream = [string]$upstreamProperty.Value }
            $platformProperty = $primary.PSObject.Properties['target_platform']
            if ($null -ne $platformProperty) { $currentPlatform = [string]$platformProperty.Value }
            $matchProperty = $primary.PSObject.Properties['match_type']
            if ($null -ne $matchProperty) { $currentMatch = [string]$matchProperty.Value }
            $enabledProperty = $primary.PSObject.Properties['enabled']
            if ($null -ne $enabledProperty) { $currentEnabled = [bool]$enabledProperty.Value }
            $priorityProperty = $primary.PSObject.Properties['priority']
            if ($null -ne $priorityProperty) { $currentPriority = [int]$priorityProperty.Value }
            $needsUpdate = -not [string]::Equals($currentUpstream, [string]$want.UpstreamModelId, [StringComparison]::OrdinalIgnoreCase) -or
                -not [string]::Equals($currentPlatform, [string]$want.TargetPlatform, [StringComparison]::OrdinalIgnoreCase) -or
                -not [string]::Equals($currentMatch, 'exact', [StringComparison]::OrdinalIgnoreCase) -or
                $currentPriority -ne [int]$want.Priority -or
                -not $currentEnabled
            if ($needsUpdate) {
                [void](Invoke-RouterApi `
                    -Session $Session `
                    -Method PUT `
                    -Path "/api/v1/admin/groups/$GroupId/composite-routes/$routeId" `
                    -Body $body)
                $updated++
            }
            foreach ($duplicate in @($matches | Select-Object -Skip 1)) {
                try {
                    [void](Invoke-RouterApi `
                        -Session $Session `
                        -Method DELETE `
                        -Path "/api/v1/admin/groups/$GroupId/composite-routes/$([long]$duplicate.id)")
                    $removed++
                } catch {
                    Write-Warning "Could not remove duplicate composite route $([long]$duplicate.id): $($_.Exception.Message)"
                }
            }
        }
    }

    foreach ($routeKey in @($existingByTarget.Keys)) {
        if ($desired.ContainsKey($routeKey)) { continue }
        foreach ($route in @($existingByTarget[$routeKey])) {
            try {
                [void](Invoke-RouterApi `
                    -Session $Session `
                    -Method DELETE `
                    -Path "/api/v1/admin/groups/$GroupId/composite-routes/$([long]$route.id)")
                $removed++
            } catch {
                Write-Warning "Could not remove stale composite route $([long]$route.id): $($_.Exception.Message)"
            }
        }
    }

    return [pscustomobject][ordered]@{
        Created = $created
        Updated = $updated
        Removed = $removed
        Desired = $desired.Count
    }
}

function Set-RouterAccountProxy {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId,
        [Parameter(Mandatory)][ValidateRange(0, [long]::MaxValue)][long]$ProxyId
    )

    [void](Invoke-RouterApi `
        -Session $Session `
        -Method PUT `
        -Path "/api/v1/admin/accounts/$AccountId" `
        -Body @{ proxy_id = $ProxyId })
}

function Get-RouterOpenAIChannelPolicy {
    param(
        [Parameter(Mandatory)][string]$BaseUrl,
        [AllowNull()]$Extra,
        [AllowEmptyString()][string]$ModelId = ''
    )

    $effectiveExtra = [ordered]@{}
    if ($null -ne $Extra) {
        if ($Extra -is [Collections.IDictionary]) {
            foreach ($key in $Extra.Keys) { $effectiveExtra[[string]$key] = $Extra[$key] }
        } else {
            foreach ($property in $Extra.PSObject.Properties) {
                $effectiveExtra[$property.Name] = $property.Value
            }
        }
    }

    $uri = $null
    $isOfficialOpenAI = [Uri]::TryCreate($BaseUrl, [UriKind]::Absolute, [ref]$uri) -and
        [string]::Equals($uri.Host, 'api.openai.com', [StringComparison]::OrdinalIgnoreCase)
    $hostName = if ($null -ne $uri) { $uri.Host.ToLowerInvariant() } else { '' }
    $mode = if ($effectiveExtra.Contains('openai_responses_mode')) {
        ([string]$effectiveExtra.openai_responses_mode).Trim().ToLowerInvariant()
    } else { '' }

    # Sub2API performs a one-time function-call probe when an API-key account is
    # created or updated. Chiral documents a native Codex Responses endpoint, so
    # keep it on that path even if a transient probe fails. Other unknown hosts
    # remain automatic and explicit account settings always win.
    if ([string]::IsNullOrWhiteSpace($mode) -and -not $isOfficialOpenAI) {
        $normalizedOpenRouterModelId = $ModelId.Trim().TrimStart('~')
        $openRouterChatBridgeRequired = $hostName -eq 'openrouter.ai' -and
            -not [string]::IsNullOrWhiteSpace($normalizedOpenRouterModelId) -and
            $normalizedOpenRouterModelId -notmatch '^(?i)deepseek/'
        $mode = if ($hostName -eq 'api.430123.xyz') {
            'force_responses'
        } elseif ($openRouterChatBridgeRequired) {
            # OpenRouter's non-DeepSeek providers reject Codex custom tools on
            # the native Responses path. The compatibility bridge preserves
            # those tools by translating them to Chat Completions functions.
            'force_chat_completions'
        } elseif ($hostName -in @(
            'api.kimi.com',
            'api.moonshot.ai',
            'api.moonshot.cn',
            'ark.cn-beijing.volces.com'
        )) {
            'force_chat_completions'
        } else {
            'auto'
        }
        $effectiveExtra.openai_responses_mode = $mode
    }

    # Compact is a separate endpoint and cannot be inferred from /responses.
    # Seed only observations verified against public provider hosts; explicit
    # user/account metadata remains authoritative and unknown hosts stay auto.
    if (-not $effectiveExtra.Contains('openai_compact_supported') -and $null -ne $uri) {
        switch ($hostName) {
            'api.430123.xyz' { $effectiveExtra.openai_compact_supported = $true }
            'openrouter.ai' { $effectiveExtra.openai_compact_supported = $false }
            'api.kimi.com' { $effectiveExtra.openai_compact_supported = $false }
            'api.moonshot.ai' { $effectiveExtra.openai_compact_supported = $false }
            'api.moonshot.cn' { $effectiveExtra.openai_compact_supported = $false }
            'ark.cn-beijing.volces.com' { $effectiveExtra.openai_compact_supported = $false }
        }
    }

    $capabilities = @()
    $supported = $null
    if ($effectiveExtra.Contains('openai_responses_supported')) {
        $supported = $effectiveExtra.openai_responses_supported
    }
    if ($mode -eq 'force_chat_completions' -or $supported -eq $false) {
        $capabilities = @('chat_completions')
    }

    return [pscustomobject][ordered]@{
        Extra = $effectiveExtra
        OpenAICapabilities = $capabilities
        ResponsesMode = $mode
        IsOfficialOpenAI = $isOfficialOpenAI
    }
}

function Sync-RouterManagedProxy {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)]$ProxySettings,
        [scriptblock]$RequestInvoker
    )

    $managedName = 'Codex-Router / Auto-detected outbound proxy'
    $invokeRequest = {
        param([string]$Method, [string]$Path, [AllowNull()]$Body)
        if ($null -ne $RequestInvoker) {
            return & $RequestInvoker $Session $Method $Path $Body
        }
        if ($null -eq $Body) {
            return Invoke-RouterApi -Session $Session -Method $Method -Path $Path
        }
        return Invoke-RouterApi -Session $Session -Method $Method -Path $Path -Body $Body
    }

    $search = [Uri]::EscapeDataString($managedName)
    $response = & $invokeRequest 'GET' "/api/v1/admin/proxies?page=1&page_size=20&search=$search" $null
    $data = Get-RouterResponseData -Response $response
    $items = if ($null -ne $data -and $null -ne $data.PSObject.Properties['items']) {
        @($data.items)
    } else {
        @($data)
    }
    $managed = @($items | Where-Object { [string]$_.name -ceq $managedName })
    if ($managed.Count -gt 1) {
        throw 'More than one Router-managed outbound proxy exists; remove the duplicate in Sub2API before applying this configuration.'
    }
    $existing = $managed | Select-Object -First 1
    $existingId = if ($null -eq $existing) { 0L } else { [long]$existing.id }

    $mode = [string]$ProxySettings.Mode
    if ($mode -eq 'unsupported') {
        $reason = [string]$ProxySettings.Diagnostic
        if ([string]::IsNullOrWhiteSpace($reason)) {
            $reason = 'The detected proxy mode cannot be represented by the bundled Sub2API proxy API.'
        }
        throw "ROUTER_PROXY_UNSUPPORTED: $reason"
    }
    $proxyUrl = [string]$ProxySettings.ProxyUrl
    if ([string]::IsNullOrWhiteSpace($proxyUrl)) {
        return [pscustomobject][ordered]@{
            ManagedProxyId = $existingId
            DesiredProxyId = 0L
            Action = 'direct'
            Source = [string]$ProxySettings.Source
        }
    }

    $uri = [Uri]$proxyUrl
    if (-not [string]::IsNullOrWhiteSpace($uri.UserInfo)) {
        throw 'ROUTER_PROXY_CREDENTIAL_STORAGE_UNSUPPORTED: The detected proxy uses authentication. The bundled Sub2API stores proxy passwords in its database, so Codex-Router will not copy this credential out of Windows Credential Manager or the process environment.'
    }
    if ($uri.Scheme.ToLowerInvariant() -notin @('http', 'https', 'socks5', 'socks5h')) {
        throw 'ROUTER_PROXY_UNSUPPORTED: The detected proxy protocol is not supported by Sub2API.'
    }
    $existingUsername = if ($null -ne $existing -and $null -ne $existing.PSObject.Properties['username']) {
        [string]$existing.username
    } else { '' }
    $existingPassword = if ($null -ne $existing -and $null -ne $existing.PSObject.Properties['password']) {
        [string]$existing.password
    } else { '' }
    if ($null -ne $existing -and
        (-not [string]::IsNullOrWhiteSpace($existingUsername) -or
         -not [string]::IsNullOrWhiteSpace($existingPassword))) {
        throw 'ROUTER_PROXY_MANAGED_RESOURCE_CONFLICT: The Router-managed proxy record contains credentials and will not be overwritten automatically.'
    }

    $host = $uri.Host.TrimStart('[').TrimEnd(']')
    $body = [ordered]@{
        name = $managedName
        protocol = $uri.Scheme.ToLowerInvariant()
        host = $host
        port = $uri.Port
        fallback_mode = 'none'
        expiry_warn_days = 0
    }
    if ($null -eq $existing) {
        $created = Get-RouterResponseData -Response (& $invokeRequest 'POST' '/api/v1/admin/proxies' $body)
        return [pscustomobject][ordered]@{
            ManagedProxyId = [long]$created.id
            DesiredProxyId = [long]$created.id
            Action = 'created'
            Source = [string]$ProxySettings.Source
        }
    }

    $changed =
        -not [string]::Equals([string]$existing.protocol, [string]$body.protocol, [StringComparison]::OrdinalIgnoreCase) -or
        -not [string]::Equals([string]$existing.host, [string]$body.host, [StringComparison]::OrdinalIgnoreCase) -or
        [int]$existing.port -ne [int]$body.port -or
        [string]$existing.fallback_mode -ne 'none'
    if ($changed) {
        [void](& $invokeRequest 'PUT' "/api/v1/admin/proxies/$existingId" $body)
    }
    return [pscustomobject][ordered]@{
        ManagedProxyId = $existingId
        DesiredProxyId = $existingId
        Action = if ($changed) { 'updated' } else { 'reused' }
        Source = [string]$ProxySettings.Source
    }
}

function Get-RouterAccountProxyReconciliation {
    param(
        [AllowNull()]$CurrentProxyId,
        [AllowNull()][long[]]$RouterManagedProxyIds,
        [Parameter(Mandatory)][ValidateRange(0, [long]::MaxValue)][long]$DesiredProxyId,
        [Parameter(Mandatory)][bool]$ShouldUseManagedProxy
    )

    $current = if ($null -eq $CurrentProxyId) { 0L } else { [long]$CurrentProxyId }
    $managedIds = @($RouterManagedProxyIds | Where-Object { $_ -gt 0 } | Select-Object -Unique)
    $isRouterManaged = $current -gt 0 -and $current -in $managedIds

    if ($ShouldUseManagedProxy) {
        if ($current -eq 0 -and $DesiredProxyId -gt 0) {
            return [pscustomobject]@{ Action = 'assign'; ProxyId = $DesiredProxyId }
        }
        if ($isRouterManaged -and $current -ne $DesiredProxyId) {
            return [pscustomobject]@{
                Action = if ($DesiredProxyId -gt 0) { 'replace' } else { 'clear' }
                ProxyId = $DesiredProxyId
            }
        }
        return [pscustomobject]@{
            Action = if ($current -gt 0 -and -not $isRouterManaged) { 'preserve-custom' } else { 'unchanged' }
            ProxyId = $current
        }
    }

    if ($isRouterManaged) {
        return [pscustomobject]@{ Action = 'clear'; ProxyId = 0L }
    }
    return [pscustomobject]@{ Action = 'unchanged'; ProxyId = $current }
}

function Set-RouterScheduledRecovery {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId,
        [Parameter(Mandatory)][string]$ModelId
    )

    $response = Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$AccountId/scheduled-test-plans"
    $plans = @(Get-RouterResponseData -Response $response)
    $plan = $plans | Where-Object {
        $null -ne $_ -and
        $null -ne $_.PSObject.Properties['cron_expression'] -and
        $_.cron_expression -eq '0 * * * *'
    } | Select-Object -First 1
    $body = @{
        account_id = $AccountId
        model_id = $ModelId
        cron_expression = '0 * * * *'
        enabled = $true
        max_results = 48
        auto_recover = $true
    }
    if ($plan) {
        [void](Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/scheduled-test-plans/$($plan.id)" -Body $body)
        return [long]$plan.id
    }

    $created = Invoke-RouterApi -Session $Session -Method POST -Path '/api/v1/admin/scheduled-test-plans' -Body $body
    return [long](Get-RouterResponseData -Response $created).id
}

function Disable-RouterScheduledRecoveryPlans {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][long]$AccountId
    )

    $response = Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$AccountId/scheduled-test-plans"
    $plans = @(Get-RouterResponseData -Response $response)
    $disabled = 0
    foreach ($plan in @($plans | Where-Object {
        $null -ne $_ -and
        $_.cron_expression -eq '0 * * * *' -and
        [bool]$_.auto_recover -and
        [bool]$_.enabled
    })) {
        [void](Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/scheduled-test-plans/$($plan.id)" -Body @{
            model_id = [string]$plan.model_id
            cron_expression = [string]$plan.cron_expression
            enabled = $false
            max_results = [int]$plan.max_results
            auto_recover = $true
        })
        $disabled++
    }
    return $disabled
}

Export-ModuleMember -Function `
    Get-RouterBaseUri, `
    New-RouterAdminSession, `
    Get-RouterResponseData, `
    Invoke-RouterApi, `
    Get-RouterGroups, `
    Get-RouterAccounts, `
    ConvertTo-RouterResetAtUtc, `
    Get-RouterOAuthRecoveryState, `
    Set-RouterAccountGroupMembership, `
    Get-RouterOAuthRoutingPriorities, `
    Get-RouterCanonicalModelId, `
    Get-RouterModelIdentity, `
    Test-RouterSameModel, `
    Test-RouterCodingPlanChannel, `
    Get-RouterChannelTier, `
    Get-RouterRecommendedDisplayName, `
    Test-RouterModelAliasCustomized, `
    Get-RouterModelDisplayName, `
    Get-RouterModelSource, `
    Get-RouterDiscoveredOAuthModelsForAccount, `
    Get-RouterModelRoutePlan, `
    Get-RouterDefaultPublicModelId, `
    Get-RouterFallbackChannelKey, `
    Test-RouterFallbackChannelSelected, `
    Get-RouterEffectiveApiPriority, `
    Get-RouterOAuthModelSuggestions, `
    Get-RouterAccountPlatformMap, `
    Get-RouterOpenRouterUpstreamModelId, `
    Get-RouterUpstreamModelId, `
    Get-RouterServableCatalogRoutes, `
    Get-RouterServableRoutingRoutes, `
    Get-RouterCompositeTargetPlatform, `
    Get-RouterCompositeRoutePlan, `
    Sync-RouterCompositeRoutes, `
    Set-RouterAccountProxy, `
    Sync-RouterManagedProxy, `
    Get-RouterOpenAIChannelPolicy, `
    Get-RouterAccountProxyReconciliation, `
    Set-RouterScheduledRecovery, `
    Disable-RouterScheduledRecoveryPlans
