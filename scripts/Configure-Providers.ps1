Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$routerRoot = Split-Path -Parent $PSScriptRoot
Import-Module "$routerRoot\scripts\CredentialStore.psm1" -Force
Import-Module "$routerRoot\scripts\RouterAdmin.psm1" -Force

function New-RandomLocalKey {
    $buffer = [byte[]]::new(32)
    [Security.Cryptography.RandomNumberGenerator]::Fill($buffer)
    try { return 'sk-local-' + ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant() }
    finally { [Array]::Clear($buffer, 0, $buffer.Length) }
}

function New-IdentityMapping([string[]]$Models) {
    $mapping = [ordered]@{}
    foreach ($model in $Models) { $mapping[$model] = $model }
    return $mapping
}

function Set-RouterApiAccount {
    param(
        [Parameter(Mandatory)]$Session,
        [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$ExistingAccounts,
        [Parameter(Mandatory)][long]$GroupId,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][int]$Priority,
        [Parameter(Mandatory)][hashtable]$Credentials,
        [Parameter(Mandatory)][hashtable]$Extra,
        [Parameter(Mandatory)][string]$ProbeModel
    )

    $body = @{
        name = $Name
        platform = 'openai'
        type = 'apikey'
        credentials = $Credentials
        extra = $Extra
        concurrency = 8
        priority = $Priority
        rate_multiplier = 1.0
        group_ids = @($GroupId)
        confirm_mixed_channel_risk = $true
    }
    $existing = $ExistingAccounts | Where-Object { $_.name -eq $Name } | Select-Object -First 1
    if ($existing) {
        [void](Invoke-RouterApi -Session $Session -Method PUT -Path "/api/v1/admin/accounts/$($existing.id)" -Body $body)
        $accountId = [long]$existing.id
        $action = 'updated'
    } else {
        $response = Invoke-RouterApi -Session $Session -Method POST -Path '/api/v1/admin/accounts' -Body $body
        $accountId = [long](Get-RouterResponseData -Response $response).id
        $action = 'created'
    }

    $planId = Set-RouterScheduledRecovery -Session $Session -AccountId $accountId -ModelId $ProbeModel
    return [pscustomobject]@{ Name = $Name; AccountId = $accountId; Priority = $Priority; Action = $action; RecoveryPlanId = $planId }
}

& "$routerRoot\scripts\Start-Router.ps1" | Out-Null

$credentialNames = @('RelayApiKey', 'KimiPrimaryApiKey', 'KimiFallbackApiKey', 'OpenRouterApiKey')
foreach ($credentialName in $credentialNames) {
    if ($null -eq (Get-RouterCredential -Name $credentialName -AllowMissing)) {
        throw "Missing Windows credential: CodexRouter/$credentialName"
    }
}

$session = New-RouterAdminSession
$complianceResponse = Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/compliance'
$compliance = Get-RouterResponseData -Response $complianceResponse
if ($compliance.required) { throw 'Complete the Sub2API compliance confirmation in the local admin page first.' }
$localProxy = Set-RouterLocalAdaptiveProxy -Session $session

$gptModels = @(
    'gpt-5.6-luna',
    'gpt-5.6-sol',
    'gpt-5.6-terra'
)
$kimiModels = @('kimi-for-coding', 'kimi-for-coding-highspeed')
$openRouterModels = @('grok-4.5', 'deepseek-v4-flash')
$publicModels = @($gptModels + $kimiModels + $openRouterModels)

$groupName = 'Codex Unified Router'
$groups = Get-RouterGroups -Session $session
$group = $groups | Where-Object { $_.name -eq $groupName } | Select-Object -First 1
$groupBody = @{
    name = $groupName
    description = 'Local Codex model router: Plus first for supported GPT models, then API providers.'
    platform = 'openai'
    rate_multiplier = 1.0
    is_exclusive = $false
    subscription_type = 'standard'
    status = 'active'
    allow_messages_dispatch = $false
    allow_live = $false
    require_oauth_only = $false
    models_list_config = @{ enabled = $true; models = $publicModels }
}
if ($group) {
    $groupResponse = Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/groups/$($group.id)" -Body $groupBody
    $group = Get-RouterResponseData -Response $groupResponse
    $groupAction = 'updated'
} else {
    $groupResponse = Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/groups' -Body $groupBody -IdempotencyKey 'codex-unified-router-group-v1'
    $group = Get-RouterResponseData -Response $groupResponse
    $groupAction = 'created'
}
$groupId = [long]$group.id

$accounts = @(Get-RouterAccounts -Session $session)
$plusAccount = $accounts | Where-Object { $_.name -eq 'ChatGPT Plus OAuth' } | Select-Object -First 1
if ($plusAccount) {
    Set-RouterAccountProxy -Session $session -AccountId ([long]$plusAccount.id) -ProxyId $localProxy.Id
    $plusRecoveryPlanId = Set-RouterScheduledRecovery `
        -Session $session `
        -AccountId ([long]$plusAccount.id) `
        -ModelId 'gpt-5.6-sol'
}
$results = [Collections.Generic.List[object]]::new()
$relayKey = Get-RouterCredential -Name 'RelayApiKey'
$kimiPrimaryKey = Get-RouterCredential -Name 'KimiPrimaryApiKey'
$kimiFallbackKey = Get-RouterCredential -Name 'KimiFallbackApiKey'
$openRouterKey = Get-RouterCredential -Name 'OpenRouterApiKey'
try {
    $results.Add((Set-RouterApiAccount `
        -Session $session `
        -ExistingAccounts $accounts `
        -GroupId $groupId `
        -Name '430123 GPT Fallback' `
        -Priority 100 `
        -Credentials @{
            base_url = 'https://api.430123.xyz/v1'
            api_key = $relayKey
            model_mapping = (New-IdentityMapping -Models $gptModels)
        } `
        -Extra @{} `
        -ProbeModel 'gpt-5.6-sol'))

    $kimiMapping = New-IdentityMapping -Models $kimiModels
    $results.Add((Set-RouterApiAccount `
        -Session $session `
        -ExistingAccounts $accounts `
        -GroupId $groupId `
        -Name 'Kimi Coding Primary' `
        -Priority 10 `
        -Credentials @{
            base_url = 'https://api.kimi.com/coding/v1'
            api_key = $kimiPrimaryKey
            model_mapping = $kimiMapping
            openai_capabilities = @('chat_completions')
        } `
        -Extra @{ openai_responses_mode = 'force_chat_completions' } `
        -ProbeModel 'kimi-for-coding'))

    $results.Add((Set-RouterApiAccount `
        -Session $session `
        -ExistingAccounts $accounts `
        -GroupId $groupId `
        -Name 'Kimi Coding Fallback' `
        -Priority 20 `
        -Credentials @{
            base_url = 'https://api.kimi.com/coding/v1'
            api_key = $kimiFallbackKey
            model_mapping = $kimiMapping
            openai_capabilities = @('chat_completions')
        } `
        -Extra @{ openai_responses_mode = 'force_chat_completions' } `
        -ProbeModel 'kimi-for-coding'))

    $results.Add((Set-RouterApiAccount `
        -Session $session `
        -ExistingAccounts $accounts `
        -GroupId $groupId `
        -Name 'OpenRouter Selected Models' `
        -Priority 10 `
        -Credentials @{
            base_url = 'https://openrouter.ai/api/v1'
            api_key = $openRouterKey
            model_mapping = [ordered]@{
                'grok-4.5' = 'x-ai/grok-4.5'
                'deepseek-v4-flash' = 'deepseek/deepseek-v4-flash'
            }
            openai_capabilities = @('chat_completions')
        } `
        -Extra @{ openai_responses_mode = 'force_chat_completions' } `
        -ProbeModel 'grok-4.5'))

} finally {
    $relayKey = $null
    $kimiPrimaryKey = $null
    $kimiFallbackKey = $null
    $openRouterKey = $null
    $kimiMapping = $null
}

$keyResponse = Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/keys?page=1&page_size=200'
$keyData = Get-RouterResponseData -Response $keyResponse
$apiKeys = if ($null -ne $keyData.PSObject.Properties['items']) { @($keyData.items) } else { @($keyData) }
$localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
$existingLocalKey = $null
if ($localKey) { $existingLocalKey = $apiKeys | Where-Object { $_.key -eq $localKey } | Select-Object -First 1 }
if (-not $existingLocalKey) {
    if (-not $localKey) {
        $namedKey = $apiKeys | Where-Object { $_.name -eq $groupName -and $_.key -like 'sk-*' } | Select-Object -First 1
        if ($namedKey) { $localKey = [string]$namedKey.key }
    }
    if (-not $localKey) {
        $localKey = New-RandomLocalKey
        Set-RouterCredential -Name 'LocalApiKey' -Secret $localKey
    }

    $existingLocalKey = $apiKeys | Where-Object { $_.key -eq $localKey } | Select-Object -First 1
    if (-not $existingLocalKey) {
        $createKeyResponse = Invoke-RouterApi `
            -Session $session `
            -Method POST `
            -Path '/api/v1/keys' `
            -Body @{ name = $groupName; group_id = $groupId; custom_key = $localKey; quota = 0 } `
            -IdempotencyKey 'codex-unified-router-local-key-v1'
        $existingLocalKey = Get-RouterResponseData -Response $createKeyResponse
    }
}
if ((Get-RouterCredential -Name 'LocalApiKey' -AllowMissing) -ne $localKey) {
    Set-RouterCredential -Name 'LocalApiKey' -Secret $localKey
}

[pscustomobject]@{
    Group = $groupName
    GroupId = $groupId
    GroupAction = $groupAction
    Models = $publicModels.Count
    LocalApiKey = 'stored in Windows Credential Manager'
} | Format-List
$localProxy | Format-List
if ($plusAccount) {
    [pscustomobject]@{
        Account = [string]$plusAccount.name
        AccountId = [long]$plusAccount.id
        ProxyId = $localProxy.Id
        RecoveryPlanId = $plusRecoveryPlanId
    } | Format-List
}
$results | Sort-Object Priority, Name | Format-Table -AutoSize

$localKey = $null
$session.Headers.Clear()
