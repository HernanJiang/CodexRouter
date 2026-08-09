Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$sourcePath = Join-Path $PSScriptRoot 'Get-UsageMonitor.ps1'
$sourceBytes = [IO.File]::ReadAllBytes($sourcePath)
if (@($sourceBytes | Where-Object { $_ -gt 127 }).Count -gt 0) {
    throw 'Get-UsageMonitor.ps1 must remain ASCII so Windows PowerShell 5.1 can parse the portable runtime script on every system code page.'
}
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    $sourcePath,
    [ref]$tokens,
    [ref]$parseErrors)
if ($parseErrors.Count -gt 0) {
    throw "Get-UsageMonitor.ps1 could not be parsed: $($parseErrors[0].Message)"
}

$requiredFunctions = @(
    'Get-SafeValue',
    'Get-SafeString',
    'Get-SafeNumber',
    'Get-FirstSafeValue',
    'Get-ChannelBaseUrl',
    'ConvertTo-NormalizedPercent',
    'Get-NormalizedWindowPercent',
    'Get-NormalizedResetValue',
    'ConvertTo-KimiWindowMinutes',
    'Get-KimiWindowKind',
    'ConvertTo-ZhipuWindowMinutes',
    'Get-ZhipuUsedPercent',
    'ConvertTo-IsoFromUnixSeconds',
    'ConvertTo-CodingPlanResetAt',
    'New-CodingPlanWindow',
    'New-BalanceUsageWindow',
    'Add-UsageWindow',
    'ConvertFrom-KimiCodingPlanUsage',
    'ConvertFrom-ZhipuCodingPlanUsage',
    'ConvertFrom-MiniMaxCodingPlanUsage',
    'ConvertFrom-ZenMuxCodingPlanUsage',
    'ConvertFrom-VolcengineCodingPlanUsage',
    'ConvertFrom-GrokQuotaUsage',
    'ConvertTo-LowerHex',
    'Get-Sha256Hex',
    'Get-HmacSha256',
    'New-VolcengineSignedHeaders',
    'Get-CodingPlanEndpoint',
    'Get-UsageCacheFallback',
    'ConvertFrom-OpenRouterKeyUsage',
    'ConvertFrom-OpenRouterCredits',
    'ConvertFrom-OpenRouterUsage',
    'ConvertFrom-DeepSeekBalance',
    'ConvertFrom-MimoUsage',
    'Merge-UsageTotals',
    'Merge-CodingPlanApiChannels',
    'Get-UsageWindowCacheKey',
    'Merge-UsageWindows',
    'Resolve-UsageAccountState'
)
foreach ($name in $requiredFunctions) {
    $definition = $ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq $name
        }, $true) | Select-Object -First 1
    if ($null -eq $definition) { throw "Missing usage parser function: $name" }
    Invoke-Expression $definition.Extent.Text
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$missingPercent = [System.Collections.ArrayList]::new()
Add-UsageWindow -Target $missingPercent -Kind 'weekly' -Window ([pscustomobject]@{
    resets_at = '2026-08-10T00:00:00Z'
    remaining_seconds = 3600
})
Assert-True ($missingPercent.Count -eq 0) 'A quota window without a readable percentage was displayed.'

$exhausted = [System.Collections.ArrayList]::new()
Add-UsageWindow -Target $exhausted -Kind 'fiveHour' -Window ([pscustomobject]@{
    used_percent = 100
})
Assert-True ($exhausted.Count -eq 1) 'A readable exhausted quota window was dropped.'
Assert-True ([double]$exhausted[0].usedPercent -eq 100.0) 'Used-percent quota semantics changed unexpectedly.'

$remaining = [System.Collections.ArrayList]::new()
Add-UsageWindow -Target $remaining -Kind 'monthly' -Window ([pscustomobject]@{
    remaining_percent = 25
})
Assert-True ($remaining.Count -eq 1) 'A remaining-percent quota window was dropped.'
Assert-True ([double]$remaining[0].usedPercent -eq 75.0) 'Remaining-percent quota was not normalized to used percent.'
$usagePercentageRatio = Get-NormalizedWindowPercent -Window ([pscustomobject]@{ usagePercentage = 0.4 })
$usagePercentagePercent = Get-NormalizedWindowPercent -Window ([pscustomobject]@{ usagePercentage = 40 })
$usedRatio = Get-NormalizedWindowPercent -Window ([pscustomobject]@{ usedRatio = 0.4 })
Assert-True ([double]$usagePercentageRatio -eq 40.0) 'Fractional usagePercentage was not converted to percent.'
Assert-True ([double]$usagePercentagePercent -eq 40.0) 'Percent-form usagePercentage was multiplied twice.'
Assert-True ([double]$usedRatio -eq 40.0) 'Ratio-form usage was not converted to percent.'
Assert-True ((ConvertTo-CodingPlanResetAt -Value ([DateTime]::SpecifyKind(
    [DateTime]::Parse('2026-08-03T12:00:00'), [DateTimeKind]::Utc))) -eq '2026-08-03T12:00:00.0000000Z') `
    'PowerShell DateTime quota resets were not normalized to UTC ISO format.'

$kimi = @(ConvertFrom-KimiCodingPlanUsage -Body ([pscustomobject]@{
    limits = @([pscustomobject]@{ detail = [pscustomobject]@{ limit = 100; remaining = 25; resetTime = 1785751200000 } })
    usage = [pscustomobject]@{ limit = 1000; remaining = 800; resetTime = 1786000000 }
}))
Assert-True ($kimi.Count -eq 2) 'Kimi Coding Plan windows were not parsed.'
Assert-True ($kimi[0].kind -eq 'fiveHour' -and [double]$kimi[0].usedPercent -eq 75.0) 'Kimi five-hour usage is incorrect.'
Assert-True ($kimi[1].kind -eq 'weekly' -and [double]$kimi[1].usedPercent -eq 20.0) 'Kimi weekly usage is incorrect.'

$kimiExhausted = @(ConvertFrom-KimiCodingPlanUsage -Body ([pscustomobject]@{
    limits = @([pscustomobject]@{ detail = [pscustomobject]@{ limit = 100; remaining = 0 } })
    usage = [pscustomobject]@{ limit = 1000; remaining = 0 }
}))
Assert-True ($kimiExhausted.Count -eq 2) 'Exhausted Kimi windows were not retained.'
Assert-True ([double]$kimiExhausted[0].usedPercent -eq 100.0 -and [double]$kimiExhausted[1].usedPercent -eq 100.0) 'Exhausted Kimi quota did not display zero remaining capacity.'

$kimiDurationWindows = @(ConvertFrom-KimiCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{
        limits = @(
            [pscustomobject]@{
                window = [pscustomobject]@{ duration = 7; timeUnit = 'TIME_UNIT_DAY' }
                detail = [pscustomobject]@{ limit = 1000; remaining = 750; resetTime = 1786000000000 }
            },
            [pscustomobject]@{
                window = [pscustomobject]@{ duration = 300; timeUnit = 'TIME_UNIT_MINUTE' }
                detail = [pscustomobject]@{ limit = 100; remaining = 60; resetTime = 1785751200000 }
            }
        )
    }
}))
Assert-True ($kimiDurationWindows.Count -eq 2) 'Kimi duration-based quota windows were not retained.'
Assert-True ($kimiDurationWindows[0].kind -eq 'fiveHour' -and [double]$kimiDurationWindows[0].usedPercent -eq 40.0) `
    'Kimi duration=300 minutes was not classified as the five-hour window.'
Assert-True ($kimiDurationWindows[1].kind -eq 'weekly' -and [double]$kimiDurationWindows[1].usedPercent -eq 25.0) `
    'Kimi duration=7 days was not classified as the weekly window.'

$kimiRatioWindows = @(ConvertFrom-KimiCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{
        usage = [pscustomobject]@{
            name = 'weekly'
            detail = [pscustomobject]@{ amountUsedRatio = 0.25; expireTime = 1786000000000 }
        }
        limits = @([pscustomobject]@{
            window = [pscustomobject]@{ duration = 5; timeUnit = 'HOUR' }
            quota = [pscustomobject]@{ used_ratio = 0.4; reset_at = 1785751200000 }
        })
    }
}))
Assert-True ($kimiRatioWindows.Count -eq 2) 'Kimi nested detail/quota usage was not parsed.'
Assert-True ($kimiRatioWindows[0].kind -eq 'fiveHour' -and [double]$kimiRatioWindows[0].usedPercent -eq 40.0) `
    'Kimi ratio-based five-hour usage was not normalized.'
Assert-True ($kimiRatioWindows[1].kind -eq 'weekly' -and [double]$kimiRatioWindows[1].usedPercent -eq 25.0) `
    'Kimi nested weekly usage was not normalized.'
$kimiBusinessError = @(ConvertFrom-KimiCodingPlanUsage -Body ([pscustomobject]@{
    code = 1004
    limits = @([pscustomobject]@{ detail = [pscustomobject]@{ limit = 100; remaining = 0 } })
}))
Assert-True ($kimiBusinessError.Count -eq 0) 'Kimi business errors were parsed as quota windows.'

$volcengine = @(ConvertFrom-VolcengineCodingPlanUsage -Body ([pscustomobject]@{
    Result = [pscustomobject]@{ QuotaUsage = @(
        [pscustomobject]@{ Level = 'session'; Percent = 12.5; ResetTimestamp = 1785751200 },
        [pscustomobject]@{ Level = 'weekly'; Percent = 45; ResetTimestamp = 1786000000000 },
        [pscustomobject]@{ Level = 'monthly'; Percent = 67.25; ResetTimestamp = 1787000000 }
    ) }
}))
Assert-True ($volcengine.Count -eq 3) 'Volcengine Coding Plan session/weekly/monthly windows were not parsed.'
Assert-True ($volcengine[0].kind -eq 'fiveHour' -and [double]$volcengine[0].usedPercent -eq 12.5) 'Volcengine session quota was not mapped to five-hour usage.'
Assert-True ($volcengine[1].kind -eq 'weekly' -and [double]$volcengine[1].usedPercent -eq 45.0) 'Volcengine weekly quota is incorrect.'
Assert-True ($volcengine[2].kind -eq 'monthly' -and [double]$volcengine[2].usedPercent -eq 67.25) 'Volcengine monthly quota is incorrect.'
Assert-True ($volcengine[0].resetAt -ne '' -and $volcengine[2].resetAt -ne '') 'Volcengine reset timestamps were not normalized.'

$volcengineHeaders = New-VolcengineSignedHeaders -AccessKeyId 'test-ak' -SecretAccessKey 'test-sk' `
    -Timestamp ([DateTimeOffset]::Parse('2026-08-07T12:34:56Z'))
Assert-True ($volcengineHeaders['X-Date'] -eq '20260807T123456Z') 'Volcengine signature timestamp is incorrect.'
Assert-True ($volcengineHeaders['X-Content-Sha256'] -eq 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855') `
    'Volcengine empty payload hash is incorrect.'
Assert-True ($volcengineHeaders['Content-Type'] -eq 'application/json; charset=UTF-8') 'Volcengine signed content type is incorrect.'
Assert-True ($volcengineHeaders.Authorization -match '^HMAC-SHA256 Credential=test-ak/20260807/cn-beijing/ark/request, SignedHeaders=content-type;host;x-content-sha256;x-date, Signature=[0-9a-f]{64}$') `
    'Volcengine Authorization header does not follow the official signing shape.'

$arkChannels = @(
    [pscustomobject]@{ platform = 'openai'; baseUrl = 'https://ark.cn-beijing.volces.com/api/coding/v3'; name = 'GLM'; configuredModel = 'glm-5.2'; queryNote = ''; totals = [pscustomobject]@{ requests = 1; totalTokens = 10; cost = 0; models = @([pscustomobject]@{ name = 'glm-5.2'; requests = 1; inputTokens = 5; outputTokens = 5; cacheReadTokens = 0; cacheCreationTokens = 0; totalTokens = 10; cost = 0 }) }; windows = @([pscustomobject]@{ kind = 'weekly'; displayName = 'Weekly quota'; usedPercent = $null }) }
    [pscustomobject]@{ platform = 'openai'; baseUrl = 'https://ark.cn-beijing.volces.com/api/coding/v3'; name = 'Kimi'; configuredModel = 'kimi'; queryNote = ''; totals = [pscustomobject]@{ requests = 2; totalTokens = 20; cost = 0; models = @([pscustomobject]@{ name = 'kimi'; requests = 2; inputTokens = 10; outputTokens = 10; cacheReadTokens = 0; cacheCreationTokens = 0; totalTokens = 20; cost = 0 }) }; windows = @([pscustomobject]@{ kind = 'monthly'; displayName = 'Monthly quota'; usedPercent = $null }) }
)
$arkMerged = @(Merge-CodingPlanApiChannels -Records $arkChannels)
Assert-True ($arkMerged.Count -eq 1) 'Ark channels with the same base URL were not aggregated.'
Assert-True ($arkMerged[0].totals.requests -eq 3 -and @($arkMerged[0].totals.models).Count -eq 2) 'Ark aggregate model usage was not merged.'

$usageSource = [IO.File]::ReadAllText($sourcePath)
Assert-True ($usageSource.Contains('/api/v1/admin/grok/accounts/$accountId/quota"')) 'Grok usage does not query the complete quota endpoint.'
Assert-True (-not $usageSource.Contains('quota?billing_only=true')) 'Grok usage still suppresses the active quota-header fallback.'
Assert-True (-not $usageSource.Contains('usage?source=passive')) 'Grok usage still calls the unsupported passive usage endpoint.'
Assert-True ($usageSource.Contains('GetCodingPlanUsage&Version=2024-01-01')) 'Volcengine Coding Plan no longer queries the official control-plane action.'
Assert-True ($usageSource.Contains('Recovery reconciliation is best-effort')) 'OAuth recovery reconciliation can still abort a complete usage snapshot.'
Assert-True ($usageSource.Contains('A transient state-file lock must not turn live usage into a query error.')) 'Usage observation persistence can still abort a complete snapshot.'

$zhipu = @(ConvertFrom-ZhipuCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{ limits = @(
        [pscustomobject]@{ type = 'TOKENS_LIMIT'; unit = 6; percentage = 45; nextResetTime = 1786000000000 },
        [pscustomobject]@{ type = 'tokens_limit'; unit = 3; percentage = 10; nextResetTime = 1785751200000 }
    ) }
}))
Assert-True ($zhipu.Count -eq 2) 'Zhipu Coding Plan windows were not parsed.'
Assert-True ($zhipu[0].kind -eq 'fiveHour' -and [double]$zhipu[0].usedPercent -eq 10.0) 'Zhipu unit=3 was not classified as five-hour.'
Assert-True ($zhipu[1].kind -eq 'weekly' -and [double]$zhipu[1].usedPercent -eq 45.0) 'Zhipu unit=6 was not classified as weekly.'

$zhipuDuration = @(ConvertFrom-ZhipuCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{ limits = @(
        [pscustomobject]@{ type = 'TOKENS_LIMIT'; unit = 1; number = 7; percentage = 65; nextResetTime = 1786000000000 },
        [pscustomobject]@{ type = 'TOKENS_LIMIT'; unit = 3; number = 5; percentage = 15; nextResetTime = 1785751200000 }
    ) }
}))
Assert-True ($zhipuDuration.Count -eq 2) 'Zhipu duration-based windows were not parsed.'
$zhipuMonthly = @(ConvertFrom-ZhipuCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{ limits = @(
        [pscustomobject]@{ type = 'TOKENS_LIMIT'; unit = 3; number = 5; usage = 100; remaining = 50 },
        [pscustomobject]@{ type = 'TIME_LIMIT'; unit = 5; number = 1; usage = 100; remaining = 25 }
    ) }
}) -SubscriptionBody ([pscustomobject]@{ data = @([pscustomobject]@{ next_renew_time = 1787000000000 }) }))
Assert-True (@($zhipuMonthly | Where-Object { $_.kind -eq 'monthly' -and [double]$_.usedPercent -eq 75.0 }).Count -eq 1) `
    'Zhipu TIME_LIMIT monthly quota was not parsed.'
Assert-True ($zhipuDuration[0].kind -eq 'fiveHour' -and [double]$zhipuDuration[0].usedPercent -eq 15.0) `
    'Zhipu unit=3/number=5 was not classified as the five-hour window.'
Assert-True ($zhipuDuration[1].kind -eq 'weekly' -and [double]$zhipuDuration[1].usedPercent -eq 65.0) `
    'Zhipu unit=1/number=7 was not classified as the weekly window.'

$minimax = @(ConvertFrom-MiniMaxCodingPlanUsage -Body ([pscustomobject]@{
    model_remains = @([pscustomobject]@{
        model_name = 'general'
        current_interval_remaining_percent = 35
        end_time = 1785751200000
        current_weekly_status = 1
        current_weekly_remaining_percent = 80
        weekly_end_time = 1786000000000
    })
}))
Assert-True ($minimax.Count -eq 2) 'MiniMax Coding Plan windows were not parsed.'
Assert-True ([double]$minimax[0].usedPercent -eq 65.0) 'MiniMax remaining percentage was not inverted.'
Assert-True ([double]$minimax[1].usedPercent -eq 20.0) 'MiniMax weekly remaining percentage was not inverted.'

$minimaxMissingWeeklyStatus = @(ConvertFrom-MiniMaxCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{ model_remains = @([pscustomobject]@{
        model_name = 'general'
        current_interval_remaining_percent = '90'
        current_weekly_remaining_percent = '55'
    }) }
}))
Assert-True ($minimaxMissingWeeklyStatus.Count -eq 2) 'MiniMax dropped a readable weekly lane when weekly status was absent.'
Assert-True ([double]$minimaxMissingWeeklyStatus[1].usedPercent -eq 45.0) 'MiniMax string weekly remaining percent was not parsed.'
$minimaxPlaceholder = @(ConvertFrom-MiniMaxCodingPlanUsage -Body ([pscustomobject]@{
    base_resp = [pscustomobject]@{ status_code = 0 }
    model_remains = @([pscustomobject]@{
        model_name = 'general'
        current_interval_remaining_percent = 100
        current_interval_status = 3
        current_weekly_remaining_percent = 90
        current_weekly_status = 3
    })
}))
Assert-True ($minimaxPlaceholder.Count -eq 1 -and $minimaxPlaceholder[0].kind -eq 'weekly' -and [double]$minimaxPlaceholder[0].usedPercent -eq 10.0) `
    'MiniMax only suppressed the empty status=3 interval lane instead of retaining a readable weekly lane.'
$minimaxError = @(ConvertFrom-MiniMaxCodingPlanUsage -Body ([pscustomobject]@{
    base_resp = [pscustomobject]@{ status_code = 1004; status_msg = 'login required' }
    model_remains = @([pscustomobject]@{ model_name = 'general'; current_interval_remaining_percent = 10 })
}))
Assert-True ($minimaxError.Count -eq 0) 'MiniMax business errors were parsed as quota windows.'

$openRouterPartial = @(ConvertFrom-OpenRouterUsage `
    -KeyBody ([pscustomobject]@{ data = [pscustomobject]@{ limit = 100; limit_remaining = 35; limit_reset = 'weekly' } }) `
    -CreditsBody $null)
Assert-True ($openRouterPartial.Count -eq 1) 'OpenRouter discarded key limits when credits were unavailable.'
Assert-True ($openRouterPartial[0].kind -eq 'weekly' -and [double]$openRouterPartial[0].usedAmount -eq 65.0) `
    'OpenRouter limit_remaining was not converted to used amount.'
$openRouterDaily = @(ConvertFrom-OpenRouterUsage `
    -KeyBody ([pscustomobject]@{ data = [pscustomobject]@{ limit = 10; usage = 2; limit_reset = 'daily' } }) `
    -CreditsBody $null)
Assert-True ($openRouterDaily.Count -eq 1 -and $openRouterDaily[0].kind -eq 'daily') `
    'OpenRouter daily limits were incorrectly classified as five-hour quota.'

$deepSeekMultiCurrency = ConvertFrom-DeepSeekBalance -Body ([pscustomobject]@{
    balance_infos = @(
        [pscustomobject]@{ currency = 'CNY'; total_balance = '120.50'; topped_up_balance = '20.00'; granted_balance = '100.50' },
        [pscustomobject]@{ currency = 'USD'; total_balance = '18.25'; topped_up_balance = '18.25'; granted_balance = '0.00' }
    )
})
Assert-True ($null -ne $deepSeekMultiCurrency) 'DeepSeek multi-currency balance was not parsed.'
Assert-True ($deepSeekMultiCurrency.currency -eq 'CNY' -and [double]$deepSeekMultiCurrency.remainingAmount -eq 120.5) `
    'DeepSeek did not preserve the provider-funded total balance row.'

$mimoWindows = @(ConvertFrom-MimoUsage `
    -BalanceBody ([pscustomobject]@{ data = [pscustomobject]@{ balance = 8.25; currency = 'USD' } }) `
    -DetailBody ([pscustomobject]@{ data = [pscustomobject]@{ planStatus = 'active'; currentPeriodEnd = '2026-09-01T00:00:00Z' } }) `
    -UsageBody ([pscustomobject]@{ data = [pscustomobject]@{ monthUsage = [pscustomobject]@{ items = @([pscustomobject]@{ name = 'month_total_token'; used = 25; limit = 100 }) } } }))
Assert-True (@($mimoWindows | Where-Object { $_.kind -eq 'balance' -and [double]$_.remainingAmount -eq 8.25 }).Count -eq 1) `
    'MiMo balance was not parsed.'
Assert-True (@($mimoWindows | Where-Object { $_.kind -eq 'monthly' -and [double]$_.usedPercent -eq 25.0 }).Count -eq 1) `
    'MiMo Token Plan usage was not parsed.'
$mimoRatio = @(ConvertFrom-MimoUsage `
    -BalanceBody ([pscustomobject]@{ data = [pscustomobject]@{ balance = 1 } }) `
    -DetailBody ([pscustomobject]@{ data = [pscustomobject]@{ planStatus = 'active' } }) `
    -UsageBody ([pscustomobject]@{ data = [pscustomobject]@{ monthUsage = [pscustomobject]@{ items = @([pscustomobject]@{ name = 'month_total_token'; percent = 0.25 }) } } }))
Assert-True (@($mimoRatio | Where-Object { $_.kind -eq 'monthly' -and [double]$_.usedPercent -eq 25.0 }).Count -eq 1) `
    'MiMo ratio percent was not converted to a used percentage.'
$mimoWrongItem = @(ConvertFrom-MimoUsage `
    -BalanceBody ([pscustomobject]@{ data = [pscustomobject]@{ balance = 1 } }) `
    -DetailBody ([pscustomobject]@{ data = [pscustomobject]@{ planStatus = 'active' } }) `
    -UsageBody ([pscustomobject]@{ data = [pscustomobject]@{ monthUsage = [pscustomobject]@{ items = @([pscustomobject]@{ name = 'other'; used = 1; limit = 2 }) } } }))
Assert-True (@($mimoWrongItem | Where-Object { $_.kind -eq 'monthly' }).Count -eq 0) `
    'MiMo used a non-total item as the monthly Token Plan quota.'
$mimoError = @(ConvertFrom-MimoUsage `
    -BalanceBody ([pscustomobject]@{ code = 401; message = 'expired' }) `
    -DetailBody $null -UsageBody $null)
Assert-True ($mimoError.Count -eq 0) 'MiMo business errors were parsed as balance data.'

$mergedWindows = @(Merge-UsageWindows `
    -Existing @([pscustomobject]@{ kind = 'fiveHour'; usedPercent = 20 }, [pscustomobject]@{ kind = 'weekly'; usedPercent = 30 }) `
    -Incoming @([pscustomobject]@{ kind = 'weekly'; usedPercent = 40 }))
Assert-True ($mergedWindows.Count -eq 2 -and [double]$mergedWindows[0].usedPercent -eq 40 -and [double]$mergedWindows[1].usedPercent -eq 20) `
    'Partial usage refresh did not merge windows without discarding the last good lane.'

$cache = @{
    'provider|base|credential' = [pscustomobject]@{
        updatedAt = [DateTimeOffset]::UtcNow.ToString('o')
        windows = @([pscustomobject]@{ kind = 'weekly'; usedPercent = 42.0 })
    }
}
$cachedUsage = Get-UsageCacheFallback -Cache $cache -Key 'provider|base|credential' -Provider 'Provider' -Note 'Live query failed; showing cached usage.'
Assert-True ($null -ne $cachedUsage -and $cachedUsage.cached -eq $true) 'Last-good usage cache was not returned after a live failure.'
Assert-True (@($cachedUsage.windows).Count -eq 1 -and [double]$cachedUsage.windows[0].usedPercent -eq 42.0) `
    'Last-good usage cache changed the stored quota window.'
$staleCache = @{
    'provider|base|stale' = [pscustomobject]@{
        updatedAt = [DateTimeOffset]::UtcNow.AddHours(-7).ToString('o')
        windows = @([pscustomobject]@{ kind = 'weekly'; usedPercent = 99.0 })
    }
}
Assert-True ($null -eq (Get-UsageCacheFallback -Cache $staleCache -Key 'provider|base|stale' -Provider 'Provider')) `
    'Stale last-good quota data was displayed after the cache TTL.'

$kimiRefreshedState = Resolve-UsageAccountState -Status 'active' -Schedulable $true `
    -StatusDetail 'class=rate_limit status=403' -HasFreshQuotaData $true `
    -Windows @([pscustomobject]@{ kind = 'fiveHour'; usedPercent = 0.0 }, [pscustomobject]@{ kind = 'weekly'; usedPercent = 0.0 })
Assert-True ($kimiRefreshedState.health -eq 'healthy') 'Fresh Kimi zero-percent quota did not restore a healthy state.'
Assert-True ([string]::IsNullOrWhiteSpace($kimiRefreshedState.statusDetail)) 'Fresh Kimi quota retained a historical 403 status detail.'

$kimiActiveFailedRefreshState = Resolve-UsageAccountState -Status 'active' -Schedulable $true `
    -StatusDetail 'class=permission status=403' -HasFreshQuotaData $false -Windows @()
Assert-True ($kimiActiveFailedRefreshState.health -eq 'healthy') 'An active schedulable Kimi channel was not kept healthy.'
Assert-True ([string]::IsNullOrWhiteSpace($kimiActiveFailedRefreshState.statusDetail)) `
    'An active schedulable Kimi channel retained a stale request error beside the current quota error.'

$grokExhaustedState = Resolve-UsageAccountState -Status 'active' -Schedulable $true `
    -StatusDetail 'class=request_failure' -HasFreshQuotaData $true `
    -Windows @([pscustomobject]@{ kind = 'weekly'; usedPercent = 100.0 })
Assert-True ($grokExhaustedState.health -eq 'quotaExhausted') 'Fresh Grok exhausted quota was not classified from the current window.'
Assert-True ([string]::IsNullOrWhiteSpace($grokExhaustedState.statusDetail)) 'Fresh Grok quota retained a historical request failure.'

$failedRefreshState = Resolve-UsageAccountState -Status 'error' -Schedulable $false `
    -StatusDetail 'class=authentication status=401' -HasFreshQuotaData $false -Windows @()
Assert-True ($failedRefreshState.health -eq 'upstreamError') 'A current failed refresh was hidden as healthy.'
Assert-True ($failedRefreshState.statusDetail -eq 'class=authentication status=401') 'A current failed refresh lost its actionable status detail.'

$zenmux = @(ConvertFrom-ZenMuxCodingPlanUsage -Body ([pscustomobject]@{
    success = $true
    data = [pscustomobject]@{
        quota_5_hour = [pscustomobject]@{ usage_percentage = 0.25; resets_at = '2026-08-03T12:00:00Z' }
        quota_7_day = [pscustomobject]@{ usage_percentage = 0.8; resets_at = '2026-08-09T12:00:00Z' }
    }
}))
Assert-True ($zenmux.Count -eq 2) 'ZenMux Coding Plan windows were not parsed.'
Assert-True ([double]$zenmux[0].usedPercent -eq 25.0) 'ZenMux five-hour ratio was not converted to percent.'
Assert-True ([double]$zenmux[1].usedPercent -eq 80.0) 'ZenMux weekly ratio was not converted to percent.'

$safeZenMux = Get-CodingPlanEndpoint -BaseUrl 'https://api.zenmux.ai/api/v1'
Assert-True ($safeZenMux.Provider -eq 'ZenMux Coding Plan') 'The exact ZenMux host was rejected.'
Assert-True ($null -eq (Get-CodingPlanEndpoint -BaseUrl 'https://attacker.example/zenmux')) `
    'A host containing the ZenMux name could receive a selected credential.'
Assert-True ($null -eq (Get-CodingPlanEndpoint -BaseUrl 'https://api.zenmux.ai.attacker.example/v1')) `
    'A suffix-confusion ZenMux host could receive a selected credential.'
Assert-True ($null -eq (Get-CodingPlanEndpoint -BaseUrl 'http://api.zenmux.ai/v1')) `
    'A coding-plan credential could be sent over plaintext HTTP.'
Assert-True ($null -eq (Get-CodingPlanEndpoint -BaseUrl 'https://user@api.zenmux.ai/v1')) `
    'A URL containing userinfo was accepted for a credential-bearing request.'
foreach ($url in @(
    'https://api.kimi.com/v1',
    'https://open.bigmodel.cn/api/paas/v4',
    'https://api.z.ai/api/paas/v4',
    'https://api.minimaxi.com/v1',
    'https://api.minimax.io/v1',
    'https://api.zenmux.ai/v1',
    'https://ark.cn-beijing.volces.com/api/v3'
)) {
    Assert-True ($null -eq (Get-CodingPlanEndpoint -BaseUrl $url)) "普通 API URL 被误判为 Coding Plan: $url"
}
$distinctWindows = @(Merge-UsageWindows `
    -Existing @([pscustomobject]@{ kind = 'weekly'; displayName = 'Plan A'; usedPercent = 20 }) `
    -Incoming @([pscustomobject]@{ kind = 'weekly'; displayName = 'Plan B'; usedPercent = 40 }))
Assert-True ($distinctWindows.Count -eq 2) 'Same-kind quota windows with different identities collided in the cache merge.'

Write-Output 'Usage monitor parsing tests passed.'

# Grok billing-probe shape: real quota lives under `billing`, not at the root.
$grokWindows = [System.Collections.ArrayList]::new()
$grokBilling = [pscustomobject]@{
    period_type = 'weekly'
    usage_percent = 100
    period_end = '2026-08-11T19:33:07.739361+08:00'
    plan = 'SuperGrok'
    used_percent = 30.88
    used_cents = 4632
    monthly_limit_cents = 15000
    billing_period_end = '2026-09-01T08:00:00+08:00'
    product_usage = @([pscustomobject]@{ product = 'GrokBuild'; usage_percent = 100 })
}
Add-UsageWindow -Target $grokWindows -Kind 'weekly' -Window ([ordered]@{
    used_percent = $grokBilling.usage_percent
    resets_at = $grokBilling.period_end
}) -DisplayName $grokBilling.plan
Add-UsageWindow -Target $grokWindows -Kind 'monthly' -Window ([ordered]@{
    used_percent = $grokBilling.used_percent
    resets_at = $grokBilling.billing_period_end
}) -DisplayName 'monthly'
Add-UsageWindow -Target $grokWindows -Kind 'model' -Window ([ordered]@{
    used_percent = $grokBilling.product_usage[0].usage_percent
    resets_at = $grokBilling.period_end
}) -DisplayName $grokBilling.product_usage[0].product
Assert-True ($grokWindows.Count -eq 3) 'Grok billing windows were not parsed.'
Assert-True ($grokWindows[0].kind -eq 'weekly' -and [double]$grokWindows[0].usedPercent -eq 100.0) 'Grok weekly subscription usage is incorrect.'
Assert-True ($grokWindows[0].displayName -eq 'SuperGrok') 'Grok plan name was not shown on the weekly window.'
Assert-True ($grokWindows[1].kind -eq 'monthly' -and [double]$grokWindows[1].usedPercent -eq 30.88) 'Grok monthly spend usage is incorrect.'
Assert-True ($grokWindows[2].kind -eq 'model' -and $grokWindows[2].displayName -eq 'GrokBuild') 'Grok per-product window was dropped.'
Assert-True ($grokWindows[0].resetAt -ne '' -and $grokWindows[1].resetAt -ne '') 'Grok reset times were not parsed.'

$grokQuotaWindows = @(ConvertFrom-GrokQuotaUsage -Body ([pscustomobject]@{
    billing = $grokBilling
    snapshot = [pscustomobject]@{
        tokens = [pscustomobject]@{ limit = 1000000; remaining = 250000; reset_at = '2026-08-08T00:00:00Z' }
        requests = [pscustomobject]@{ limit = 100; remaining = 80; reset_at = '2026-08-08T00:00:00Z' }
    }
}))
Assert-True ($grokQuotaWindows.Count -eq 5) 'Grok billing and active quota-header windows were not combined.'
Assert-True (@($grokQuotaWindows | Where-Object { $_.displayName -eq 'Grok token quota' -and [double]$_.usedPercent -eq 75.0 }).Count -eq 1) `
    'Grok token quota headers were not normalized to used percent.'
Assert-True (@($grokQuotaWindows | Where-Object { $_.displayName -eq 'Grok request quota' -and [double]$_.usedPercent -eq 20.0 }).Count -eq 1) `
    'Grok request quota headers were not normalized to used percent.'

$grokUnifiedWindows = @(ConvertFrom-GrokQuotaUsage -Body ([pscustomobject]@{
    config = [pscustomobject]@{
        creditUsagePercent = 42.5
        currentPeriod = [pscustomobject]@{ type = 'WEEKLY'; end = '2026-08-12T00:00:00Z' }
    }
}))
Assert-True ($grokUnifiedWindows.Count -eq 1 -and $grokUnifiedWindows[0].kind -eq 'weekly' -and [double]$grokUnifiedWindows[0].usedPercent -eq 42.5) `
    'Grok unified credits response was not parsed.'

$grokSource = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'Get-UsageMonitor.ps1') -Raw
Assert-True ($grokSource -match "Name 'billing'") 'Grok quota parsing no longer reads the billing payload.'
Assert-True ($grokSource -notmatch 'quota" -TimeoutSec 4') 'Grok quota probe still uses the too-aggressive 4s timeout.'
Assert-True ($grokSource -match 'product_usage') 'Grok per-product quota parsing was removed.'
