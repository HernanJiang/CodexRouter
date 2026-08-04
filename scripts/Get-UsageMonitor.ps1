param(
    [string]$ProfileName = 'Codex-Router',
    [string]$ConfigPath = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$utf8NoBom = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $utf8NoBom
$OutputEncoding = $utf8NoBom

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1')
Import-Module (Join-Path $PSScriptRoot 'UserData.psm1') -Force

function Get-SafeValue {
    param([AllowNull()]$InputObject, [Parameter(Mandatory)][string]$Name)
    if ($null -eq $InputObject) { return $null }
    if ($InputObject -is [System.Collections.IDictionary]) {
        if ($InputObject.Contains($Name)) { return $InputObject[$Name] }
        return $null
    }
    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Get-SafeString {
    param([AllowNull()]$InputObject, [Parameter(Mandatory)][string]$Name)
    $value = Get-SafeValue -InputObject $InputObject -Name $Name
    if ($null -eq $value) { return '' }
    return [string]$value
}

function Get-SafeNumber {
    param([AllowNull()]$InputObject, [Parameter(Mandatory)][string]$Name)
    $value = Get-SafeValue -InputObject $InputObject -Name $Name
    if ($null -eq $value) { return 0 }
    return [double]$value
}

function Invoke-LocalAdminRead {
    param([Parameter(Mandatory)][scriptblock]$Operation)
    $delays = @(0, 250, 750)
    for ($attempt = 0; $attempt -lt $delays.Count; $attempt++) {
        if ($delays[$attempt] -gt 0) { Start-Sleep -Milliseconds $delays[$attempt] }
        try { return & $Operation } catch {
            if ($attempt -eq $delays.Count - 1) { throw }
        }
    }
}

function ConvertTo-IsoFromUnixSeconds {
    param([AllowNull()]$Value)
    if ($null -eq $Value) { return '' }
    try { return [DateTimeOffset]::FromUnixTimeSeconds([long]$Value).UtcDateTime.ToString('o') } catch { return '' }
}

function ConvertTo-CodingPlanResetAt {
    param([AllowNull()]$Value)
    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) { return '' }
    if ($Value -is [DateTimeOffset]) { return $Value.ToUniversalTime().UtcDateTime.ToString('o') }
    if ($Value -is [DateTime]) {
        $dateTime = [DateTime]$Value
        if ($dateTime.Kind -eq [DateTimeKind]::Unspecified) {
            $dateTime = [DateTime]::SpecifyKind($dateTime, [DateTimeKind]::Utc)
        }
        return $dateTime.ToUniversalTime().ToString('o')
    }
    if ($Value -is [string] -and -not ([string]$Value -match '^\d+$')) { return [string]$Value }
    try {
        $timestamp = [long]$Value
        if ($timestamp -le 0) { return '' }
        if ($timestamp -ge 1000000000000L) {
            return [DateTimeOffset]::FromUnixTimeMilliseconds($timestamp).UtcDateTime.ToString('o')
        }
        return [DateTimeOffset]::FromUnixTimeSeconds($timestamp).UtcDateTime.ToString('o')
    } catch { return '' }
}

function New-CodingPlanWindow {
    param(
        [Parameter(Mandatory)][string]$Kind,
        [Parameter(Mandatory)][double]$UsedPercent,
        [AllowNull()]$ResetAt,
        [string]$DisplayName = ''
    )
    return [ordered]@{
        kind = $Kind
        displayName = $DisplayName
        usedPercent = [Math]::Max(0.0, [Math]::Min(100.0, $UsedPercent))
        resetAt = ConvertTo-CodingPlanResetAt -Value $ResetAt
        remainingSeconds = -1
        requests = 0
        tokens = 0
    }
}

function ConvertFrom-KimiCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $windows = @()
    foreach ($item in @(Get-SafeValue -InputObject $Body -Name 'limits')) {
        $detail = Get-SafeValue -InputObject $item -Name 'detail'
        if ($null -eq $detail) { continue }
        $limit = Get-SafeNumber -InputObject $detail -Name 'limit'
        $remaining = Get-SafeNumber -InputObject $detail -Name 'remaining'
        if ($limit -le 0) { continue }
        $windows += New-CodingPlanWindow -Kind 'fiveHour' `
            -UsedPercent ((($limit - $remaining) / $limit) * 100.0) `
            -ResetAt (Get-SafeValue -InputObject $detail -Name 'resetTime')
    }
    $usage = Get-SafeValue -InputObject $Body -Name 'usage'
    if ($null -ne $usage) {
        $limit = Get-SafeNumber -InputObject $usage -Name 'limit'
        $remaining = Get-SafeNumber -InputObject $usage -Name 'remaining'
        if ($limit -gt 0) {
            $windows += New-CodingPlanWindow -Kind 'weekly' `
                -UsedPercent ((($limit - $remaining) / $limit) * 100.0) `
                -ResetAt (Get-SafeValue -InputObject $usage -Name 'resetTime')
        }
    }
    return @($windows)
}

function ConvertFrom-ZhipuCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    $classified = @{}
    $unclassified = @()
    foreach ($item in @(Get-SafeValue -InputObject $data -Name 'limits')) {
        if ((Get-SafeString -InputObject $item -Name 'type') -notmatch '^(?i)TOKENS_LIMIT$') { continue }
        $unit = [long](Get-SafeNumber -InputObject $item -Name 'unit')
        $kind = if ($unit -eq 3) { 'fiveHour' } elseif ($unit -eq 6) { 'weekly' } else { '' }
        $window = New-CodingPlanWindow `
            -Kind $(if ($kind) { $kind } else { 'other' }) `
            -UsedPercent (Get-SafeNumber -InputObject $item -Name 'percentage') `
            -ResetAt (Get-SafeValue -InputObject $item -Name 'nextResetTime')
        if ($kind -and -not $classified.ContainsKey($kind)) { $classified[$kind] = $window }
        elseif (-not $kind) { $unclassified += $window }
    }
    foreach ($window in @($unclassified | Sort-Object { if ($_.resetAt) { $_.resetAt } else { '' } })) {
        $kind = if (-not $classified.ContainsKey('fiveHour')) { 'fiveHour' } elseif (-not $classified.ContainsKey('weekly')) { 'weekly' } else { '' }
        if (-not $kind) { break }
        $window.kind = $kind
        $classified[$kind] = $window
    }
    $result = @()
    foreach ($kind in @('fiveHour', 'weekly')) {
        if ($classified.ContainsKey($kind)) { $result += $classified[$kind] }
    }
    return @($result)
}

function ConvertFrom-MiniMaxCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $item = @(Get-SafeValue -InputObject $Body -Name 'model_remains' | Where-Object {
        (Get-SafeString -InputObject $_ -Name 'model_name') -eq 'general'
    } | Select-Object -First 1)
    if ($item.Count -eq 0) { return @() }
    $record = $item[0]
    $windows = @()
    $remaining = Get-SafeValue -InputObject $record -Name 'current_interval_remaining_percent'
    if ($null -ne $remaining) {
        $windows += New-CodingPlanWindow -Kind 'fiveHour' -UsedPercent (100.0 - [double]$remaining) `
            -ResetAt (Get-SafeValue -InputObject $record -Name 'end_time')
    }
    if ([long](Get-SafeNumber -InputObject $record -Name 'current_weekly_status') -eq 1) {
        $weeklyRemaining = Get-SafeValue -InputObject $record -Name 'current_weekly_remaining_percent'
        if ($null -ne $weeklyRemaining) {
            $windows += New-CodingPlanWindow -Kind 'weekly' -UsedPercent (100.0 - [double]$weeklyRemaining) `
                -ResetAt (Get-SafeValue -InputObject $record -Name 'weekly_end_time')
        }
    }
    return @($windows)
}

function ConvertFrom-ZenMuxCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    if ((Get-SafeValue -InputObject $Body -Name 'success') -ne $true) { return @() }
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    $windows = @()
    foreach ($definition in @(
        @{ Name = 'quota_5_hour'; Kind = 'fiveHour' },
        @{ Name = 'quota_7_day'; Kind = 'weekly' }
    )) {
        $quota = Get-SafeValue -InputObject $data -Name $definition.Name
        if ($null -eq $quota) { continue }
        $ratio = Get-SafeValue -InputObject $quota -Name 'usage_percentage'
        if ($null -eq $ratio) { continue }
        $usedPercent = [double]$ratio
        if ($usedPercent -le 1.0) { $usedPercent *= 100.0 }
        $windows += New-CodingPlanWindow -Kind $definition.Kind -UsedPercent $usedPercent `
            -ResetAt (Get-SafeValue -InputObject $quota -Name 'resets_at')
    }
    return @($windows)
}

function Get-CodingPlanUsage {
    param([Parameter(Mandatory)]$Channel)
    $baseUrl = (Get-SafeString -InputObject $Channel -Name 'baseUrl').TrimEnd('/')
    $credentialName = Get-SafeString -InputObject $Channel -Name 'credentialName'
    $lower = $baseUrl.ToLowerInvariant()
    $provider = if ($lower.Contains('api.kimi.com/coding')) { 'Kimi Coding Plan' }
        elseif ($lower.Contains('open.bigmodel.cn') -or $lower.Contains('bigmodel.cn') -or $lower.Contains('api.z.ai')) { 'Zhipu GLM Coding Plan' }
        elseif ($lower.Contains('api.minimaxi.com') -or $lower.Contains('api.minimax.io')) { 'MiniMax Coding Plan' }
        elseif ($lower.Contains('zenmux')) { 'ZenMux Coding Plan' }
        elseif ($lower.Contains('volces.com/api/coding')) { 'Volcengine Coding Plan' }
        else { '' }
    if (-not $provider) { return $null }
    if ($provider -eq 'Volcengine Coding Plan') {
        return [pscustomobject]@{ provider = $provider; windows = @(); note = 'Volcengine quota requires separate control-plane AK/SK credentials; the inference API key cannot query it.' }
    }
    $apiKey = Get-RouterCredential -Name $credentialName -AllowMissing
    if ([string]::IsNullOrWhiteSpace($apiKey)) {
        return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider credential is unavailable." }
    }
    try {
        $headers = @{ Accept = 'application/json' }
        if ($provider -eq 'Zhipu GLM Coding Plan') { $headers.Authorization = $apiKey }
        else { $headers.Authorization = "Bearer $apiKey" }
        $uri = switch ($provider) {
            'Kimi Coding Plan' { 'https://api.kimi.com/coding/v1/usages' }
            'Zhipu GLM Coding Plan' {
                if ($lower.Contains('bigmodel.cn')) { 'https://open.bigmodel.cn/api/monitor/usage/quota/limit' }
                else { 'https://api.z.ai/api/monitor/usage/quota/limit' }
            }
            'MiniMax Coding Plan' {
                if ($lower.Contains('api.minimaxi.com')) { 'https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains' }
                else { 'https://api.minimax.io/v1/api/openplatform/coding_plan/remains' }
            }
            'ZenMux Coding Plan' { $baseUrl }
        }
        $body = Invoke-RestMethod -Method GET -Uri $uri -Headers $headers -TimeoutSec 15
        $windows = switch ($provider) {
            'Kimi Coding Plan' { ConvertFrom-KimiCodingPlanUsage -Body $body }
            'Zhipu GLM Coding Plan' { ConvertFrom-ZhipuCodingPlanUsage -Body $body }
            'MiniMax Coding Plan' { ConvertFrom-MiniMaxCodingPlanUsage -Body $body }
            'ZenMux Coding Plan' { ConvertFrom-ZenMuxCodingPlanUsage -Body $body }
        }
        return [pscustomobject]@{ provider = $provider; windows = @($windows); note = "$provider quota queried directly from the provider." }
    } catch {
        $status = 'transport error'
        $response = $_.Exception.PSObject.Properties['Response']
        if ($null -ne $response -and $null -ne $response.Value) {
            $statusCode = $response.Value.PSObject.Properties['StatusCode']
            if ($null -ne $statusCode) { $status = "HTTP $([int]$statusCode.Value)" }
        }
        return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider quota query failed ($status)." }
    } finally {
        $apiKey = $null
        if ($headers) { $headers.Clear() }
    }
}

function Get-WindowLabel {
    param([long]$Seconds, [string]$Fallback)
    if ($Seconds -gt 0 -and $Seconds -le 21600) { return 'fiveHour' }
    if ($Seconds -le 691200 -and $Seconds -gt 21600) { return 'weekly' }
    if ($Seconds -gt 691200) { return 'monthly' }
    return $Fallback
}

function Convert-Stats {
    param([AllowNull()]$Stats)
    $summary = Get-SafeValue -InputObject $Stats -Name 'summary'
    $models = @()
    foreach ($model in @(Get-SafeValue -InputObject $Stats -Name 'models')) {
        if ($null -eq $model) { continue }
        $models += [ordered]@{
            name = Get-SafeString -InputObject $model -Name 'model'
            requests = [long](Get-SafeNumber -InputObject $model -Name 'requests')
            inputTokens = [long](Get-SafeNumber -InputObject $model -Name 'input_tokens')
            outputTokens = [long](Get-SafeNumber -InputObject $model -Name 'output_tokens')
            cacheReadTokens = [long](Get-SafeNumber -InputObject $model -Name 'cache_read_tokens')
            cacheCreationTokens = [long](Get-SafeNumber -InputObject $model -Name 'cache_creation_tokens')
            totalTokens = [long](Get-SafeNumber -InputObject $model -Name 'total_tokens')
            cost = Get-SafeNumber -InputObject $model -Name 'actual_cost'
        }
    }
    return [ordered]@{
        requests = [long](Get-SafeNumber -InputObject $summary -Name 'total_requests')
        totalTokens = [long](Get-SafeNumber -InputObject $summary -Name 'total_tokens')
        cost = Get-SafeNumber -InputObject $summary -Name 'total_cost'
        models = $models
    }
}

function Add-UsageWindow {
    param(
        [System.Collections.ArrayList]$Target,
        [string]$Kind,
        [AllowNull()]$Window,
        [string]$DisplayName = ''
    )
    if ($null -eq $Window) { return }
    $utilization = Get-SafeValue -InputObject $Window -Name 'utilization'
    if ($null -eq $utilization) { $utilization = Get-SafeValue -InputObject $Window -Name 'used_percent' }
    if ($null -eq $utilization) {
        $remainingPercent = Get-SafeValue -InputObject $Window -Name 'remaining_percent'
        if ($null -ne $remainingPercent) { $utilization = 100.0 - [double]$remainingPercent }
    }
    if ($null -eq $utilization) { return }
    try { $usedPercent = [double]$utilization } catch { return }
    if ([double]::IsNaN($usedPercent) -or [double]::IsInfinity($usedPercent)) { return }
    $usedPercent = [Math]::Max(0.0, [Math]::Min(100.0, $usedPercent))
    $resetValue = Get-SafeValue -InputObject $Window -Name 'resets_at'
    if ($null -eq $resetValue) { $resetValue = Get-SafeValue -InputObject $Window -Name 'reset_time' }
    if ($null -eq $resetValue) { $resetValue = Get-SafeValue -InputObject $Window -Name 'reset_at' }
    $resetAt = ConvertTo-CodingPlanResetAt -Value $resetValue
    $remainingValue = Get-SafeValue -InputObject $Window -Name 'remaining_seconds'
    $remaining = if ($null -eq $remainingValue) { -1 } else { [long]$remainingValue }
    $stats = Get-SafeValue -InputObject $Window -Name 'window_stats'
    [void]$Target.Add([ordered]@{
        kind = $Kind
        displayName = $DisplayName
        usedPercent = $usedPercent
        resetAt = $resetAt
        remainingSeconds = $remaining
        requests = [long](Get-SafeNumber -InputObject $stats -Name 'requests')
        tokens = [long](Get-SafeNumber -InputObject $stats -Name 'tokens')
    })
}

function Get-AccountRecord {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)]$Account,
        [Parameter()][AllowEmptyCollection()][string[]]$ConfiguredModels = @(),
        [AllowNull()]$ConfiguredChannel,
        [AllowNull()][hashtable]$CodingPlanCache
    )
    $accountId = [long](Get-SafeValue -InputObject $Account -Name 'id')
    $kind = Get-SafeString -InputObject $Account -Name 'type'
    $platform = Get-SafeString -InputObject $Account -Name 'platform'
    $statsData = $null
    $usageData = $null
    $queryNote = ''
    try {
        $statsData = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$accountId/stats" -TimeoutSec 10)
    } catch {
        $queryNote = $_.Exception.Message
    }
    if ($kind -eq 'oauth') {
        try {
            $usageTimeout = if ($platform -eq 'grok') { 4 } else { 10 }
            $usageData = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$accountId/usage" -TimeoutSec $usageTimeout)
        } catch {
            if (-not $queryNote) {
                $queryNote = if ($platform -eq 'grok') { 'Grok live usage timed out; showing the latest account statistics.' } else { $_.Exception.Message }
            }
        }
    }

    $codingPlan = $null
    if ($kind -eq 'apikey' -and $null -ne $ConfiguredChannel) {
        $cacheKey = (Get-SafeString -InputObject $ConfiguredChannel -Name 'credentialName') + '|' +
            (Get-SafeString -InputObject $ConfiguredChannel -Name 'baseUrl').TrimEnd('/').ToLowerInvariant()
        if ($null -ne $CodingPlanCache -and $CodingPlanCache.ContainsKey($cacheKey)) {
            $codingPlan = $CodingPlanCache[$cacheKey]
        } else {
            $codingPlan = Get-CodingPlanUsage -Channel $ConfiguredChannel
            if ($null -ne $CodingPlanCache -and $null -ne $codingPlan) { $CodingPlanCache[$cacheKey] = $codingPlan }
        }
    }

    $windows = [System.Collections.ArrayList]::new()
    Add-UsageWindow -Target $windows -Kind 'fiveHour' -Window (Get-SafeValue -InputObject $usageData -Name 'five_hour')
    Add-UsageWindow -Target $windows -Kind 'weekly' -Window (Get-SafeValue -InputObject $usageData -Name 'seven_day')
    Add-UsageWindow -Target $windows -Kind 'monthly' -Window (Get-SafeValue -InputObject $usageData -Name 'monthly')

    if ($kind -eq 'oauth' -and $platform -eq 'openai') {
        try {
            $quota = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/openai/accounts/$accountId/quota" -TimeoutSec 10)
            $rateLimit = Get-SafeValue -InputObject $quota -Name 'rate_limit'
            foreach ($windowName in @('primary_window', 'secondary_window')) {
                $quotaWindow = Get-SafeValue -InputObject $rateLimit -Name $windowName
                if ($null -eq $quotaWindow) { continue }
                $windowSeconds = [long](Get-SafeNumber -InputObject $quotaWindow -Name 'limit_window_seconds')
                $windowKind = Get-WindowLabel -Seconds $windowSeconds -Fallback 'other'
                $quotaCandidate = [System.Collections.ArrayList]::new()
                Add-UsageWindow -Target $quotaCandidate -Kind $windowKind -Window $quotaWindow
                if ($quotaCandidate.Count -gt 0) {
                    for ($index = $windows.Count - 1; $index -ge 0; $index--) {
                        if ($windows[$index].kind -eq $windowKind) { $windows.RemoveAt($index) }
                    }
                    [void]$windows.Add($quotaCandidate[0])
                }
            }
        } catch {
            if (-not $queryNote) { $queryNote = 'OpenAI quota refresh unavailable; showing cached usage.' }
        }
    }

    if ($kind -eq 'oauth' -and $platform -eq 'grok') {
        try {
            $quota = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/grok/accounts/$accountId/quota" -TimeoutSec 4)
            foreach ($property in @($quota.PSObject.Properties)) {
                if ($property.Name -notmatch 'hour|week|month|quota|window') { continue }
                Add-UsageWindow -Target $windows -Kind 'other' -Window $property.Value -DisplayName $property.Name
            }
        } catch {
            if (-not $queryNote) { $queryNote = 'Grok live quota timed out; showing the latest passive usage.' }
        }
    }

    if ($kind -eq 'oauth' -and $platform -eq 'antigravity') {
        $quotaMap = Get-SafeValue -InputObject $usageData -Name 'antigravity_quota'
        $detailMap = Get-SafeValue -InputObject $usageData -Name 'antigravity_quota_details'
        foreach ($modelId in $ConfiguredModels) {
            $modelWindow = Get-SafeValue -InputObject $quotaMap -Name $modelId
            if ($null -eq $modelWindow) { continue }
            $detail = Get-SafeValue -InputObject $detailMap -Name $modelId
            $displayName = Get-SafeString -InputObject $detail -Name 'display_name'
            if (-not $displayName) { $displayName = $modelId }
            Add-UsageWindow -Target $windows -Kind 'model' -Window $modelWindow -DisplayName $displayName
        }
    }
    if ($null -ne $codingPlan) {
        foreach ($window in @($codingPlan.windows)) { [void]$windows.Add($window) }
        $queryNote = if ($queryNote) { "$queryNote $($codingPlan.note)" } else { [string]$codingPlan.note }
    }

    $status = Get-SafeString -InputObject $Account -Name 'status'
    $schedulable = Get-SafeValue -InputObject $Account -Name 'schedulable'
    $statusDetail = Get-SafeString -InputObject $Account -Name 'temp_unschedulable_reason'
    if (-not $statusDetail) { $statusDetail = Get-SafeString -InputObject $Account -Name 'error_message' }
    $quotaExhausted = @($windows | Where-Object { $null -ne $_.usedPercent -and [double]$_.usedPercent -ge 99.999 }).Count -gt 0
    $health = if ($quotaExhausted) {
        'quotaExhausted'
    } elseif ($status -eq 'active' -and $schedulable -ne $false) {
        'healthy'
    } elseif ($statusDetail -match 'usage limit|quota|billing cycle') {
        'quotaExhausted'
    } elseif ($statusDetail -or $status -eq 'error') {
        'upstreamError'
    } else {
        'cooldown'
    }
    return [ordered]@{
        id = $accountId
        name = Get-SafeString -InputObject $Account -Name 'name'
        kind = $kind
        platform = $platform
        status = $status
        health = $health
        statusDetail = $statusDetail
        queryNote = $queryNote
        lastUsedAt = Get-SafeString -InputObject $Account -Name 'last_used_at'
        updatedAt = Get-SafeString -InputObject $usageData -Name 'updated_at'
        totals = Convert-Stats -Stats $statsData
        windows = @($windows)
    }
}

$effectiveConfigPath = if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    Get-RouterConfigPath -RouterRoot $routerRoot
} else {
    [IO.Path]::GetFullPath($ConfigPath)
}
$config = if (Test-Path -LiteralPath $effectiveConfigPath) { Get-Content -LiteralPath $effectiveConfigPath -Raw | ConvertFrom-Json } else { $null }
$oauthIds = @()
$apiAccountNames = @{}
$apiChannelsByName = @{}
$modelsByOAuth = @{}
foreach ($model in @(Get-SafeValue -InputObject $config -Name 'models')) {
    if ($null -eq $model) { continue }
    $source = Get-SafeString -InputObject $model -Name 'source'
    $modelId = Get-SafeString -InputObject $model -Name 'model'
    if ($source -eq 'oauth') {
        $oauthId = [long](Get-SafeNumber -InputObject $model -Name 'oauthAccountId')
        if ($oauthId -gt 0) {
            if ($oauthIds -notcontains $oauthId) { $oauthIds += $oauthId }
            if (-not $modelsByOAuth.ContainsKey($oauthId)) { $modelsByOAuth[$oauthId] = [System.Collections.ArrayList]::new() }
            if ($modelId -and $modelsByOAuth[$oauthId] -notcontains $modelId) { [void]$modelsByOAuth[$oauthId].Add($modelId) }
        }
    } else {
        $alias = Get-SafeString -InputObject $model -Name 'alias'
        if (-not $alias) { $alias = $modelId }
        if ($alias) {
            $accountName = "Codex-Router / $alias"
            $apiAccountNames[$accountName] = $true
            $apiChannelsByName[$accountName] = $model
        }
    }
}
foreach ($id in @(Get-SafeValue -InputObject $config -Name 'oauthAccountIds')) {
    if ($null -ne $id -and $oauthIds -notcontains [long]$id) { $oauthIds += [long]$id }
}

$session = Invoke-LocalAdminRead -Operation { New-RouterAdminSession }
try {
    $groups = @(Invoke-LocalAdminRead -Operation { Get-RouterGroups -Session $session })
    $routerGroup = $groups | Where-Object { (Get-SafeString -InputObject $_ -Name 'name') -eq 'Codex-Router' } | Select-Object -First 1
    $routerGroupId = [long](Get-SafeNumber -InputObject $routerGroup -Name 'id')
    $selected = @()
    $availableAccounts = @(Invoke-LocalAdminRead -Operation { Get-RouterAccounts -Session $session })
    foreach ($account in $availableAccounts) {
        $id = [long](Get-SafeNumber -InputObject $account -Name 'id')
        $kind = Get-SafeString -InputObject $account -Name 'type'
        $name = Get-SafeString -InputObject $account -Name 'name'
        $groupIds = @(Get-SafeValue -InputObject $account -Name 'group_ids')
        $selectedOAuth = $kind -eq 'oauth' -and $oauthIds -contains $id
        $selectedApi = $kind -eq 'apikey' -and $apiAccountNames.ContainsKey($name) -and
            ($routerGroupId -le 0 -or $groupIds -contains $routerGroupId)
        if ($selectedOAuth -or $selectedApi) {
            $selected += $account
        }
    }

    $subscriptions = @()
    $apiChannels = @()
    $codingPlanCache = @{}
    foreach ($account in $selected) {
        $id = [long](Get-SafeNumber -InputObject $account -Name 'id')
        $configuredModels = if ($modelsByOAuth.ContainsKey($id)) { @($modelsByOAuth[$id]) } else { @() }
        $configuredChannel = if ($apiChannelsByName.ContainsKey([string]$account.name)) { $apiChannelsByName[[string]$account.name] } else { $null }
        $record = Get-AccountRecord -Session $session -Account $account -ConfiguredModels $configuredModels `
            -ConfiguredChannel $configuredChannel -CodingPlanCache $codingPlanCache
        if ($record.kind -eq 'oauth') { $subscriptions += $record } else { $apiChannels += $record }
    }

    $observationPath = Join-Path (Get-RouterDataRoot -RouterRoot $routerRoot) 'state\oauth-recovery-observations.json'
    $observations = @{}
    if (Test-Path -LiteralPath $observationPath) {
        try {
            $saved = Get-Content -LiteralPath $observationPath -Raw | ConvertFrom-Json
            foreach ($entry in @($saved.entries)) {
                if ([long]$entry.accountId -gt 0) { $observations[[long]$entry.accountId] = $entry }
            }
        } catch { $observations = @{} }
    }
    foreach ($record in $subscriptions) {
        $account = $selected | Where-Object { [long]$_.id -eq [long]$record.id } | Select-Object -First 1
        if ($null -eq $account) { continue }
        $exhaustedWindows = @($record.windows | Where-Object {
            $null -ne $_.usedPercent -and [double]$_.usedPercent -ge 99.999
        })
        if ($record.health -eq 'quotaExhausted') {
            $resetTimes = @($exhaustedWindows | ForEach-Object {
                ConvertTo-RouterResetAtUtc -Value $_.resetAt
            } | Where-Object { $null -ne $_ } | Sort-Object)
            $resetAt = if ($resetTimes.Count -gt 0) { $resetTimes[-1].UtcDateTime.ToString('o') } else { '' }
            $observations[[long]$record.id] = [pscustomobject][ordered]@{
                accountId = [long]$record.id
                exhausted = $true
                resetAt = $resetAt
                observedAt = [DateTime]::UtcNow.ToString('o')
            }
            if ($routerGroupId -gt 0) {
                [void](Set-RouterAccountGroupMembership -Session $session -AccountId ([long]$record.id) `
                    -GroupId $routerGroupId -Enabled $false -Account $account)
            }
        } elseif ($record.windows.Count -gt 0 -and -not $record.queryNote) {
            [void]$observations.Remove([long]$record.id)
        }
    }
    $observationDocument = [ordered]@{ entries = @($observations.Values) } | ConvertTo-Json -Depth 6
    Write-RouterTextFileAtomic -Path $observationPath -Text $observationDocument

    $totalTokens = [long](($subscriptions + $apiChannels | ForEach-Object { $_.totals.totalTokens } | Measure-Object -Sum).Sum)
    $totalRequests = [long](($subscriptions + $apiChannels | ForEach-Object { $_.totals.requests } | Measure-Object -Sum).Sum)
    $totalCost = [double](($subscriptions + $apiChannels | ForEach-Object { $_.totals.cost } | Measure-Object -Sum).Sum)
    $result = [ordered]@{
        profileName = $ProfileName
        queriedAt = [DateTime]::UtcNow.ToString('o')
        totalTokens = $totalTokens
        totalRequests = $totalRequests
        totalCost = $totalCost
        subscriptions = $subscriptions
        apiChannels = $apiChannels
    } | ConvertTo-Json -Depth 14 -Compress
    [Console]::Out.WriteLine($result)
} finally {
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
