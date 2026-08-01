Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $routerRoot 'codex-router-config.json'
if (-not (Test-Path -LiteralPath $configPath)) { throw "Configuration not found: $configPath" }

Import-Module (Join-Path $PSScriptRoot 'CredentialStore.psm1') -Force
Import-Module (Join-Path $PSScriptRoot 'RouterAdmin.psm1') -Force
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$models = @($config.models)
if ($models.Count -eq 0) { throw 'At least one model is required.' }

function New-RandomLocalKey {
    $buffer = [byte[]]::new(32)
    [Security.Cryptography.RandomNumberGenerator]::Fill($buffer)
    try { return 'sk-local-' + ([BitConverter]::ToString($buffer)).Replace('-', '').ToLowerInvariant() }
    finally { [Array]::Clear($buffer, 0, $buffer.Length) }
}

function Set-TopLevelTomlValue {
    param([string]$Content, [string]$Key, [string]$Value)
    $pattern = '(?m)^' + [Text.RegularExpressions.Regex]::Escape($Key) + '\s*=.*$'
    $line = "$Key = $Value"
    if ($Content -match $pattern) { return [Text.RegularExpressions.Regex]::Replace($Content, $pattern, $line, 1) }
    $firstTable = [Text.RegularExpressions.Regex]::Match($Content, '(?m)^\[')
    if ($firstTable.Success) { return $Content.Insert($firstTable.Index, "$line`r`n") }
    if ([string]::IsNullOrWhiteSpace($Content)) { return "$line`r`n" }
    return $Content.TrimEnd() + "`r`n$line`r`n"
}

function Escape-TomlString([string]$Value) {
    return $Value.Replace('\', '\\').Replace('"', '\"')
}

Write-Output '[1/7] Initializing local credentials and database...'
& (Join-Path $PSScriptRoot 'Initialize-Router.ps1')
Write-Output '[2/7] Starting PostgreSQL, Redis, and Sub2API...'
& (Join-Path $PSScriptRoot 'Start-Router.ps1')
Write-Output '[3/7] Local services are ready; signing in to the admin API...'
$session = New-RouterAdminSession
Write-Output '[4/7] Checking Sub2API compliance status...'
$compliance = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/admin/compliance')
if ($compliance.required) {
    $acceptedProperty = $config.PSObject.Properties['acceptCompliance']
    if ($null -eq $acceptedProperty -or -not [bool]$acceptedProperty.Value) {
        throw 'You must read and accept the Sub2API deployment and operation compliance commitment in the configurator.'
    }
    [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/compliance/accept' -Body @{
        phrase = [string]$compliance.ack_phrase_zh
        language = 'zh'
    })
    Write-Output 'Sub2API compliance acknowledgement recorded for this local administrator.'
}

$modelNames = @($models | ForEach-Object { [string]$_.model } | Where-Object { $_ })
Write-Output '[5/7] Creating or updating model channels...'
$groupName = 'Codex-Router'
$groups = @(Get-RouterGroups -Session $session)
$group = $groups | Where-Object { $_.name -eq $groupName } | Select-Object -First 1
$groupBody = @{
    name = $groupName
    description = 'Single-user local Codex multi-model router managed by Codex-Router.'
    platform = 'openai'
    rate_multiplier = 1.0
    is_exclusive = $false
    subscription_type = 'standard'
    status = 'active'
    allow_messages_dispatch = $false
    allow_live = $false
    require_oauth_only = $false
    models_list_config = @{ enabled = $true; models = $modelNames }
}
if ($group) {
    $group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/groups/$($group.id)" -Body $groupBody)
} else {
    $group = Get-RouterResponseData (Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/groups' -Body $groupBody -IdempotencyKey 'codex-router-group-v2')
}
$groupId = [long]$group.id
$existingAccounts = @(Get-RouterAccounts -Session $session)

foreach ($model in $models) {
    $credentialName = [string]$model.credentialName
    if ([string]::IsNullOrWhiteSpace($credentialName)) { throw "Model '$($model.model)' has no credential reference." }
    $apiKey = Get-RouterCredential -Name $credentialName -AllowMissing
    if ([string]::IsNullOrWhiteSpace($apiKey)) { throw "Missing API Key for model '$($model.model)'. Edit the model and enter its API Key." }
    try {
        $accountName = 'Codex-Router / ' + $(if ($model.alias) { [string]$model.alias } else { [string]$model.model })
        $mapping = [ordered]@{}
        $mapping[[string]$model.model] = [string]$model.model
        $credentials = @{
            base_url = ([string]$model.baseURL).TrimEnd('/')
            api_key = $apiKey
            model_mapping = $mapping
        }
        $extra = @{}
        if ($model.extra -and ([string]$model.extra).Trim() -ne '{}') {
            $extraObject = ([string]$model.extra) | ConvertFrom-Json
            foreach ($property in $extraObject.PSObject.Properties) { $extra[$property.Name] = $property.Value }
        }
        if (([string]$model.model) -match '(?i)kimi|moonshot') {
            $credentials.openai_capabilities = @('chat_completions')
            if (-not $extra.ContainsKey('openai_responses_mode')) { $extra.openai_responses_mode = 'force_chat_completions' }
        }
        $body = @{
            name = $accountName
            platform = 'openai'
            type = 'apikey'
            credentials = $credentials
            extra = $extra
            concurrency = 8
            priority = [int]$model.priority
            rate_multiplier = 1.0
            group_ids = @($groupId)
            confirm_mixed_channel_risk = $true
        }
        $existing = $existingAccounts | Where-Object { $_.name -eq $accountName } | Select-Object -First 1
        if ($existing) {
            [void](Invoke-RouterApi -Session $session -Method PUT -Path "/api/v1/admin/accounts/$($existing.id)" -Body $body)
            Write-Output "Updated channel: $accountName"
        } else {
            [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/admin/accounts' -Body $body)
            Write-Output "Created channel: $accountName"
        }
    } finally {
        $apiKey = $null
    }
}

$localKey = Get-RouterCredential -Name 'LocalApiKey' -AllowMissing
if ([string]::IsNullOrWhiteSpace($localKey)) {
    $localKey = New-RandomLocalKey
    Set-RouterCredential -Name 'LocalApiKey' -Secret $localKey
}
$keyResponse = Invoke-RouterApi -Session $session -Method GET -Path '/api/v1/keys?page=1&page_size=200'
$keyData = Get-RouterResponseData $keyResponse
$keys = if ($null -ne $keyData.PSObject.Properties['items']) { @($keyData.items) } else { @($keyData) }
if (-not ($keys | Where-Object { $_.key -eq $localKey } | Select-Object -First 1)) {
    [void](Invoke-RouterApi -Session $session -Method POST -Path '/api/v1/keys' -Body @{
        name = $groupName
        group_id = $groupId
        custom_key = $localKey
        quota = 0
    } -IdempotencyKey 'codex-router-local-key-v2')
}

$codexHome = if ($config.deploy.codexHome) { [string]$config.deploy.codexHome } elseif ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path ([Environment]::GetFolderPath('UserProfile')) '.codex' }
Write-Output '[6/7] Writing Codex configuration and the local access key...'
[IO.Directory]::CreateDirectory($codexHome) | Out-Null
$codexConfig = Join-Path $codexHome 'config.toml'
$catalogPath = Join-Path $routerRoot 'config\model-catalog.json'
$text = if (Test-Path -LiteralPath $codexConfig) { [IO.File]::ReadAllText($codexConfig) } else { '' }
$text = Set-TopLevelTomlValue $text 'model_provider' '"sub2api"'
$text = Set-TopLevelTomlValue $text 'model' ('"' + (Escape-TomlString ([string]$models[0].model)) + '"')
$text = Set-TopLevelTomlValue $text 'model_catalog_json' ('"' + (Escape-TomlString $catalogPath) + '"')
$text = [Text.RegularExpressions.Regex]::Replace($text, '(?ms)^\[model_providers\.sub2api\]\r?\n.*?(?=^\[|\z)', '')
$providerBlock = @"
[model_providers.sub2api]
name = "Codex-Router"
base_url = "http://127.0.0.1:18080/v1"
wire_api = "responses"
env_key = "CODEX_ROUTER_API_KEY"
requires_openai_auth = false
request_max_retries = 4
stream_max_retries = 5
stream_idle_timeout_ms = 720000
supports_websockets = false
"@
$text = $text.TrimEnd() + "`r`n`r`n" + $providerBlock.Trim() + "`r`n"
if (Test-Path -LiteralPath $codexConfig) {
    $backup = "$codexConfig.codex-router-$(Get-Date -Format 'yyyyMMdd-HHmmss-fff').bak"
    [IO.File]::Copy($codexConfig, $backup, $false)
}
[IO.File]::WriteAllText($codexConfig, $text, [Text.UTF8Encoding]::new($false))
[Environment]::SetEnvironmentVariable('CODEX_ROUTER_API_KEY', $localKey, 'User')

Write-Output "Configured $($models.Count) model channel(s)."
Write-Output "Codex configuration written to: $codexConfig"
Write-Output 'Local access key is stored in Windows Credential Manager and the current user environment.'
Write-Output '[7/7] Deployment complete.'
$localKey = $null
$session.Headers.Clear()
