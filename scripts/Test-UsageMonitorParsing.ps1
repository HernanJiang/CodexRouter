Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$sourcePath = Join-Path $PSScriptRoot 'Get-UsageMonitor.ps1'
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
    'ConvertTo-IsoFromUnixSeconds',
    'ConvertTo-CodingPlanResetAt',
    'New-CodingPlanWindow',
    'Add-UsageWindow',
    'ConvertFrom-KimiCodingPlanUsage',
    'ConvertFrom-ZhipuCodingPlanUsage',
    'ConvertFrom-MiniMaxCodingPlanUsage',
    'ConvertFrom-ZenMuxCodingPlanUsage'
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

$zhipu = @(ConvertFrom-ZhipuCodingPlanUsage -Body ([pscustomobject]@{
    data = [pscustomobject]@{ limits = @(
        [pscustomobject]@{ type = 'TOKENS_LIMIT'; unit = 6; percentage = 45; nextResetTime = 1786000000000 },
        [pscustomobject]@{ type = 'tokens_limit'; unit = 3; percentage = 10; nextResetTime = 1785751200000 }
    ) }
}))
Assert-True ($zhipu.Count -eq 2) 'Zhipu Coding Plan windows were not parsed.'
Assert-True ($zhipu[0].kind -eq 'fiveHour' -and [double]$zhipu[0].usedPercent -eq 10.0) 'Zhipu unit=3 was not classified as five-hour.'
Assert-True ($zhipu[1].kind -eq 'weekly' -and [double]$zhipu[1].usedPercent -eq 45.0) 'Zhipu unit=6 was not classified as weekly.'

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

Write-Output 'Usage monitor parsing tests passed.'
