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

$session = New-RouterAdminSession
try {
    $groups = @(Get-RouterGroups -Session $session)
    $routerGroup = $groups | Where-Object { (Get-SafeString -InputObject $_ -Name 'name') -eq 'Codex-Router' } | Select-Object -First 1
    $routerGroupIdValue = Get-SafePropertyValue -InputObject $routerGroup -Name 'id'
    $routerGroupId = if ($null -ne $routerGroupIdValue) { [long]$routerGroupIdValue } else { 0 }
    $accounts = @(Get-RouterAccounts -Session $session | Where-Object { (Get-SafeString -InputObject $_ -Name 'type') -eq 'oauth' })
    $result = foreach ($account in $accounts) {
        $accountId = [long](Get-SafePropertyValue -InputObject $account -Name 'id')
        $detail = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId")
        $platform = Get-SafeString -InputObject $detail -Name 'platform'
        $models = @()
        try {
            $modelData = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path "/api/v1/admin/accounts/$accountId/models")
            $seenModelIds = @{}
            $models = @($modelData | ForEach-Object {
                $modelId = Get-SafeString -InputObject $_ -Name 'id'
                if ($platform -eq 'openai' -and $modelId -eq 'gpt-5.6') {
                    $modelId = 'gpt-5.6-sol'
                }
                if (-not $modelId -or $seenModelIds.ContainsKey($modelId)) { return }
                $seenModelIds[$modelId] = $true
                $displayName = Get-SafeString -InputObject $_ -Name 'display_name'
                if ($modelId -eq 'gpt-5.6-sol') { $displayName = 'ChatGPT-5.6-Sol' }
                [ordered]@{
                    id = $modelId
                    displayName = if ($displayName) { $displayName } else { $modelId }
                    suggested = $false
                }
            })
        } catch {
            $models = @()
        }
        # Provider discovery can lag behind the known Codex/provider catalog.
        # Preserve every model returned by Sub2API, then append only missing
        # suggestions. Suggested access is still verified by the first request.
        foreach ($suggestedModel in @(Get-RouterOAuthModelSuggestions -Platform $platform)) {
            if ($models.id -notcontains $suggestedModel.id) {
                $models += [ordered]@{
                    id = $suggestedModel.id
                    displayName = $suggestedModel.displayName
                    suggested = $true
                }
            }
        }
        $credentials = Get-SafePropertyValue -InputObject $detail -Name 'credentials'
        $extra = Get-SafePropertyValue -InputObject $detail -Name 'extra'
        $credentialEmail = Get-SafeString -InputObject $credentials -Name 'email'
        if (-not $credentialEmail) { $credentialEmail = Get-SafeString -InputObject $extra -Name 'email' }
        $plan = Get-SafeString -InputObject $credentials -Name 'plan_type'
        if (-not $plan) { $plan = Get-SafeString -InputObject $credentials -Name 'tier_id' }
        if (-not $plan) { $plan = Get-SafeString -InputObject $extra -Name 'subscription_tier' }
        $groupIds = @(Get-SafePropertyValue -InputObject $detail -Name 'group_ids')
        $expiresAt = Get-SafeString -InputObject $detail -Name 'expires_at'
        if (-not $expiresAt) { $expiresAt = Get-SafeString -InputObject $credentials -Name 'expires_at' }
        [ordered]@{
            id = [long](Get-SafePropertyValue -InputObject $detail -Name 'id')
            name = Get-SafeString -InputObject $detail -Name 'name'
            platform = $platform
            status = Get-SafeString -InputObject $detail -Name 'status'
            email = $credentialEmail
            plan = $plan
            priority = [int](Get-SafePropertyValue -InputObject $detail -Name 'priority')
            boundToRouter = $routerGroupId -gt 0 -and $groupIds -contains $routerGroupId
            error = Get-SafeString -InputObject $detail -Name 'error_message'
            expiresAt = $expiresAt
            models = $models
        }
    }
    @($result) | ConvertTo-Json -Depth 12 -Compress
} finally {
    if ($session -and $session.Headers) { $session.Headers.Clear() }
}
