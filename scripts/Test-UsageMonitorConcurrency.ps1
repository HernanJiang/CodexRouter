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

$definition = $ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Invoke-UsageTasksBounded'
    }, $true) | Select-Object -First 1
if ($null -eq $definition) {
    throw 'Missing bounded usage task scheduler.'
}
Invoke-Expression $definition.Extent.Text

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

$events = [System.Collections.ArrayList]::new()
$worker = {
    param($Task)
    if ([int]$Task.id -eq 1) { Start-Sleep -Milliseconds 1800 }
    else { Start-Sleep -Milliseconds 80 }
    return [pscustomobject]@{ id = [int]$Task.id; finishedAt = [DateTime]::UtcNow }
}
$onResult = {
    param($Envelope)
    [void]$events.Add([pscustomobject]@{
        id = [int]$Envelope.result.id
        elapsedMs = [long](([DateTime]::UtcNow - $startedAt).TotalMilliseconds)
    })
}

$startedAt = [DateTime]::UtcNow
$results = @(Invoke-UsageTasksBounded -Tasks @(
        [pscustomobject]@{ id = 1 }
        [pscustomobject]@{ id = 2 }
        [pscustomobject]@{ id = 3 }
    ) -WorkerScript $worker -MaxConcurrency 2 -OnResult $onResult)

Assert-True ($results.Count -eq 3) 'All usage providers must produce an isolated task result.'
Assert-True ($events.Count -eq 3) 'Each completed provider must be published exactly once.'
Assert-True ([int]$events[0].id -ne 1 -and [long]$events[0].elapsedMs -lt 1200) `
    'A slow provider blocked the first fast usage result.'

$boundedStartedAt = [DateTime]::UtcNow
$boundedResults = @(Invoke-UsageTasksBounded -Tasks @(
        [pscustomobject]@{ id = 11; delayMs = 650 }
        [pscustomobject]@{ id = 12; delayMs = 650 }
        [pscustomobject]@{ id = 13; delayMs = 650 }
        [pscustomobject]@{ id = 14; delayMs = 650 }
    ) -WorkerScript {
        param($Task)
        Start-Sleep -Milliseconds ([int]$Task.delayMs)
        [pscustomobject]@{ id = [int]$Task.id }
    } -MaxConcurrency 2)
$boundedElapsedMs = [long](([DateTime]::UtcNow - $boundedStartedAt).TotalMilliseconds)
Assert-True ($boundedResults.Count -eq 4) 'Bounded execution lost a usage provider result.'
Assert-True ($boundedElapsedMs -ge 900 -and $boundedElapsedMs -lt 2200) `
    'Usage task scheduler did not enforce the configured concurrency bound.'

$queuedTimeoutResults = @(Invoke-UsageTasksBounded -Tasks @(
        [pscustomobject]@{ id = 21; delayMs = 850 }
        [pscustomobject]@{ id = 22; delayMs = 350 }
    ) -WorkerScript {
        param($Task)
        Start-Sleep -Milliseconds ([int]$Task.delayMs)
        [pscustomobject]@{ id = [int]$Task.id }
    } -MaxConcurrency 1 -TaskTimeoutSec 1)
$queuedSecond = @($queuedTimeoutResults | Where-Object { [int]$_.task.id -eq 22 })
Assert-True ($queuedSecond.Count -eq 1 -and -not [bool]$queuedSecond[0].timedOut) `
    'A queued provider consumed its timeout before its worker actually started.'
Assert-True ([int]$queuedSecond[0].result.id -eq 22) 'The queued provider result was lost after waiting for a worker slot.'
Write-Output 'usage concurrency regression passed'
