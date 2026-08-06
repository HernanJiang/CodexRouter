Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force

function Get-SafePropertyValue {
    param(
        [Parameter()][AllowNull()]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )
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
    param(
        [Parameter()][AllowNull()]$InputObject,
        [Parameter(Mandatory)][string]$Name
    )
    $value = Get-SafePropertyValue -InputObject $InputObject -Name $Name
    if ($null -eq $value) { return '' }
    return [string]$value
}

function Get-SafeLong {
    param(
        [Parameter()][AllowNull()]$Value,
        [long]$Default = 0
    )
    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) { return $Default }
    try { return [long]$Value } catch { return $Default }
}

function Test-OAuthRouterHealth {
    param([int]$TimeoutMilliseconds = 1500)
    $request = $null
    $response = $null
    try {
        $baseUri = Get-RouterBaseUri
        $request = [Net.HttpWebRequest]::Create("$baseUri/health")
        $request.Method = 'GET'
        $request.Proxy = $null
        $request.Timeout = $TimeoutMilliseconds
        $request.ReadWriteTimeout = $TimeoutMilliseconds
        $request.KeepAlive = $false
        $response = [Net.HttpWebResponse]$request.GetResponse()
        return [int]$response.StatusCode -eq 200
    } catch {
        return $false
    } finally {
        if ($null -ne $response) { $response.Dispose() }
    }
}

function Wait-OAuthRouterReady {
    # Only wait for /health here. Creating admin sessions during the wait loop
    # used to trip Sub2API login rate limits and make OAuth loads flaky.
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        if (Test-OAuthRouterHealth) { return }
        Start-Sleep -Milliseconds 300
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: local Router health check failed.'
}

function New-OAuthAdminSessionWithRetry {
    $delays = @(0, 600, 1500)
    $lastError = $null
    foreach ($delay in $delays) {
        if ($delay -gt 0) { Start-Sleep -Milliseconds $delay }
        try {
            return New-RouterAdminSession
        } catch {
            $lastError = $_
            $message = [string]$_.Exception.Message
            if ($message -match '(?i)rate-limited|429|too many requests') {
                Start-Sleep -Milliseconds 2000
            }
            if ($message -notmatch '(?i)token|login|unauthorized|429|rate|refused|timeout|closed|unavailable|503') {
                throw
            }
        }
    }
    if ($null -ne $lastError) { throw $lastError }
    throw 'ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: admin login failed without a specific error.'
}

function Invoke-OAuthAdminRead {
    param([Parameter(Mandatory)][scriptblock]$Operation)
    $delays = @(0, 250, 600, 1200, 2000)
    $lastError = $null
    for ($attempt = 0; $attempt -lt $delays.Count; $attempt++) {
        if ($delays[$attempt] -gt 0) { Start-Sleep -Milliseconds $delays[$attempt] }
        try {
            return & $Operation
        } catch {
            $lastError = $_
            $message = [string]$_.Exception.Message
            if ($message -match '(?i)401|unauthorized|access token|login|no access token') {
                # Admin JWT can expire mid-list; reopen the session and retry.
                try {
                    if ($script:OAuthSession -and $script:OAuthSession.Headers) {
                        $script:OAuthSession.Headers.Clear()
                    }
                    $script:OAuthSession = New-OAuthAdminSessionWithRetry
                } catch {
                    $lastError = $_
                }
            }
        }
    }
    if ($null -ne $lastError) { throw $lastError }
    throw 'ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: admin read failed without a specific error.'
}

function ConvertTo-ModelList {
    param([AllowNull()]$ModelData)
    if ($null -eq $ModelData) { return @() }
    if ($ModelData -is [System.Collections.IEnumerable] -and -not ($ModelData -is [string])) {
        $asObject = $ModelData
        if ($null -ne $asObject.PSObject -and $null -ne $asObject.PSObject.Properties['items']) {
            return @($asObject.items)
        }
        return @($ModelData)
    }
    if ($null -ne $ModelData.PSObject.Properties['items']) {
        return @($ModelData.items)
    }
    return @($ModelData)
}

$script:OAuthSession = $null
try {
    Wait-OAuthRouterReady
    $script:OAuthSession = New-OAuthAdminSessionWithRetry
    $groups = @(Invoke-OAuthAdminRead -Operation { Get-RouterGroups -Session $script:OAuthSession })
    $routerGroup = $groups | Where-Object { (Get-SafeString -InputObject $_ -Name 'name') -eq 'Codex-Router' } | Select-Object -First 1
    $routerGroupId = Get-SafeLong -Value (Get-SafePropertyValue -InputObject $routerGroup -Name 'id')
    $accounts = @(Invoke-OAuthAdminRead -Operation {
        Get-RouterAccounts -Session $script:OAuthSession | Where-Object { (Get-SafeString -InputObject $_ -Name 'type') -eq 'oauth' }
    })

    $result = [System.Collections.Generic.List[object]]::new()
    $detailFailures = 0
    foreach ($account in $accounts) {
        try {
            $accountId = Get-SafeLong -Value (Get-SafePropertyValue -InputObject $account -Name 'id')
            if ($accountId -le 0) { continue }
            $detail = Invoke-OAuthAdminRead -Operation {
                Get-RouterResponseData (Invoke-RouterApi -Session $script:OAuthSession -Method GET -Path "/api/v1/admin/accounts/$accountId")
            }
            $platform = Get-SafeString -InputObject $detail -Name 'platform'
            $models = [System.Collections.Generic.List[object]]::new()
            $seenModelIds = @{}
            try {
                $modelData = Invoke-OAuthAdminRead -Operation {
                    Get-RouterResponseData (Invoke-RouterApi -Session $script:OAuthSession -Method GET -Path "/api/v1/admin/accounts/$accountId/models")
                }
                foreach ($entry in @(ConvertTo-ModelList -ModelData $modelData)) {
                    $modelId = Get-SafeString -InputObject $entry -Name 'id'
                    if ($platform -eq 'openai' -and $modelId -eq 'gpt-5.6') {
                        $modelId = 'gpt-5.6-sol'
                    }
                    if (-not $modelId -or $seenModelIds.ContainsKey($modelId)) { continue }
                    $seenModelIds[$modelId] = $true
                    $displayName = Get-SafeString -InputObject $entry -Name 'display_name'
                    if (-not $displayName) { $displayName = Get-SafeString -InputObject $entry -Name 'displayName' }
                    if ($modelId -eq 'gpt-5.6-sol') { $displayName = 'ChatGPT-5.6-Sol' }
                    [void]$models.Add([ordered]@{
                        id = $modelId
                        displayName = if ($displayName) { $displayName } else { $modelId }
                        suggested = $false
                    })
                }
            } catch {
                $models.Clear()
                $seenModelIds = @{}
            }

            foreach ($suggestedModel in @(Get-RouterOAuthModelSuggestions -Platform $platform)) {
                if ($seenModelIds.ContainsKey([string]$suggestedModel.id)) { continue }
                $seenModelIds[[string]$suggestedModel.id] = $true
                [void]$models.Add([ordered]@{
                    id = [string]$suggestedModel.id
                    displayName = [string]$suggestedModel.displayName
                    suggested = $true
                })
            }

            $credentials = Get-SafePropertyValue -InputObject $detail -Name 'credentials'
            $extra = Get-SafePropertyValue -InputObject $detail -Name 'extra'
            $credentialEmail = Get-SafeString -InputObject $credentials -Name 'email'
            if (-not $credentialEmail) { $credentialEmail = Get-SafeString -InputObject $extra -Name 'email' }
            $plan = Get-SafeString -InputObject $credentials -Name 'plan_type'
            if (-not $plan) { $plan = Get-SafeString -InputObject $credentials -Name 'tier_id' }
            if (-not $plan) { $plan = Get-SafeString -InputObject $extra -Name 'subscription_tier' }
            $groupIds = @()
            foreach ($groupId in @(Get-SafePropertyValue -InputObject $detail -Name 'group_ids')) {
                $parsedGroupId = Get-SafeLong -Value $groupId -Default -1
                if ($parsedGroupId -ge 0) { $groupIds += $parsedGroupId }
            }
            # Compact list payloads sometimes omit group_ids even when the account
            # still belongs to the Router group. Fall back to the summary value.
            if ($groupIds.Count -eq 0) {
                foreach ($groupId in @(Get-SafePropertyValue -InputObject $account -Name 'group_ids')) {
                    $parsedGroupId = Get-SafeLong -Value $groupId -Default -1
                    if ($parsedGroupId -ge 0) { $groupIds += $parsedGroupId }
                }
            }
            $expiresAt = Get-SafeString -InputObject $detail -Name 'expires_at'
            if (-not $expiresAt) { $expiresAt = Get-SafeString -InputObject $credentials -Name 'expires_at' }
            # Some providers store unix seconds; normalize for the UI.
            if ($expiresAt -match '^\d{9,12}$') {
                try {
                    $expiresAt = [DateTimeOffset]::FromUnixTimeSeconds([long]$expiresAt).UtcDateTime.ToString('o')
                } catch { }
            }
            $priorityValue = Get-SafePropertyValue -InputObject $detail -Name 'priority'
            $priority = 0
            if ($null -ne $priorityValue -and -not [string]::IsNullOrWhiteSpace([string]$priorityValue)) {
                try { $priority = [int]$priorityValue } catch { $priority = 0 }
            }
            $errorMessage = Get-SafeString -InputObject $detail -Name 'error_message'
            if (-not $errorMessage) {
                $errorMessage = Get-SafeString -InputObject $detail -Name 'temp_unschedulable_reason'
            }
            [void]$result.Add([ordered]@{
                id = Get-SafeLong -Value (Get-SafePropertyValue -InputObject $detail -Name 'id') -Default $accountId
                name = Get-SafeString -InputObject $detail -Name 'name'
                platform = $platform
                status = Get-SafeString -InputObject $detail -Name 'status'
                email = $credentialEmail
                plan = $plan
                priority = $priority
                boundToRouter = $routerGroupId -gt 0 -and $groupIds -contains $routerGroupId
                error = $errorMessage
                expiresAt = $expiresAt
                models = @($models)
            })
        } catch {
            $detailFailures++
            continue
        }
    }

    if ($accounts.Count -gt 0 -and $result.Count -eq 0 -and $detailFailures -gt 0) {
        throw "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: failed to load details for $detailFailures OAuth account(s)."
    }

    $json = ConvertTo-Json -InputObject @($result.ToArray()) -Depth 12 -Compress
    if ([string]::IsNullOrWhiteSpace($json)) { $json = '[]' }
    $utf8 = [Text.UTF8Encoding]::new($false)
    [Console]::OutputEncoding = $utf8
    [Console]::Out.Write($json)
} catch {
    $message = [string]$_.Exception.Message
    if ([string]::IsNullOrWhiteSpace($message)) { $message = [string]$_ }
    if ($message -notmatch 'ROUTER_OAUTH_ACCOUNTS_') {
        $message = "ROUTER_OAUTH_ACCOUNTS_UNAVAILABLE: $message"
    }
    [Console]::Error.WriteLine($message)
    exit 1
} finally {
    if ($script:OAuthSession -and $script:OAuthSession.Headers) { $script:OAuthSession.Headers.Clear() }
}
