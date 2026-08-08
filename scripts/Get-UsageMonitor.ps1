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
        foreach ($key in @($InputObject.Keys)) {
            if ([string]$key -ieq $Name) { return $InputObject[$key] }
        }
        return $null
    }
    foreach ($property in @($InputObject.PSObject.Properties)) {
        if ([string]$property.Name -ieq $Name) { return $property.Value }
    }
    return $null
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
    $number = 0.0
    if ([double]::TryParse([string]$value, [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$number)) {
        if (-not [double]::IsNaN($number) -and -not [double]::IsInfinity($number)) { return $number }
    }
    return 0.0
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

function New-BalanceUsageWindow {
    param(
        [Parameter(Mandatory)][string]$DisplayName,
        [Parameter(Mandatory)][double]$RemainingAmount,
        [string]$Currency = 'USD',
        [AllowNull()]$LimitAmount = $null,
        [AllowNull()]$UsedAmount = $null
    )
    $usedPercent = $null
    if ($null -ne $LimitAmount) {
        try {
            $limit = [double]$LimitAmount
            if ($limit -gt 0 -and $null -ne $UsedAmount) {
                $usedPercent = ConvertTo-NormalizedPercent -Value (([double]$UsedAmount / $limit) * 100.0)
            }
        } catch { }
    }
    return [ordered]@{
        kind = 'balance'
        displayName = $DisplayName
        usedPercent = $usedPercent
        resetAt = ''
        remainingSeconds = -1
        requests = 0
        tokens = 0
        remainingAmount = [Math]::Max(0.0, $RemainingAmount)
        limitAmount = $LimitAmount
        usedAmount = $UsedAmount
        currency = $Currency.ToUpperInvariant()
    }
}

function Get-FirstSafeValue {
    param([AllowNull()]$InputObject, [Parameter(Mandatory)][string[]]$Names)
    foreach ($name in $Names) {
        $value = Get-SafeValue -InputObject $InputObject -Name $name
        if ($null -eq $value) { continue }
        if ($value -is [string] -and [string]::IsNullOrWhiteSpace([string]$value)) { continue }
        return ,$value
    }
    return $null
}

function Get-ChannelBaseUrl {
    param([AllowNull()]$Channel)
    $value = Get-FirstSafeValue -InputObject $Channel -Names @('baseUrl','baseURL')
    if ($null -eq $value) { return '' }
    return ([string]$value).TrimEnd('/')
}

function ConvertTo-NormalizedPercent {
    param([AllowNull()]$Value, [ValidateSet('percent','ratio','used','remaining')][string]$Semantics = 'percent')
    if ($null -eq $Value) { return $null }
    try { $number = [double]$Value } catch { return $null }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) { return $null }
    if ($Semantics -eq 'remaining') { $number = 100.0 - $number }
    elseif ($Semantics -eq 'ratio' -and $number -le 1.0) { $number *= 100.0 }
    return [Math]::Max(0.0, [Math]::Min(100.0, $number))
}

function Get-NormalizedWindowPercent {
    param([AllowNull()]$Window, [string]$Provider = '')
    if ($null -eq $Window) { return $null }
    foreach ($name in @('utilization','used_percent','usedPercent','percentage','percent','usage_percentage','usagePercentage','remaining_percent','remainingPercent')) {
        $value = Get-SafeValue -InputObject $Window -Name $name
        if ($null -eq $value) { continue }
        $numeric = ConvertTo-NormalizedPercent -Value $value
        if ($null -eq $numeric) { continue }
        if ($name -match '(?i)usage_percentage|usagePercentage') {
            try { if ([double]$value -le 1.0) { $numeric = ConvertTo-NormalizedPercent -Value $value -Semantics 'ratio' } } catch { }
        }
        elseif ($name -match '(?i)remaining_percent|remainingPercent') {
            $numeric = ConvertTo-NormalizedPercent -Value $value -Semantics 'remaining'
        }
        if ($null -ne $numeric) { return $numeric }
    }
    $limit = Get-FirstSafeValue -InputObject $Window -Names @('limit','limitValue','limit_value','total','totalValue','total_value','quota','quotaValue','quota_value','max','maxValue','max_value')
    $used = Get-FirstSafeValue -InputObject $Window -Names @('used','usedValue','used_value','usedAmount','used_amount','consumed','consumedValue','consumed_value','current','currentValue','current_value','total_used')
    $remaining = Get-FirstSafeValue -InputObject $Window -Names @('remaining','remainingValue','remaining_value','limit_remaining')
    if ($null -ne $limit) {
        try { $limitNumber = [double]$limit } catch { $limitNumber = 0.0 }
        if ($limitNumber -gt 0 -and $null -ne $used) {
            try { return ConvertTo-NormalizedPercent -Value (([double]$used / $limitNumber) * 100.0) } catch { }
        }
        if ($limitNumber -gt 0 -and $null -ne $remaining) {
            try { return ConvertTo-NormalizedPercent -Value ((([double]$limitNumber - [double]$remaining) / $limitNumber) * 100.0) } catch { }
        }
    }
    return $null
}

function Get-NormalizedResetValue {
    param([AllowNull()]$Window)
    return Get-FirstSafeValue -InputObject $Window -Names @('resets_at','reset_time','reset_at','resetAt','resetTime','next_reset_at','nextResetAt','ResetTimestamp','reset_timestamp')
}

function ConvertTo-KimiWindowMinutes {
    param([AllowNull()]$Window)
    if ($null -eq $Window) { return $null }
    $durationValue = Get-FirstSafeValue -InputObject $Window -Names @('duration','windowDuration','window_duration','size','value','length')
    if ($null -eq $durationValue) { return $null }
    try { $duration = [double]$durationValue } catch { return $null }
    if ($duration -le 0) { return $null }
    $unit = [string](Get-FirstSafeValue -InputObject $Window -Names @('timeUnit','time_unit','unit','windowUnit','window_unit'))
    $unit = $unit.Trim().ToUpperInvariant()
    if ($unit.Contains('MIN')) { return $duration }
    if ($unit.Contains('HOUR')) { return $duration * 60.0 }
    if ($unit.Contains('DAY')) { return $duration * 24.0 * 60.0 }
    if ($unit.Contains('WEEK')) { return $duration * 7.0 * 24.0 * 60.0 }
    if ($unit.Contains('MONTH')) { return $duration * 30.0 * 24.0 * 60.0 }
    return $null
}

function Get-KimiWindowKind {
    param([AllowNull()]$Window)
    $minutes = ConvertTo-KimiWindowMinutes -Window $Window
    if ($null -ne $minutes) {
        if ([double]$minutes -le 360.0) { return 'fiveHour' }
        return 'weekly'
    }
    $name = [string](Get-FirstSafeValue -InputObject $Window -Names @('name','label','title'))
    if ($name -match '(?i)hour|5h') { return 'fiveHour' }
    if ($name -match '(?i)week|7d|day') { return 'weekly' }
    return ''
}

function ConvertTo-ZhipuWindowMinutes {
    param([AllowNull()]$Window)
    if ($null -eq $Window) { return $null }
    $unitValue = Get-SafeValue -InputObject $Window -Name 'unit'
    if ($null -eq $unitValue) { return $null }
    try { $unit = [long]$unitValue } catch { return $null }
    $numberValue = Get-SafeValue -InputObject $Window -Name 'number'
    if ($null -eq $numberValue) {
        if ($unit -eq 3) { return 300.0 }
        if ($unit -eq 6) { return 10080.0 }
        return $null
    }
    try { $number = [double]$numberValue } catch { return $null }
    if ($number -le 0) { return $null }
    switch ($unit) {
        5 { return $number }
        3 { return $number * 60.0 }
        1 { return $number * 24.0 * 60.0 }
        6 { return $number * 7.0 * 24.0 * 60.0 }
        default { return $null }
    }
}

function Get-ZhipuUsedPercent {
    param([AllowNull()]$Window)
    $totalValue = Get-SafeValue -InputObject $Window -Name 'usage'
    if ($null -ne $totalValue) {
        try { $total = [double]$totalValue } catch { $total = 0.0 }
        if ($total -gt 0) {
            $remainingValue = Get-SafeValue -InputObject $Window -Name 'remaining'
            $currentValue = Get-FirstSafeValue -InputObject $Window -Names @('currentValue','current_value')
            $used = $null
            if ($null -ne $remainingValue) {
                try { $used = $total - [double]$remainingValue } catch { }
            }
            if ($null -ne $currentValue) {
                try {
                    $current = [double]$currentValue
                    if ($null -eq $used -or $current -gt [double]$used) { $used = $current }
                } catch { }
            }
            if ($null -ne $used) {
                $used = [Math]::Max(0.0, [Math]::Min($total, [double]$used))
                return ConvertTo-NormalizedPercent -Value (($used / $total) * 100.0)
            }
        }
    }
    return Get-NormalizedWindowPercent -Window $Window
}

function Get-ProviderFromChannel {
    param([AllowNull()]$Channel)
    if ($null -eq $Channel) { return '' }
    $baseUrl = (Get-ChannelBaseUrl -Channel $Channel).ToLowerInvariant()
    $uri = $null
    if (-not [Uri]::TryCreate($baseUrl, [UriKind]::Absolute, [ref]$uri)) { return 'thirdparty' }
    $hostName = $uri.DnsSafeHost.ToLowerInvariant()
    $path = $uri.AbsolutePath.TrimEnd('/').ToLowerInvariant()
    if ($hostName -eq 'api.kimi.com' -and $path.StartsWith('/coding')) { return 'kimi-coding' }
    if ($hostName -eq 'ark.cn-beijing.volces.com' -and ($path -match '^/api/(coding|plan)(/|$)')) { return 'volcengine-coding' }
    if (($hostName -eq 'open.bigmodel.cn' -or $hostName -eq 'bigmodel.cn' -or $hostName -eq 'api.z.ai') -and $path -match '^/api/monitor(/|$)') { return 'zhipu-coding' }
    if (($hostName -eq 'api.minimaxi.com' -or $hostName -eq 'api.minimax.io') -and $path -match '^/v1/api/openplatform/coding_plan(/|$)') { return 'minimax-coding' }
    if (($hostName -eq 'api.zenmux.ai' -or $hostName -eq 'zenmux.ai') -and $path -match '^/api/(v\d+/)?(usage|quota)(/|$)') { return 'zenmux-coding' }
    if ($hostName -eq 'openrouter.ai') { return 'openrouter' }
    if ($hostName -eq 'api.moonshot.ai' -or $hostName -eq 'api.moonshot.cn') { return 'moonshot' }
    if ($hostName -eq 'api.xiaomimimo.com' -or $hostName -eq 'platform.xiaomimimo.com') { return 'mimo' }
    if ($hostName -eq 'api.deepseek.com') { return 'deepseek' }
    if ($hostName -eq 'api.openai.com') { return 'openai-api' }
    if ($hostName -eq 'api.anthropic.com') { return 'anthropic-api' }
    return 'thirdparty'
}

function ConvertFrom-GrokQuotaUsage {
    param([AllowNull()]$Body)
    $windows = [System.Collections.ArrayList]::new()
    if ($null -eq $Body) { return @($windows) }

    $billing = Get-SafeValue -InputObject $Body -Name 'billing'
    if ($null -eq $billing) { $billing = $Body }
    $periodType = Get-SafeString -InputObject $billing -Name 'period_type'
    $subscriptionKind = switch ($periodType) {
        'weekly' { 'weekly' }
        'monthly' { 'monthly' }
        'daily' { 'other' }
        default { 'weekly' }
    }
    $subscriptionPercent = Get-SafeValue -InputObject $billing -Name 'usage_percent'
    if ($null -ne $subscriptionPercent) {
        [void]$windows.Add((New-CodingPlanWindow -Kind $subscriptionKind `
            -UsedPercent ([double]$subscriptionPercent) `
            -ResetAt (Get-SafeValue -InputObject $billing -Name 'period_end') `
            -DisplayName (Get-SafeString -InputObject $billing -Name 'plan')))
    }

    $monthlyPercent = Get-SafeValue -InputObject $billing -Name 'used_percent'
    if ($null -ne $monthlyPercent -and $subscriptionKind -ne 'monthly') {
        $usedCents = [long](Get-SafeNumber -InputObject $billing -Name 'used_cents')
        $limitCents = [long](Get-SafeNumber -InputObject $billing -Name 'monthly_limit_cents')
        $monthlyLabel = if ($limitCents -gt 0) {
            'Monthly quota ${0:N2} / ${1:N2}' -f ($usedCents / 100), ($limitCents / 100)
        } else {
            'Monthly quota'
        }
        [void]$windows.Add((New-CodingPlanWindow -Kind 'monthly' `
            -UsedPercent ([double]$monthlyPercent) `
            -ResetAt (Get-SafeValue -InputObject $billing -Name 'billing_period_end') `
            -DisplayName $monthlyLabel))
    }

    foreach ($product in @(Get-SafeValue -InputObject $billing -Name 'product_usage')) {
        if ($null -eq $product) { continue }
        $productPercent = Get-SafeValue -InputObject $product -Name 'usage_percent'
        if ($null -eq $productPercent) { continue }
        $productName = Get-SafeString -InputObject $product -Name 'product'
        if (-not $productName) { $productName = 'Grok' }
        [void]$windows.Add((New-CodingPlanWindow -Kind 'model' `
            -UsedPercent ([double]$productPercent) `
            -ResetAt (Get-SafeValue -InputObject $billing -Name 'period_end') `
            -DisplayName $productName))
    }

    $snapshot = Get-SafeValue -InputObject $Body -Name 'snapshot'
    foreach ($definition in @(
        @{ Name = 'tokens'; DisplayName = 'Grok token quota' },
        @{ Name = 'requests'; DisplayName = 'Grok request quota' }
    )) {
        $quota = Get-SafeValue -InputObject $snapshot -Name $definition.Name
        if ($null -eq $quota) { continue }
        $limit = Get-SafeNumber -InputObject $quota -Name 'limit'
        $remaining = Get-SafeValue -InputObject $quota -Name 'remaining'
        if ($limit -le 0 -or $null -eq $remaining) { continue }
        $resetAt = Get-SafeValue -InputObject $quota -Name 'reset_at'
        if ($null -eq $resetAt) { $resetAt = Get-SafeValue -InputObject $quota -Name 'reset_unix' }
        [void]$windows.Add((New-CodingPlanWindow -Kind 'other' `
            -UsedPercent ((($limit - [double]$remaining) / $limit) * 100.0) `
            -ResetAt $resetAt `
            -DisplayName $definition.DisplayName))
    }
    return @($windows)
}

function ConvertFrom-KimiCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    if ($null -ne $data) { $Body = $data }
    $classified = @{}
    $unclassified = @()
    $usage = Get-SafeValue -InputObject $Body -Name 'usage'
    if ($null -ne $usage) {
        $usedPercent = Get-NormalizedWindowPercent -Window $usage
        if ($null -ne $usedPercent) {
            $kind = Get-KimiWindowKind -Window $usage
            if (-not $kind) { $kind = 'weekly' }
            $classified[$kind] = New-CodingPlanWindow -Kind $kind `
                -UsedPercent ([double]$usedPercent) `
                -ResetAt (Get-NormalizedResetValue -Window $usage)
        }
    }
    $limits = Get-SafeValue -InputObject $Body -Name 'limits'
    if ($null -eq $limits) { $limits = Get-SafeValue -InputObject $Body -Name 'limitInfos' }
    if ($null -eq $limits) { $limits = Get-SafeValue -InputObject $Body -Name 'limit_infos' }
    if ($null -eq $limits) { $limits = Get-SafeValue -InputObject $Body -Name 'rateLimits' }
    if ($null -eq $limits) { $limits = Get-SafeValue -InputObject $Body -Name 'rate_limits' }
    if ($null -eq $limits) { $limits = Get-SafeValue -InputObject $Body -Name 'windows' }
    foreach ($item in @($limits)) {
        $detail = Get-FirstSafeValue -InputObject $item -Names @('detail','usage','quota')
        if ($null -eq $detail) { $detail = $item }
        $usedPercent = Get-NormalizedWindowPercent -Window $detail
        if ($null -eq $usedPercent) { continue }
        $window = Get-FirstSafeValue -InputObject $item -Names @('window','period','rateLimit','rate_limit','timeWindow','time_window')
        if ($null -eq $window) { $window = $item }
        $kind = Get-KimiWindowKind -Window $window
        $resetAt = Get-NormalizedResetValue -Window $detail
        if ($null -eq $resetAt) { $resetAt = Get-NormalizedResetValue -Window $window }
        $parsedWindow = New-CodingPlanWindow -Kind $(if ($kind) { $kind } else { 'other' }) `
            -UsedPercent ([double]$usedPercent) `
            -ResetAt $resetAt
        if ($kind -and -not $classified.ContainsKey($kind)) { $classified[$kind] = $parsedWindow }
        elseif (-not $kind) { $unclassified += $parsedWindow }
    }
    foreach ($window in $unclassified) {
        $kind = if (-not $classified.ContainsKey('fiveHour')) { 'fiveHour' } elseif (-not $classified.ContainsKey('weekly')) { 'weekly' } else { '' }
        if (-not $kind) { break }
        $window.kind = $kind
        $classified[$kind] = $window
    }
    $result = @()
    foreach ($kind in @('fiveHour','weekly')) {
        if ($classified.ContainsKey($kind)) { $result += $classified[$kind] }
    }
    return @($result)
}

function ConvertFrom-ZhipuCodingPlanUsage {
    param(
        [Parameter(Mandatory)]$Body,
        [AllowNull()]$SubscriptionBody = $null
    )
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    $tokenLimits = @()
    $limits = @(Get-SafeValue -InputObject $data -Name 'limits')
    foreach ($item in $limits) {
        if ((Get-SafeString -InputObject $item -Name 'type') -notmatch '^(?i)TOKENS_LIMIT$') { continue }
        $usedPercent = Get-ZhipuUsedPercent -Window $item
        if ($null -eq $usedPercent) { continue }
        $minutes = ConvertTo-ZhipuWindowMinutes -Window $item
        $tokenLimits += [pscustomobject]@{ item = $item; usedPercent = $usedPercent; minutes = $minutes }
    }
    $tokenLimits = @($tokenLimits | Sort-Object {
        if ($null -eq $_.minutes) { [double]::MaxValue } else { [double]$_.minutes }
    })
    if ($tokenLimits.Count -eq 0) { return @() }
    $session = $null
    $weekly = $null
    if ($tokenLimits.Count -ge 2) {
        $session = $tokenLimits[0]
        $weekly = $tokenLimits[$tokenLimits.Count - 1]
    } elseif ($null -ne $tokenLimits[0].minutes -and [double]$tokenLimits[0].minutes -le 360.0) {
        $session = $tokenLimits[0]
    } else {
        $weekly = $tokenLimits[0]
    }
    $result = @()
    if ($null -ne $session) {
        $result += New-CodingPlanWindow -Kind 'fiveHour' -UsedPercent ([double]$session.usedPercent) `
            -ResetAt (Get-FirstSafeValue -InputObject $session.item -Names @('nextResetTime','next_reset_time'))
    }
    if ($null -ne $weekly -and $weekly -ne $session) {
        $result += New-CodingPlanWindow -Kind 'weekly' -UsedPercent ([double]$weekly.usedPercent) `
            -ResetAt (Get-FirstSafeValue -InputObject $weekly.item -Names @('nextResetTime','next_reset_time'))
    }
    $timeLimit = @($limits | Where-Object {
        (Get-SafeString -InputObject $_ -Name 'type') -match '^(?i)TIME_LIMIT$' -and $null -ne (Get-ZhipuUsedPercent -Window $_)
    } | Select-Object -First 1)
    if ($timeLimit.Count -gt 0) {
        $subscriptionData = Get-SafeValue -InputObject $SubscriptionBody -Name 'data'
        $subscription = @($subscriptionData | Select-Object -First 1)
        $resetAt = Get-FirstSafeValue -InputObject $timeLimit[0] -Names @('nextResetTime','next_reset_time')
        if ($null -eq $resetAt -and $subscription.Count -gt 0) {
            $resetAt = Get-FirstSafeValue -InputObject $subscription[0] -Names @('nextRenewTime','next_renew_time')
        }
        $result += New-CodingPlanWindow -Kind 'monthly' -UsedPercent ([double](Get-ZhipuUsedPercent -Window $timeLimit[0])) `
            -ResetAt $resetAt -DisplayName 'Z.ai MCP monthly quota'
    }
    return @($result)
}

function ConvertFrom-MiniMaxCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $baseResp = Get-SafeValue -InputObject $Body -Name 'base_resp'
    if ($null -ne $baseResp) {
        $statusCode = Get-FirstSafeValue -InputObject $baseResp -Names @('status_code','statusCode','code')
        if ($null -ne $statusCode) {
            try { if ([long]$statusCode -ne 0) { return @() } } catch { return @() }
        }
    }
    $rows = Get-SafeValue -InputObject $Body -Name 'model_remains'
    if ($null -eq $rows) {
        $data = Get-SafeValue -InputObject $Body -Name 'data'
        $rows = Get-SafeValue -InputObject $data -Name 'model_remains'
    }
    $item = @($rows | Where-Object {
        (Get-SafeString -InputObject $_ -Name 'model_name') -eq 'general'
    } | Select-Object -First 1)
    if ($item.Count -eq 0) { return @() }
    $record = $item[0]
    $windows = @()
    $remaining = Get-SafeValue -InputObject $record -Name 'current_interval_remaining_percent'
    $intervalStatus = Get-SafeValue -InputObject $record -Name 'current_interval_status'
    $intervalPlaceholder = $null -ne $intervalStatus -and [long]$intervalStatus -eq 3 -and `
        ($null -eq $remaining -or [double]$remaining -ge 100.0)
    if ($null -ne $remaining -and -not $intervalPlaceholder) {
        $windows += New-CodingPlanWindow -Kind 'fiveHour' -UsedPercent (100.0 - [double]$remaining) `
            -ResetAt (Get-SafeValue -InputObject $record -Name 'end_time')
    }
    $weeklyRemaining = Get-SafeValue -InputObject $record -Name 'current_weekly_remaining_percent'
    $weeklyStatus = Get-SafeValue -InputObject $record -Name 'current_weekly_status'
    $weeklyPlaceholder = $null -ne $weeklyStatus -and [long]$weeklyStatus -eq 3 -and `
        ($null -eq $weeklyRemaining -or [double]$weeklyRemaining -ge 100.0)
    if ($null -ne $weeklyRemaining -and -not $weeklyPlaceholder) {
        $windows += New-CodingPlanWindow -Kind 'weekly' -UsedPercent (100.0 - [double]$weeklyRemaining) `
            -ResetAt (Get-SafeValue -InputObject $record -Name 'weekly_end_time')
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

function ConvertFrom-VolcengineCodingPlanUsage {
    param([Parameter(Mandatory)]$Body)
    $result = Get-SafeValue -InputObject $Body -Name 'Result'
    if ($null -eq $result) { $result = $Body }
    $windows = @()
    $quotaRows = Get-SafeValue -InputObject $result -Name 'QuotaUsage'
    if ($null -eq $quotaRows) { $quotaRows = Get-SafeValue -InputObject $result -Name 'quotaUsage' }
    foreach ($quota in @($quotaRows)) {
        if ($null -eq $quota) { continue }
        $percent = Get-SafeValue -InputObject $quota -Name 'Percent'
        if ($null -eq $percent) { continue }
        $level = (Get-SafeString -InputObject $quota -Name 'Level').Trim().ToLowerInvariant()
        $kind = switch -Regex ($level) {
            '^(session|5h|5-hour|fivehour|five_hour|rolling_5h)$' { 'fiveHour'; break }
            '^(weekly|week|7d)$' { 'weekly'; break }
            '^(monthly|month)$' { 'monthly'; break }
            default { '' }
        }
        if (-not $kind) { continue }
        $displayName = switch ($kind) {
            'fiveHour' { 'Volcengine 5-hour quota' }
            'weekly' { 'Volcengine weekly quota' }
            'monthly' { 'Volcengine monthly quota' }
        }
        $windows += New-CodingPlanWindow -Kind $kind -UsedPercent ([double]$percent) `
            -ResetAt (Get-SafeValue -InputObject $quota -Name 'ResetTimestamp') `
            -DisplayName $displayName
    }
    return @($windows)
}

function ConvertFrom-DeepSeekBalance {
    param([AllowNull()]$Body)
    $rows = @(Get-SafeValue -InputObject $Body -Name 'balance_infos')
    $candidates = @()
    foreach ($row in $rows) {
        if ($null -eq $row) { continue }
        $totalValue = Get-SafeValue -InputObject $row -Name 'total_balance'
        if ($null -eq $totalValue) { continue }
        try { $total = [double]$totalValue } catch { continue }
        $currency = (Get-SafeString -InputObject $row -Name 'currency').ToUpperInvariant()
        if (-not $currency) { continue }
        $candidates += [pscustomobject]@{ total = $total; currency = $currency }
    }
    if ($candidates.Count -eq 0) { return $null }
    $selected = $candidates | Where-Object { $_.total -gt 0 } | Sort-Object total -Descending | Select-Object -First 1
    if ($null -eq $selected) {
        $selected = $candidates | Where-Object { $_.currency -eq 'USD' } | Select-Object -First 1
    }
    if ($null -eq $selected) { $selected = $candidates[0] }
    return New-BalanceUsageWindow -DisplayName 'DeepSeek balance' `
        -RemainingAmount ([double]$selected.total) -Currency ([string]$selected.currency)
}

function ConvertFrom-MimoUsage {
    param(
        [AllowNull()]$BalanceBody,
        [AllowNull()]$DetailBody,
        [AllowNull()]$UsageBody
    )
    $windows = @()
    foreach ($body in @($BalanceBody, $DetailBody, $UsageBody)) {
        if ($null -eq $body) { continue }
        $code = Get-FirstSafeValue -InputObject $body -Names @('code','statusCode','status_code','errorCode','error_code')
        if ($null -eq $code) { continue }
        try { if ([long]$code -ne 0) { return @() } } catch { return @() }
    }
    $balanceData = Get-SafeValue -InputObject $BalanceBody -Name 'data'
    if ($null -eq $balanceData) { $balanceData = $BalanceBody }
    $balance = Get-FirstSafeValue -InputObject $balanceData -Names @('balance','amount','remaining')
    if ($null -ne $balance) {
        try {
            $currency = Get-SafeString -InputObject $balanceData -Name 'currency'
            if (-not $currency) { $currency = 'USD' }
            $windows += New-BalanceUsageWindow -DisplayName 'MiMo balance' -RemainingAmount ([double]$balance) -Currency $currency
        } catch { }
    }

    $detail = Get-SafeValue -InputObject $DetailBody -Name 'data'
    if ($null -eq $detail) { $detail = $DetailBody }
    $status = [string](Get-FirstSafeValue -InputObject $detail -Names @('planStatus','plan_status','subscriptionStatus','subscription_status','status','state'))
    $status = $status.ToLowerInvariant()
    $expired = $status -match 'expired|ended'
    $usageData = Get-SafeValue -InputObject $UsageBody -Name 'data'
    if ($null -eq $usageData) { $usageData = $UsageBody }
    $monthUsage = Get-FirstSafeValue -InputObject $usageData -Names @('monthUsage','month_usage')
    if ($null -eq $monthUsage) { $monthUsage = $usageData }
    $items = @(Get-SafeValue -InputObject $monthUsage -Name 'items')
    $item = @($items | Where-Object { (Get-SafeString -InputObject $_ -Name 'name').ToLowerInvariant() -eq 'month_total_token' } | Select-Object -First 1)
    if ($items.Count -eq 0) { $item = @($monthUsage) }
    if ($item.Count -gt 0 -and -not $expired) {
        $limit = Get-FirstSafeValue -InputObject $item[0] -Names @('limit','total','quota')
        $used = Get-FirstSafeValue -InputObject $item[0] -Names @('used','current','consumed')
        $percent = $null
        if ($null -ne $limit -and $null -ne $used) {
            try {
                if ([double]$limit -gt 0) { $percent = ([double]$used / [double]$limit) * 100.0 }
            } catch { }
        }
        if ($null -eq $percent) {
            $rawPercent = Get-SafeValue -InputObject $item[0] -Name 'percent'
            if ($null -ne $rawPercent) {
                $percent = ConvertTo-NormalizedPercent -Value $rawPercent -Semantics 'ratio'
            } else {
                $percent = Get-NormalizedWindowPercent -Window $item[0]
            }
        }
        if ($null -ne $percent) {
            $resetAt = Get-FirstSafeValue -InputObject $detail -Names @('currentPeriodEnd','current_period_end')
            $windows += New-CodingPlanWindow -Kind 'monthly' -UsedPercent ([double]$percent) -ResetAt $resetAt -DisplayName 'MiMo Token Plan'
        }
    }
    return @($windows)
}

function ConvertTo-LowerHex {
    param([Parameter(Mandatory)][byte[]]$Bytes)
    return ([BitConverter]::ToString($Bytes) -replace '-', '').ToLowerInvariant()
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)][AllowEmptyCollection()][byte[]]$Bytes)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ConvertTo-LowerHex -Bytes $sha.ComputeHash($Bytes) }
    finally { $sha.Dispose() }
}

function Get-HmacSha256 {
    param(
        [Parameter(Mandatory)][byte[]]$Key,
        [Parameter(Mandatory)][string]$Value
    )
    $hmac = [Security.Cryptography.HMACSHA256]::new()
    $hmac.Key = [byte[]]$Key.Clone()
    $valueBytes = [Text.Encoding]::UTF8.GetBytes($Value)
    try { return $hmac.ComputeHash($valueBytes) }
    finally {
        [Array]::Clear($valueBytes, 0, $valueBytes.Length)
        $hmac.Dispose()
    }
}

function New-VolcengineSignedHeaders {
    param(
        [Parameter(Mandatory)][string]$AccessKeyId,
        [Parameter(Mandatory)][string]$SecretAccessKey,
        [Parameter(Mandatory)][DateTimeOffset]$Timestamp
    )
    $hostName = 'open.volcengineapi.com'
    $region = 'cn-beijing'
    $service = 'ark'
    $contentType = 'application/json; charset=UTF-8'
    $signedHeaders = 'content-type;host;x-content-sha256;x-date'
    $emptyBytes = [byte[]]::new(0)
    $payloadHash = Get-Sha256Hex -Bytes $emptyBytes
    $xDate = $Timestamp.UtcDateTime.ToString('yyyyMMddTHHmmssZ')
    $shortDate = $xDate.Substring(0, 8)
    $canonicalHeaders = "content-type:$contentType`nhost:$hostName`nx-content-sha256:$payloadHash`nx-date:$xDate`n"
    $canonicalQuery = 'Action=GetCodingPlanUsage&Version=2024-01-01'
    $canonicalRequest = "POST`n/`n$canonicalQuery`n$canonicalHeaders`n$signedHeaders`n$payloadHash"
    $requestBytes = [Text.Encoding]::UTF8.GetBytes($canonicalRequest)
    try { $requestHash = Get-Sha256Hex -Bytes $requestBytes }
    finally { [Array]::Clear($requestBytes, 0, $requestBytes.Length) }
    $scope = "$shortDate/$region/$service/request"
    $stringToSign = "HMAC-SHA256`n$xDate`n$scope`n$requestHash"
    $secretBytes = [Text.Encoding]::UTF8.GetBytes($SecretAccessKey)
    $dateKey = $null
    $regionKey = $null
    $serviceKey = $null
    $signingKey = $null
    $signatureBytes = $null
    try {
        $dateKey = Get-HmacSha256 -Key $secretBytes -Value $shortDate
        $regionKey = Get-HmacSha256 -Key $dateKey -Value $region
        $serviceKey = Get-HmacSha256 -Key $regionKey -Value $service
        $signingKey = Get-HmacSha256 -Key $serviceKey -Value 'request'
        $signatureBytes = Get-HmacSha256 -Key $signingKey -Value $stringToSign
        $signature = ConvertTo-LowerHex -Bytes $signatureBytes
        return @{
            Accept = 'application/json'
            'Content-Type' = $contentType
            'X-Date' = $xDate
            'X-Content-Sha256' = $payloadHash
            Authorization = "HMAC-SHA256 Credential=$AccessKeyId/$scope, SignedHeaders=$signedHeaders, Signature=$signature"
        }
    } finally {
        foreach ($bytes in @($secretBytes, $dateKey, $regionKey, $serviceKey, $signingKey, $signatureBytes)) {
            if ($null -ne $bytes) { [Array]::Clear($bytes, 0, $bytes.Length) }
        }
    }
}

function Get-CodingPlanEndpoint {
    param([Parameter(Mandatory)][string]$BaseUrl)
    $uri = $null
    if (-not [Uri]::TryCreate($BaseUrl.TrimEnd('/'), [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -ne 'https' -or
        -not [string]::IsNullOrEmpty($uri.UserInfo)) {
        return $null
    }
    $endpointHost = $uri.IdnHost.ToLowerInvariant()
    $endpointPath = $uri.AbsolutePath.TrimEnd('/').ToLowerInvariant()
    switch ($endpointHost) {
        'api.kimi.com' {
            if ($endpointPath -notmatch '^/coding(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'Kimi Coding Plan'; Uri = 'https://api.kimi.com/coding/v1/usages' }
        }
        'open.bigmodel.cn' {
            if ($endpointPath -notmatch '^/api/monitor(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'Zhipu GLM Coding Plan'; Uri = 'https://open.bigmodel.cn/api/monitor/usage/quota/limit' }
        }
        'bigmodel.cn' {
            if ($endpointPath -notmatch '^/api/monitor(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'Zhipu GLM Coding Plan'; Uri = 'https://open.bigmodel.cn/api/monitor/usage/quota/limit' }
        }
        'api.z.ai' {
            if ($endpointPath -notmatch '^/api/monitor(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'Zhipu GLM Coding Plan'; Uri = 'https://api.z.ai/api/monitor/usage/quota/limit' }
        }
        'api.minimaxi.com' {
            if ($endpointPath -notmatch '^/v1/api/openplatform/coding_plan(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'MiniMax Coding Plan'; Uri = 'https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains' }
        }
        'api.minimax.io' {
            if ($endpointPath -notmatch '^/v1/api/openplatform/coding_plan(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'MiniMax Coding Plan'; Uri = 'https://api.minimax.io/v1/api/openplatform/coding_plan/remains' }
        }
        'api.zenmux.ai' {
            if ($endpointPath -notmatch '^/api/(?:v\d+/)?(?:usage|quota)(?:/|$)' -and $endpointPath -notmatch '^/api/v\d+(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'ZenMux Coding Plan'; Uri = $uri.AbsoluteUri.TrimEnd('/') }
        }
        'zenmux.ai' {
            if ($endpointPath -notmatch '^/api/(?:v\d+/)?(?:usage|quota)(?:/|$)' -and $endpointPath -notmatch '^/api/v\d+(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'ZenMux Coding Plan'; Uri = $uri.AbsoluteUri.TrimEnd('/') }
        }
        'ark.cn-beijing.volces.com' {
            if ($endpointPath -notmatch '^/api/(?:coding|plan)(?:/|$)') { return $null }
            return [pscustomobject]@{ Provider = 'Volcengine Coding Plan'; Uri = '' }
        }
        default { return $null }
    }
}

function Get-UsageMonitorCachePath {
    return (Join-Path (Get-RouterDataRoot -RouterRoot $routerRoot) 'state\usage-monitor-last-good.json')
}

function Get-UsageWindowCacheKey {
    param([AllowNull()]$Window)
    $kind = (Get-SafeString -InputObject $Window -Name 'kind').ToLowerInvariant()
    $displayName = (Get-SafeString -InputObject $Window -Name 'displayName').ToLowerInvariant()
    $currency = (Get-SafeString -InputObject $Window -Name 'currency').ToLowerInvariant()
    $limit = Get-FirstSafeValue -InputObject $Window -Names @('limitAmount','limit_amount','limit')
    $limitText = if ($null -eq $limit) { '' } else { [string]$limit }
    if (-not $kind) { return '' }
    return "$kind|$displayName|$currency|$limitText"
}

function Merge-UsageWindows {
    param(
        [AllowNull()]$Existing,
        [AllowNull()]$Incoming
    )
    $merged = [ordered]@{}
    foreach ($window in @($Incoming) + @($Existing)) {
        if ($null -eq $window) { continue }
        $key = Get-UsageWindowCacheKey -Window $window
        if (-not $key) { continue }
        if (-not $merged.Contains($key)) { $merged[$key] = $window }
    }
    return @($merged.Values)
}

function Read-UsageMonitorCache {
    $path = Get-UsageMonitorCachePath
    if (-not (Test-Path -LiteralPath $path)) { return @{} }
    try {
        $document = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
        $entries = @{}
        foreach ($property in @($document.PSObject.Properties)) { $entries[$property.Name] = $property.Value }
        return $entries
    } catch { return @{} }
}

function Write-UsageMonitorCache {
    param([Parameter(Mandatory)][hashtable]$Cache)
    try {
        $document = [ordered]@{}
        foreach ($key in @($Cache.Keys)) {
            $entry = $Cache[$key]
            if ($null -ne $entry) { $document[$key] = $entry }
        }
        $json = $document | ConvertTo-Json -Depth 10 -Compress
        Write-RouterTextFileAtomic -Path (Get-UsageMonitorCachePath) -Text $json
    } catch {
        # A locked state file must not turn live usage into a query error.
    }
}

function Get-UsageCacheKey {
    param(
        [Parameter(Mandatory)][string]$Provider,
        [Parameter(Mandatory)][string]$BaseUrl,
        [string]$CredentialName = ''
    )
    return ('{0}|{1}|{2}' -f $Provider.ToLowerInvariant(), $BaseUrl.TrimEnd('/').ToLowerInvariant(), $CredentialName.ToLowerInvariant())
}

function Get-UsageResponseStatus {
    param([Parameter(Mandatory)]$ErrorRecord)
    $response = $ErrorRecord.Exception.PSObject.Properties['Response']
    if ($null -eq $response -or $null -eq $response.Value) { return 0 }
    $status = $response.Value.PSObject.Properties['StatusCode']
    if ($null -eq $status) { return 0 }
    try { return [int]$status.Value } catch { return 0 }
}

function Test-UsageRetryableStatus {
    param([int]$StatusCode)
    return $StatusCode -eq 0 -or $StatusCode -eq 408 -or $StatusCode -eq 425 -or $StatusCode -eq 429 -or $StatusCode -ge 500
}

function Invoke-UsageRestMethod {
    param(
        [Parameter(Mandatory)][string]$Uri,
        [Parameter(Mandatory)][hashtable]$Headers,
        [ValidateSet('GET','POST')][string]$Method = 'GET',
        [AllowNull()][string]$Body = $null,
        [int]$TimeoutSec = 15
    )
    $delays = @(0, 350)
    for ($attempt = 0; $attempt -lt $delays.Count; $attempt++) {
        if ($delays[$attempt] -gt 0) { Start-Sleep -Milliseconds $delays[$attempt] }
        try {
            $params = @{
                Method = $Method
                Uri = $Uri
                Headers = $Headers
                TimeoutSec = $TimeoutSec
                ErrorAction = 'Stop'
            }
            if ($Method -eq 'POST') {
                $params.ContentType = 'application/json; charset=UTF-8'
                $params.Body = if ($null -eq $Body) { '' } else { $Body }
            }
            return Invoke-RestMethod @params
        } catch {
            $status = Get-UsageResponseStatus -ErrorRecord $_
            if ($attempt -eq $delays.Count - 1 -or -not (Test-UsageRetryableStatus -StatusCode $status)) { throw }
        }
    }
}

function Get-UsageCacheFallback {
    param(
        [Parameter(Mandatory)][hashtable]$Cache,
        [Parameter(Mandatory)][string]$Key,
        [Parameter(Mandatory)][string]$Provider,
        [string]$Note = ''
    )
    if (-not $Cache.ContainsKey($Key)) { return $null }
    $entry = $Cache[$Key]
    if ($null -eq $entry) { return $null }
    $windows = @(Get-SafeValue -InputObject $entry -Name 'windows')
    if ($windows.Count -eq 0) { return $null }
    $cachedAt = Get-SafeString -InputObject $entry -Name 'updatedAt'
    if (-not $cachedAt) { return $null }
    try {
        $cacheAge = [DateTimeOffset]::UtcNow - [DateTimeOffset]::Parse($cachedAt).ToUniversalTime()
        if ($cacheAge.TotalSeconds -lt -300 -or $cacheAge.TotalHours -gt 6) { return $null }
    } catch { return $null }
    $suffix = if ($cachedAt) { " Last successful refresh: $cachedAt." } else { '' }
    return [pscustomobject]@{
        provider = $Provider
        windows = $windows
        note = if ($Note) { "$Note$suffix" } else { "Showing last successful $Provider data.$suffix" }
        cached = $true
    }
}

function Save-UsageCacheEntry {
    param(
        [Parameter(Mandatory)][hashtable]$Cache,
        [Parameter(Mandatory)][string]$Key,
        [Parameter(Mandatory)]$Result,
        [switch]$MergeExisting
    )
    if ($null -eq $Result -or @($Result.windows).Count -eq 0) { return }
    $existingWindows = if ($MergeExisting -and $Cache.ContainsKey($Key)) { Get-SafeValue -InputObject $Cache[$Key] -Name 'windows' } else { @() }
    $windows = if ($MergeExisting) { @(Merge-UsageWindows -Existing $existingWindows -Incoming $Result.windows) } else { @($Result.windows) }
    if ($Result -is [System.Collections.IDictionary]) { $Result['windows'] = $windows } else { $Result.windows = $windows }
    $Cache[$Key] = [ordered]@{
        updatedAt = [DateTime]::UtcNow.ToString('o')
        windows = $windows
        note = [string]$Result.note
    }
}

function ConvertFrom-OpenRouterKeyUsage {
    param([AllowNull()]$Body)
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    if ($null -eq $data) { $data = $Body }
    $limit = Get-FirstSafeValue -InputObject $data -Names @('limit','total_limit')
    $used = Get-FirstSafeValue -InputObject $data -Names @('usage','used','total_usage')
    $remaining = Get-FirstSafeValue -InputObject $data -Names @('limit_remaining','remaining')
    if ($null -eq $limit -or ($null -eq $used -and $null -eq $remaining)) { return $null }
    $reset = (Get-SafeString -InputObject $data -Name 'limit_reset').ToLowerInvariant()
    $kind = if ($reset -eq 'daily') { 'daily' } elseif ($reset -eq 'weekly') { 'weekly' } else { 'monthly' }
    $label = if ($reset -eq 'daily') { 'OpenRouter daily limit' } elseif ($reset -eq 'weekly') { 'OpenRouter weekly limit' } elseif ($reset -eq 'monthly') { 'OpenRouter monthly limit' } else { 'OpenRouter API key limit' }
    return [ordered]@{
        kind = $kind
        displayName = $label
        limit = $limit
        used = $used
        remaining = $remaining
    }
}

function ConvertFrom-OpenRouterCredits {
    param([AllowNull()]$Body)
    $data = Get-SafeValue -InputObject $Body -Name 'data'
    if ($null -eq $data) { $data = $Body }
    $total = Get-FirstSafeValue -InputObject $data -Names @('total_credits','totalCredits','limit')
    $used = Get-FirstSafeValue -InputObject $data -Names @('total_usage','totalUsage','usage','used')
    if ($null -eq $total -or $null -eq $used) { return $null }
    return [ordered]@{
        kind = 'monthly'
        displayName = 'OpenRouter credits'
        limit = $total
        used = $used
    }
}

function ConvertFrom-OpenRouterUsage {
    param([AllowNull()]$KeyBody, [AllowNull()]$CreditsBody)
    $windows = @()
    $keyWindow = ConvertFrom-OpenRouterKeyUsage -Body $KeyBody
    if ($null -ne $keyWindow) {
        $limit = 0.0
        $used = 0.0
        try { $limit = [double]$keyWindow.limit } catch { $limit = 0.0 }
        if ($null -ne $keyWindow.used) {
            try { $used = [double]$keyWindow.used } catch { $used = 0.0 }
        } elseif ($null -ne $keyWindow.remaining) {
            try { $used = $limit - [double]$keyWindow.remaining } catch { $used = 0.0 }
        }
        $window = New-BalanceUsageWindow -DisplayName ([string]$keyWindow.displayName) -RemainingAmount ([Math]::Max(0.0, $limit - $used)) -LimitAmount $limit -UsedAmount $used
        $window.kind = [string]$keyWindow.kind
        $windows += $window
    }
    $creditWindow = ConvertFrom-OpenRouterCredits -Body $CreditsBody
    if ($null -ne $creditWindow) {
        $limit = [double]$creditWindow.limit
        $used = [double]$creditWindow.used
        $windows += New-BalanceUsageWindow -DisplayName ([string]$creditWindow.displayName) -RemainingAmount ([Math]::Max(0.0, $limit - $used)) -LimitAmount $limit -UsedAmount $used
    }
    return @($windows)
}

function Get-ProviderUsage {
    param(
        [Parameter(Mandatory)]$Channel,
        [AllowNull()][hashtable]$UsageCache
    )
    $provider = Get-ProviderFromChannel -Channel $Channel
    $baseUrl = Get-ChannelBaseUrl -Channel $Channel
    $credentialName = Get-SafeString -InputObject $Channel -Name 'credentialName'
    $cacheKey = Get-UsageCacheKey -Provider $provider -BaseUrl $baseUrl -CredentialName $credentialName
    $apiKey = Get-RouterCredential -Name $credentialName -AllowMissing
    if ([string]::IsNullOrWhiteSpace($apiKey)) {
        return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider credential is unavailable." }
    }
    $headers = $null
    try {
        switch ($provider) {
            'openrouter' {
                $headers = @{ Authorization = "Bearer $apiKey"; Accept = 'application/json'; 'HTTP-Referer' = 'https://github.com/Javis603/token-monitor'; 'X-OpenRouter-Title' = 'Token Monitor' }
                $keyBody = $null
                $creditsBody = $null
                try { $keyBody = Invoke-UsageRestMethod -Uri 'https://openrouter.ai/api/v1/key' -Headers $headers -TimeoutSec 15 } catch { }
                try { $creditsBody = Invoke-UsageRestMethod -Uri 'https://openrouter.ai/api/v1/credits' -Headers $headers -TimeoutSec 15 } catch { }
                $windows = @(ConvertFrom-OpenRouterUsage -KeyBody $keyBody -CreditsBody $creditsBody)
                if ($windows.Count -eq 0) { throw 'OpenRouter returned no readable key or credit usage.' }
                $result = [pscustomobject]@{ provider = 'OpenRouter'; windows = $windows; note = 'OpenRouter official key and credit usage.' }
                $partial = ($null -eq $keyBody -or $null -eq $creditsBody)
                if ($null -ne $UsageCache) { Save-UsageCacheEntry -Cache $UsageCache -Key $cacheKey -Result $result -MergeExisting:$partial }
                return $result
            }
            'deepseek' {
                $headers = @{ Authorization = "Bearer $apiKey"; Accept = 'application/json' }
                $body = Invoke-UsageRestMethod -Uri 'https://api.deepseek.com/user/balance' -Headers $headers -TimeoutSec 15
                $window = ConvertFrom-DeepSeekBalance -Body $body
                if ($null -eq $window) { throw 'DeepSeek returned no readable balance.' }
                $result = [pscustomobject]@{ provider = 'DeepSeek'; windows = @($window); note = 'DeepSeek official balance API.' }
                if ($null -ne $UsageCache) { Save-UsageCacheEntry -Cache $UsageCache -Key $cacheKey -Result $result }
                return $result
            }
            'mimo' {
                if ($apiKey -notmatch '(?i)(^|;\s*)api-platform_serviceToken=[^;]+' -or
                    $apiKey -notmatch '(?i)(^|;\s*)userId=[^;]+') {
                    return [pscustomobject]@{ provider = 'MiMo'; windows = @(); note = 'MiMo official Token Plan usage requires a browser Cookie containing api-platform_serviceToken and userId; local token statistics are shown.' }
                }
                $headers = @{
                    Accept = 'application/json, text/plain, */*'
                    Cookie = $apiKey
                    Origin = 'https://platform.xiaomimimo.com'
                    Referer = 'https://platform.xiaomimimo.com/#/console/balance'
                    'User-Agent' = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131 Safari/537.36'
                }
                $balanceBody = Invoke-UsageRestMethod -Uri 'https://platform.xiaomimimo.com/api/v1/balance' -Headers $headers -TimeoutSec 15
                $detailBody = $null
                $usageBody = $null
                try { $detailBody = Invoke-UsageRestMethod -Uri 'https://platform.xiaomimimo.com/api/v1/tokenPlan/detail' -Headers $headers -TimeoutSec 15 } catch { }
                try { $usageBody = Invoke-UsageRestMethod -Uri 'https://platform.xiaomimimo.com/api/v1/tokenPlan/usage' -Headers $headers -TimeoutSec 15 } catch { }
                $windows = @(ConvertFrom-MimoUsage -BalanceBody $balanceBody -DetailBody $detailBody -UsageBody $usageBody)
                if ($windows.Count -eq 0) { throw 'MiMo returned no readable balance or Token Plan usage.' }
                $result = [pscustomobject]@{ provider = 'MiMo'; windows = $windows; note = 'MiMo official balance and Token Plan usage.' }
                if ($null -ne $UsageCache) { Save-UsageCacheEntry -Cache $UsageCache -Key $cacheKey -Result $result -MergeExisting }
                return $result
            }
            default {
                return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider does not expose a reliable official time-window usage endpoint with the configured credential; local token statistics are shown." }
            }
        }
    } catch {
        $fallback = if ($null -ne $UsageCache) { Get-UsageCacheFallback -Cache $UsageCache -Key $cacheKey -Provider $provider -Note "$provider live usage query failed; showing cached usage." } else { $null }
        if ($null -ne $fallback) { return $fallback }
        $status = 'transport error'
        $response = $_.Exception.PSObject.Properties['Response']
        if ($null -ne $response -and $null -ne $response.Value) {
            $statusCode = $response.Value.PSObject.Properties['StatusCode']
            if ($null -ne $statusCode) { $status = "HTTP $([int]$statusCode.Value)" }
        }
        return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider usage query failed ($status); local token statistics are shown." }
    } finally {
        $apiKey = $null
        if ($headers) { $headers.Clear() }
    }
}

function Get-CodingPlanUsage {
    param(
        [Parameter(Mandatory)]$Channel,
        [AllowNull()][hashtable]$UsageCache
    )
    $baseUrl = Get-ChannelBaseUrl -Channel $Channel
    $credentialName = Get-SafeString -InputObject $Channel -Name 'credentialName'
    $endpoint = Get-CodingPlanEndpoint -BaseUrl $baseUrl
    if ($null -eq $endpoint) { return $null }
    $provider = [string]$endpoint.Provider
    $cacheKey = Get-UsageCacheKey -Provider $provider -BaseUrl $baseUrl -CredentialName $credentialName
    if ($provider -eq 'Volcengine Coding Plan') {
        $accessKeyId = Get-RouterCredential -Name 'VolcengineAccessKeyId' -AllowMissing
        $secretAccessKey = Get-RouterCredential -Name 'VolcengineSecretAccessKey' -AllowMissing
        if ([string]::IsNullOrWhiteSpace($accessKeyId) -or [string]::IsNullOrWhiteSpace($secretAccessKey)) {
            return [pscustomobject]@{ provider = $provider; windows = @(); note = 'Add the Volcengine control-plane AK/SK in this model to query official 5-hour, weekly, and monthly quota.' }
        }
        try {
            $uri = 'https://open.volcengineapi.com/?Action=GetCodingPlanUsage&Version=2024-01-01'
            $headers = New-VolcengineSignedHeaders -AccessKeyId $accessKeyId `
                -SecretAccessKey $secretAccessKey -Timestamp ([DateTimeOffset]::UtcNow)
            $body = Invoke-UsageRestMethod -Method POST -Uri $uri -Headers $headers -Body '' -TimeoutSec 15
            $metadata = Get-SafeValue -InputObject $body -Name 'ResponseMetadata'
            $apiError = Get-SafeValue -InputObject $metadata -Name 'Error'
            if ($null -ne $apiError) {
                $errorCode = Get-SafeString -InputObject $apiError -Name 'Code'
                throw "Volcengine control-plane error: $errorCode"
            }
            $windows = @(ConvertFrom-VolcengineCodingPlanUsage -Body $body)
            if ($windows.Count -eq 0) { throw 'Volcengine Coding Plan returned no readable quota windows.' }
            $result = [pscustomobject]@{ provider = $provider; windows = $windows; note = '' }
            if ($null -ne $UsageCache) { Save-UsageCacheEntry -Cache $UsageCache -Key $cacheKey -Result $result }
            return $result
        } catch {
            $status = 'transport or authorization error'
            $response = $_.Exception.PSObject.Properties['Response']
            if ($null -ne $response -and $null -ne $response.Value) {
                $statusCode = $response.Value.PSObject.Properties['StatusCode']
                if ($null -ne $statusCode) { $status = "HTTP $([int]$statusCode.Value)" }
            }
            $fallback = if ($null -ne $UsageCache) { Get-UsageCacheFallback -Cache $UsageCache -Key $cacheKey -Provider $provider -Note "Volcengine Coding Plan query failed ($status); showing cached quota." } else { $null }
            if ($null -ne $fallback) { return $fallback }
            return [pscustomobject]@{ provider = $provider; windows = @(); note = "Volcengine Coding Plan quota query failed ($status)." }
        } finally {
            $accessKeyId = $null
            $secretAccessKey = $null
            if ($headers) { $headers.Clear() }
        }
    }
    $apiKey = Get-RouterCredential -Name $credentialName -AllowMissing
    if ([string]::IsNullOrWhiteSpace($apiKey)) {
        return [pscustomobject]@{ provider = $provider; windows = @(); note = "$provider credential is unavailable." }
    }
    $headers = $null
    try {
        $headers = @{ Accept = 'application/json' }
        $headers.Authorization = "Bearer $apiKey"
        $uris = @([string]$endpoint.Uri)
        if ($provider -eq 'MiniMax Coding Plan') {
            $uris = @(
                'https://api.minimax.io/v1/token_plan/remains',
                'https://api.minimax.io/v1/api/openplatform/coding_plan/remains',
                'https://api.minimaxi.com/v1/token_plan/remains',
                'https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains'
            )
        }
        $lastError = $null
        foreach ($uri in $uris) {
            try {
                $body = Invoke-UsageRestMethod -Method GET -Uri $uri -Headers $headers -TimeoutSec 15
                $subscriptionBody = $null
                if ($provider -eq 'Zhipu GLM Coding Plan') {
                    $subscriptionBase = if ($baseUrl -match '(?i)open\.bigmodel\.cn|bigmodel\.cn') { 'https://open.bigmodel.cn' } else { 'https://api.z.ai' }
                    try {
                        $subscriptionBody = Invoke-UsageRestMethod -Method GET -Uri "$subscriptionBase/api/biz/subscription/list" -Headers $headers -TimeoutSec 15
                    } catch { }
                }
                $windows = switch ($provider) {
                    'Kimi Coding Plan' { ConvertFrom-KimiCodingPlanUsage -Body $body }
                    'Zhipu GLM Coding Plan' { ConvertFrom-ZhipuCodingPlanUsage -Body $body -SubscriptionBody $subscriptionBody }
                    'MiniMax Coding Plan' { ConvertFrom-MiniMaxCodingPlanUsage -Body $body }
                    'ZenMux Coding Plan' { ConvertFrom-ZenMuxCodingPlanUsage -Body $body }
                }
                if (@($windows).Count -gt 0) {
                    $result = [pscustomobject]@{ provider = $provider; windows = @($windows); note = "$provider quota queried directly from the provider." }
                    if ($null -ne $UsageCache) { Save-UsageCacheEntry -Cache $UsageCache -Key $cacheKey -Result $result }
                    return $result
                }
                $lastError = "$uri returned no readable quota windows"
            } catch { $lastError = $_ }
        }
        if ($null -ne $lastError) { throw $lastError }
        throw "$provider returned no readable quota windows"
    } catch {
        $status = 'transport error'
        $response = $_.Exception.PSObject.Properties['Response']
        if ($null -ne $response -and $null -ne $response.Value) {
            $statusCode = $response.Value.PSObject.Properties['StatusCode']
            if ($null -ne $statusCode) { $status = "HTTP $([int]$statusCode.Value)" }
        }
        $fallback = if ($null -ne $UsageCache) { Get-UsageCacheFallback -Cache $UsageCache -Key $cacheKey -Provider $provider -Note "$provider live quota query failed ($status); showing cached quota." } else { $null }
        if ($null -ne $fallback) { return $fallback }
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
    $usedPercent = Get-NormalizedWindowPercent -Window $Window
    if ($null -eq $usedPercent) { return }
    $resetValue = Get-NormalizedResetValue -Window $Window
    $resetAt = ConvertTo-CodingPlanResetAt -Value $resetValue
    $remainingValue = Get-SafeValue -InputObject $Window -Name 'remaining_seconds'
    if ($null -eq $remainingValue) {
        $resetDate = $null
        try { $resetDate = [DateTimeOffset]::Parse($resetAt).ToUniversalTime() } catch { }
        $remaining = if ($null -eq $resetDate) { -1 } else { [Math]::Max(-1, [long]($resetDate - [DateTimeOffset]::UtcNow).TotalSeconds) }
    } else {
        $remaining = [long](Get-SafeNumber -InputObject $Window -Name 'remaining_seconds')
    }
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
        [AllowNull()][hashtable]$CodingPlanCache,
        [AllowNull()][hashtable]$UsageCache
    )
    $accountId = [long](Get-SafeValue -InputObject $Account -Name 'id')
    $kind = Get-SafeString -InputObject $Account -Name 'type'
    $platform = Get-SafeString -InputObject $Account -Name 'platform'
    $statsData = $null
    $usageData = $null
    $grokQuotaData = $null
    $queryNote = ''
    try {
        $statsData = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET -Path "/api/v1/admin/accounts/$accountId/stats" -TimeoutSec 10)
    } catch {
        $queryNote = $_.Exception.Message
    }
    if ($kind -eq 'oauth') {
        if ($platform -eq 'grok') {
            try {
                $grokQuotaData = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET `
                    -Path "/api/v1/admin/grok/accounts/$accountId/quota?billing_only=true" -TimeoutSec 10)
                $usageData = $grokQuotaData
            } catch {
                $grokQuotaData = $null
                if (-not $queryNote) { $queryNote = 'Grok billing quota is unavailable; showing local usage statistics.' }
            }
        } else {
            try {
                $usageData = Get-RouterResponseData (Invoke-RouterApi -Session $Session -Method GET `
                    -Path "/api/v1/admin/accounts/$accountId/usage" -TimeoutSec 10)
            } catch {
                if (-not $queryNote) { $queryNote = $_.Exception.Message }
            }
        }
    }

    $providerUsage = $null
    if ($kind -eq 'apikey' -and $null -ne $ConfiguredChannel) {
        $configuredBaseUrl = (Get-ChannelBaseUrl -Channel $ConfiguredChannel).ToLowerInvariant()
        $cacheKey = if ($configuredBaseUrl -match 'ark\.cn-beijing\.volces\.com/api/coding') {
            'volcengine-coding-plan-control-plane'
        } else {
            (Get-SafeString -InputObject $ConfiguredChannel -Name 'credentialName') + '|' + $configuredBaseUrl
        }
        if ($null -ne $CodingPlanCache -and $CodingPlanCache.ContainsKey($cacheKey)) {
            $providerUsage = $CodingPlanCache[$cacheKey]
        } else {
            $providerUsage = Get-CodingPlanUsage -Channel $ConfiguredChannel -UsageCache $UsageCache
            if ($null -eq $providerUsage) {
                $providerUsage = Get-ProviderUsage -Channel $ConfiguredChannel -UsageCache $UsageCache
            }
            if ($null -ne $CodingPlanCache -and $null -ne $providerUsage) { $CodingPlanCache[$cacheKey] = $providerUsage }
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
        $grokQuota = if ($null -ne $grokQuotaData) { $grokQuotaData } else { $usageData }
        foreach ($window in @(ConvertFrom-GrokQuotaUsage -Body $grokQuota)) {
            [void]$windows.Add($window)
        }
        if ($null -eq $grokQuota -and -not $queryNote) {
            $queryNote = 'Grok billing quota is unavailable; showing local usage statistics.'
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
    if ($null -ne $providerUsage) {
        foreach ($window in @($providerUsage.windows)) { [void]$windows.Add($window) }
        if (-not [string]::IsNullOrWhiteSpace([string]$providerUsage.note)) {
            $queryNote = if ($queryNote) { "$queryNote $($providerUsage.note)" } else { [string]$providerUsage.note }
        }
    }

    if ($kind -eq 'oauth' -and $null -ne $UsageCache) {
        $oauthCacheKey = Get-UsageCacheKey -Provider "oauth-$platform" -BaseUrl "account:$accountId"
        if ($windows.Count -gt 0) {
            $cacheResult = [pscustomobject]@{
                provider = $platform
                windows = @($windows)
                note = ''
            }
            $mergeExisting = -not [string]::IsNullOrWhiteSpace($queryNote)
            Save-UsageCacheEntry -Cache $UsageCache -Key $oauthCacheKey -Result $cacheResult -MergeExisting:$mergeExisting
            if ($mergeExisting) {
                $windows.Clear()
                foreach ($window in @($cacheResult.windows)) { [void]$windows.Add($window) }
            }
        } else {
            $fallback = Get-UsageCacheFallback -Cache $UsageCache -Key $oauthCacheKey -Provider $platform -Note "$platform live quota query failed; showing cached quota."
            if ($null -ne $fallback) {
                foreach ($window in @($fallback.windows)) { [void]$windows.Add($window) }
                $queryNote = if ($queryNote) { "$queryNote $($fallback.note)" } else { [string]$fallback.note }
            }
        }
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
        baseUrl = if ($null -ne $ConfiguredChannel) { (Get-ChannelBaseUrl -Channel $ConfiguredChannel).ToLowerInvariant() } else { '' }
        configuredModel = if ($null -ne $ConfiguredChannel) { Get-SafeString -InputObject $ConfiguredChannel -Name 'model' } else { '' }
        totals = Convert-Stats -Stats $statsData
        windows = @($windows)
    }
}

function Merge-UsageTotals {
    param([Parameter(Mandatory)]$Records)
    $models = @{}
    $requests = 0L
    $tokens = 0L
    $cost = 0.0
    foreach ($record in @($Records)) {
        $requests += [long]$record.totals.requests
        $tokens += [long]$record.totals.totalTokens
        $cost += [double]$record.totals.cost
        foreach ($model in @($record.totals.models)) {
            $name = [string]$model.name
            if (-not $name) { continue }
            if (-not $models.ContainsKey($name)) {
                $models[$name] = [ordered]@{ name = $name; requests = 0L; inputTokens = 0L; outputTokens = 0L; cacheReadTokens = 0L; cacheCreationTokens = 0L; totalTokens = 0L; cost = 0.0 }
            }
            foreach ($field in @('requests','inputTokens','outputTokens','cacheReadTokens','cacheCreationTokens','totalTokens')) {
                $models[$name][$field] += [long](Get-SafeNumber -InputObject $model -Name $field)
            }
            $models[$name].cost += [double](Get-SafeNumber -InputObject $model -Name 'cost')
        }
    }
    return [ordered]@{ requests = $requests; totalTokens = $tokens; cost = $cost; models = @($models.Values) }
}

function Merge-CodingPlanApiChannels {
    param([Parameter(Mandatory)]$Records)
    $groups = @{}
    foreach ($record in @($Records)) {
        $baseUrl = Get-SafeString -InputObject $record -Name 'baseUrl'
        $baseUrl = $baseUrl -replace '(?i)/api/(coding|plan)/v\d+(?:/)?$', ''
        $key = "$(Get-SafeString -InputObject $record -Name 'platform')|$($baseUrl.TrimEnd('/').ToLowerInvariant())"
        if (-not $groups.ContainsKey($key)) { $groups[$key] = [System.Collections.ArrayList]::new() }
        [void]$groups[$key].Add($record)
    }
    $merged = @()
    foreach ($group in $groups.Values) {
        if (@($group).Count -eq 1 -or [string]$group[0].platform -ne 'openai' -or [string]$group[0].baseUrl -notmatch '(?i)ark\.cn-beijing\.volces\.com/api/(coding|plan)') {
            $merged += @($group)
            continue
        }
        $first = $group[0]
        $first.name = 'Codex-Router / Volcengine Ark Coding Plan'
        $first.configuredModel = 'ark-coding-plan'
        $first.totals = Merge-UsageTotals -Records $group
        $windowMap = @{}
        foreach ($record in $group) {
            foreach ($window in @($record.windows)) {
                $windowKey = "$(Get-SafeString -InputObject $window -Name 'kind')|$(Get-SafeString -InputObject $window -Name 'displayName')"
                if (-not $windowMap.ContainsKey($windowKey)) { $windowMap[$windowKey] = $window }
            }
        }
        $first.windows = @($windowMap.Values)
        $first.queryNote = (@($group | ForEach-Object { [string]$_.queryNote } | Where-Object { $_ } | Select-Object -Unique) -join ' ')
        $merged += $first
    }
    return @($merged)
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
    $usageCache = Read-UsageMonitorCache
    foreach ($account in $selected) {
        $id = [long](Get-SafeNumber -InputObject $account -Name 'id')
        $configuredModels = if ($modelsByOAuth.ContainsKey($id)) { @($modelsByOAuth[$id]) } else { @() }
        $configuredChannel = if ($apiChannelsByName.ContainsKey([string]$account.name)) { $apiChannelsByName[[string]$account.name] } else { $null }
        $record = Get-AccountRecord -Session $session -Account $account -ConfiguredModels $configuredModels `
            -ConfiguredChannel $configuredChannel -CodingPlanCache $codingPlanCache -UsageCache $usageCache
        if ($record.kind -eq 'oauth') { $subscriptions += $record } else { $apiChannels += $record }
    }
    $apiChannels = @(Merge-CodingPlanApiChannels -Records $apiChannels)
    Write-UsageMonitorCache -Cache $usageCache

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
            $prior = if ($observations.ContainsKey([long]$record.id)) { $observations[[long]$record.id] } else { $null }
            $priorLastProbe = if ($null -eq $prior) { $null } else { $prior.PSObject.Properties['lastProbeAt'] }
            $priorNextProbe = if ($null -eq $prior) { $null } else { $prior.PSObject.Properties['nextProbeAt'] }
            $priorError = if ($null -eq $prior) { $null } else { $prior.PSObject.Properties['recentError'] }
            $observations[[long]$record.id] = [pscustomobject][ordered]@{
                accountId = [long]$record.id
                exhausted = $true
                resetAt = $resetAt
                observedAt = [DateTime]::UtcNow.ToString('o')
                lastProbeAt = if ($null -eq $priorLastProbe) { '' } else { [string]$priorLastProbe.Value }
                nextProbeAt = if ($null -eq $priorNextProbe) { '' } else { [string]$priorNextProbe.Value }
                recentError = if ($null -eq $priorError) { '' } else { [string]$priorError.Value }
            }
            if ($routerGroupId -gt 0) {
                try {
                    [void](Set-RouterAccountGroupMembership -Session $session -AccountId ([long]$record.id) `
                        -GroupId $routerGroupId -Enabled $false -Account $account)
                } catch {
                    # Recovery reconciliation is best-effort and must not discard
                    # an otherwise complete usage snapshot.
                }
            }
        } elseif ($record.windows.Count -gt 0 -and -not $record.queryNote) {
            [void]$observations.Remove([long]$record.id)
        }
    }
    $observationDocument = [ordered]@{ entries = @($observations.Values) } | ConvertTo-Json -Depth 6
    try {
        Write-RouterTextFileAtomic -Path $observationPath -Text $observationDocument
    } catch {
        # A transient state-file lock must not turn live usage into a query error.
    }

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
