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
        $response = [Net.HttpWebResponse]$request.GetResponse()
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

function New-RouterAdminSession {
    $password = Get-RouterCredential -Name 'AdminPassword'
    try {
        $candidates = @(
            @{ email = 'admin@admin.com'; password = $password },
            @{ email = 'admin@admin.com'; password = 'adminadmin' },
            @{ email = 'admin@sub2api.local'; password = $password },
            # Legacy defaults are retained only so upgrades can migrate them.
            @{ email = 'admin@admin.com'; password = 'admin123' },
            @{ email = 'admin@sub2api.local'; password = 'admin123' }
        )
        $token = $null
        $attempted = @{}
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
                $login = $null
            }
        }
        if (-not $token) { throw 'Sub2API admin login returned no access token.' }

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
    # made here; missing reset data falls back to one recovery probe per hour.
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
        $reason -match '(?i)quota|usage limit|rate.?limit|billing cycle|exhaust'

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
            NextCheckSeconds = 3600L
            ResetAt = if ($null -eq $resetAt) { '' } else { $resetAt.UtcDateTime.ToString('o') }
            Reason = if ($reason) { $reason } elseif ($resetReached) { 'quota reset time reached' } else { 'quota exhausted without a reset time' }
        }
    }

    return [pscustomobject][ordered]@{
        Action = 'healthy'
        ShouldIsolate = $false
        NextCheckSeconds = 3600L
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
        '^deepseek-v4-(pro|flash)' { return 'DeepSeek-V4-Flash' }
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
        $automaticNames += @('DeepSeek V4 Pro', 'DeepSeek-V4-Pro')
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
                Selected = $selected
                Discovered = $false
            }
        }
    )
    $selectedOAuth = @($descriptors | Where-Object { $_.Source -eq 'oauth' -and $_.Selected })
    if ($oauthSelectionInitialized -and $fallbackEnabled) {
        foreach ($accountId in $oauthAccountIds) {
            $hasExplicitModelsForAccount = @($selectedOAuth | Where-Object {
                [long]$_.Model.oauthAccountId -eq $accountId
            }).Count -gt 0
            if ($hasExplicitModelsForAccount) { continue }

            foreach ($modelId in @(Get-RouterDiscoveredOAuthModelsForAccount `
                -DiscoveredOAuthModelsByAccount $DiscoveredOAuthModelsByAccount `
                -AccountId $accountId)) {
                $selectedOAuth += [pscustomobject][ordered]@{
                    Index = -1
                    Model = [pscustomobject][ordered]@{
                        model = $modelId
                        source = 'oauth'
                        oauthAccountId = $accountId
                    }
                    ModelId = $modelId
                    Source = 'oauth'
                    CanonicalModelId = Get-RouterCanonicalModelId -ModelId $modelId
                    Selected = $true
                    Discovered = $true
                }
            }
        }
    }
    $catalogIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $plan = @(
        foreach ($descriptor in $descriptors) {
            $matchingOAuth = @($selectedOAuth | Where-Object {
                $_.CanonicalModelId -eq $descriptor.CanonicalModelId
            })
            $isOAuth = $descriptor.Source -eq 'oauth'
            $matchingSelectedApiFallbacks = if ($isOAuth -and $fallbackEnabled) {
                @($descriptors | Where-Object {
                    $_.Source -ne 'oauth' -and
                    $_.CanonicalModelId -eq $descriptor.CanonicalModelId -and
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
                $hasExplicitMatchingOAuth = @($matchingOAuth | Where-Object {
                    -not [bool]$_.Discovered
                }).Count -gt 0
                if ($hasExplicitMatchingOAuth) {
                    $includeInCatalog = $false
                } else {
                    # With an implicit OAuth binding there is no OAuth model row
                    # to represent the merged route in the Codex catalog. Reuse
                    # the first API row's metadata while exposing the OAuth ID.
                    $publicModelId = [string]$matchingOAuth[0].ModelId
                    $includeInCatalog = $catalogIds.Add($publicModelId)
                }
                if ($isFallback) {
                    $requestModelIds = @($matchingOAuth | ForEach-Object { $_.ModelId } | Select-Object -Unique)
                }
            } elseif (-not $fallbackEnabled) {
                $sameCanonicalCount = @($descriptors | Where-Object {
                    $_.Selected -and $_.CanonicalModelId -eq $descriptor.CanonicalModelId
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
                [pscustomobject][ordered]@{ id = 'gemini-3.6-flash'; displayName = 'Gemini-3.6-Flash' }
            )
        }
        default { return @() }
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
        [AllowNull()]$Extra
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
        $mode = if ($hostName -eq 'api.430123.xyz') {
            'force_responses'
        } elseif ($hostName -in @(
            'api.kimi.com',
            'api.moonshot.ai',
            'api.moonshot.cn'
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
    Set-RouterAccountProxy, `
    Sync-RouterManagedProxy, `
    Get-RouterOpenAIChannelPolicy, `
    Get-RouterAccountProxyReconciliation, `
    Set-RouterScheduledRecovery, `
    Disable-RouterScheduledRecoveryPlans
