Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$routerRoot = Split-Path -Parent $PSScriptRoot
$modulePath = Join-Path $PSScriptRoot 'CredentialStore.psm1'
Import-Module $modulePath -Force

function Assert-Test {
    param([Parameter(Mandatory)][bool]$Condition, [Parameter(Mandatory)][string]$Message)
    if (-not $Condition) { throw "Lifecycle test failed: $Message" }
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('codex-router-lifecycle-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null
$listener = $null
$client = $null
$acceptedClient = $null
$fakeSub2Api = $null
$fakeClient = $null
try {
    $lock = Enter-RouterLifecycleLock `
        -RouterRoot $testRoot `
        -TimeoutMilliseconds 1000 `
        -Operation 'test lock holder'
    try {
        $previousMarker = [Environment]::GetEnvironmentVariable(
            'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
            'Process')
        [Environment]::SetEnvironmentVariable(
            'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
            [string]$PID,
            'Process')
        try {
        $nestedLock = Enter-RouterLifecycleLock `
            -RouterRoot $testRoot `
            -TimeoutMilliseconds 300 `
            -Operation 'test nested operation'
        Assert-Test `
            -Condition ([bool]$nestedLock.Inherited) `
            -Message 'a nested lifecycle operation self-deadlocked instead of inheriting the lock'
        Exit-RouterLifecycleLock -Lock $nestedLock

        $contenderPath = Join-Path $testRoot 'contender.ps1'
        $resultPath = Join-Path $testRoot 'contender-result.txt'
        $contenderSource = @'
param(
    [Parameter(Mandatory)][string]$ModulePath,
    [Parameter(Mandatory)][string]$RouterRoot,
    [Parameter(Mandatory)][string]$ResultPath
)
$ErrorActionPreference = 'Stop'
Import-Module $ModulePath -Force
try {
    $lock = Enter-RouterLifecycleLock -RouterRoot $RouterRoot -TimeoutMilliseconds 300 -Operation 'test contender'
    try { [IO.File]::WriteAllText($ResultPath, 'unexpectedly-acquired') }
    finally { Exit-RouterLifecycleLock -Lock $lock }
} catch {
    [IO.File]::WriteAllText($ResultPath, $_.Exception.Message)
}
'@
        [IO.File]::WriteAllText($contenderPath, $contenderSource, [Text.UTF8Encoding]::new($false))
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
        $startInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$contenderPath`" -ModulePath `"$modulePath`" -RouterRoot `"$testRoot`" -ResultPath `"$resultPath`""
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $contender = [Diagnostics.Process]::new()
        $contender.StartInfo = $startInfo
        try {
            Assert-Test -Condition $contender.Start() -Message 'could not start the lock contender'
            Assert-Test -Condition $contender.WaitForExit(5000) -Message 'lock contender did not finish'
        } finally {
            $contender.Dispose()
        }
        $contentionResult = [IO.File]::ReadAllText($resultPath)
        Assert-Test `
            -Condition $contentionResult.Contains('ROUTER_LIFECYCLE_BUSY') `
            -Message 'a second process bypassed the lifecycle lock'
        } finally {
            [Environment]::SetEnvironmentVariable(
                'CODEX_ROUTER_LIFECYCLE_LOCK_HELD',
                $previousMarker,
                'Process')
        }
    } finally {
        Exit-RouterLifecycleLock -Lock $lock
    }

    $releasedLock = Enter-RouterLifecycleLock `
        -RouterRoot $testRoot `
        -TimeoutMilliseconds 500 `
        -Operation 'test lock reacquire'
    Exit-RouterLifecycleLock -Lock $releasedLock

    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
    $client = [Net.Sockets.TcpClient]::new()
    $client.Connect('127.0.0.1', $port)
    $acceptedClient = $listener.AcceptTcpClient()

    $activeConnections = 0
    for ($attempt = 0; $attempt -lt 20 -and $activeConnections -eq 0; $attempt++) {
        try {
            $activeConnections = Get-RouterEstablishedConnectionCount -ProcessId $PID -Port $port
        } catch {
            $activeConnections = 0
        }
        if ($activeConnections -eq 0) { Start-Sleep -Milliseconds 50 }
    }
    Assert-Test -Condition ($activeConnections -gt 0) -Message 'the test TCP connection did not reach Established state'

    $pidBefore = (Get-Process -Id $PID -ErrorAction Stop).Id
    foreach ($operation in @('Stop Router', 'Proxy settings change')) {
        $deferredMessage = ''
        try {
            Assert-RouterServiceInterruptionAllowed `
                -ProcessId $PID `
                -Port $port `
                -Operation $operation
        } catch {
            $deferredMessage = $_.Exception.Message
        }
        Assert-Test `
            -Condition $deferredMessage.Contains('ROUTER_LIFECYCLE_DEFERRED') `
            -Message "$operation did not return the explicit deferred result"
        Assert-Test `
            -Condition ((Get-Process -Id $PID -ErrorAction Stop).Id -eq $pidBefore) `
            -Message "$operation changed the protected process PID"
    }

    $fakeRoot = Join-Path $testRoot 'fake-router'
    foreach ($relative in @('app', 'data\pids', 'scripts')) {
        [IO.Directory]::CreateDirectory((Join-Path $fakeRoot $relative)) | Out-Null
    }
    foreach ($scriptName in @('CredentialStore.psm1', 'RouterAdmin.psm1', 'UserData.psm1', 'Stop-Router.ps1')) {
        Copy-Item `
            -LiteralPath (Join-Path $PSScriptRoot $scriptName) `
            -Destination (Join-Path (Join-Path $fakeRoot 'scripts') $scriptName)
    }
    $serverSource = @'
using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;

public static class LifecycleTestServer
{
    private static readonly List<TcpClient> Clients = new List<TcpClient>();

    public static void Main(string[] args)
    {
        var listener = new TcpListener(IPAddress.Loopback, Int32.Parse(args[0]));
        listener.Start();
        while (true)
        {
            var client = listener.AcceptTcpClient();
            lock (Clients) { Clients.Add(client); }
        }
    }
}
'@
    $fakeExe = Join-Path $fakeRoot 'app\sub2api.exe'
    Add-Type `
        -TypeDefinition $serverSource `
        -Language CSharp `
        -OutputAssembly $fakeExe `
        -OutputType ConsoleApplication
    $portProbe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $portProbe.Start()
    $fakePort = ([Net.IPEndPoint]$portProbe.LocalEndpoint).Port
    $portProbe.Stop()
    $fakeConfig = @{
        deploy = @{ sub2apiHost = "http://127.0.0.1:$fakePort" }
    } | ConvertTo-Json -Depth 3
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'codex-router-config.json'),
        $fakeConfig,
        [Text.UTF8Encoding]::new($false))
    $fakeSub2Api = Start-Process `
        -FilePath $fakeExe `
        -ArgumentList ([string]$fakePort) `
        -WorkingDirectory (Join-Path $fakeRoot 'app') `
        -WindowStyle Hidden `
        -PassThru
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'data\pids\sub2api.pid'),
        [string]$fakeSub2Api.Id,
        [Text.Encoding]::ASCII)
    $fakeListening = $false
    for ($attempt = 0; $attempt -lt 40 -and -not $fakeListening; $attempt++) {
        $fakeListening = $null -ne (Get-NetTCPConnection `
            -LocalPort $fakePort `
            -OwningProcess $fakeSub2Api.Id `
            -State Listen `
            -ErrorAction SilentlyContinue)
        if (-not $fakeListening) { Start-Sleep -Milliseconds 50 }
    }
    Assert-Test -Condition $fakeListening -Message 'the isolated fake Sub2API did not start listening'
    $fakeClient = [Net.Sockets.TcpClient]::new()
    $fakeClient.Connect('127.0.0.1', $fakePort)
    $fakeActive = 0
    for ($attempt = 0; $attempt -lt 20 -and $fakeActive -eq 0; $attempt++) {
        try {
            $fakeActive = Get-RouterEstablishedConnectionCount `
                -ProcessId $fakeSub2Api.Id `
                -Port $fakePort
        } catch {
            $fakeActive = 0
        }
        if ($fakeActive -eq 0) { Start-Sleep -Milliseconds 50 }
    }
    Assert-Test -Condition ($fakeActive -gt 0) -Message 'the isolated fake Sub2API connection was not Established'
    $fakePidBefore = $fakeSub2Api.Id
    $stopInfo = [Diagnostics.ProcessStartInfo]::new()
    $stopInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $stopScript = Join-Path $fakeRoot 'scripts\Stop-Router.ps1'
    $stopInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$stopScript`""
    $stopInfo.WorkingDirectory = $fakeRoot
    $stopInfo.UseShellExecute = $false
    $stopInfo.CreateNoWindow = $true
    $stopInfo.RedirectStandardOutput = $true
    $stopInfo.RedirectStandardError = $true
    $stopAttempt = [Diagnostics.Process]::new()
    $stopAttempt.StartInfo = $stopInfo
    try {
        Assert-Test -Condition $stopAttempt.Start() -Message 'could not run the isolated Stop-Router script'
        Assert-Test -Condition $stopAttempt.WaitForExit(10000) -Message 'isolated Stop-Router did not finish'
        $stopOutput = $stopAttempt.StandardOutput.ReadToEnd() + $stopAttempt.StandardError.ReadToEnd()
        Assert-Test -Condition ($stopAttempt.ExitCode -ne 0) -Message 'active Stop-Router unexpectedly succeeded'
        Assert-Test `
            -Condition $stopOutput.Contains('ROUTER_LIFECYCLE_DEFERRED') `
            -Message 'active Stop-Router did not report the explicit deferred result'
    } finally {
        $stopAttempt.Dispose()
    }
    Assert-Test `
        -Condition ((Get-Process -Id $fakePidBefore -ErrorAction Stop).Id -eq $fakePidBefore) `
        -Message 'the real Stop-Router entry point changed the active Sub2API PID'

    Copy-Item `
        -LiteralPath (Join-Path $PSScriptRoot 'Start-Router.ps1') `
        -Destination (Join-Path $fakeRoot 'scripts\Start-Router.ps1')
    $escapedModulePath = $modulePath.Replace("'", "''")
    $credentialStub = @"
Import-Module '$escapedModulePath' -Force -Prefix Real
function Get-RouterCredential {
    param([Parameter(Mandatory)][string]`$Name, [switch]`$AllowMissing)
    return 'local-test-value'
}
function Write-RouterFileAtomic {
    param([Parameter(Mandatory)][string]`$Path, [Parameter(Mandatory)][byte[]]`$Bytes)
    Write-RealRouterFileAtomic -Path `$Path -Bytes `$Bytes
}
function Enter-RouterLifecycleLock {
    param(
        [Parameter(Mandatory)][string]`$RouterRoot,
        [int]`$TimeoutMilliseconds = 10000,
        [string]`$Operation = 'test operation'
    )
    Enter-RealRouterLifecycleLock @PSBoundParameters
}
function Exit-RouterLifecycleLock {
    param([AllowNull()]`$Lock)
    Exit-RealRouterLifecycleLock -Lock `$Lock
}
function Enter-RouterConfigLock {
    param(
        [Parameter(Mandatory)][string]`$RouterRoot,
        [int]`$TimeoutMilliseconds = 10000
    )
    Enter-RealRouterConfigLock @PSBoundParameters
}
function Exit-RouterConfigLock {
    param([AllowNull()]`$Lock)
    Exit-RealRouterConfigLock -Lock `$Lock
}
function Assert-RouterServiceInterruptionAllowed {
    param(
        [Parameter(Mandatory)][int]`$ProcessId,
        [Parameter(Mandatory)][int]`$Port,
        [Parameter(Mandatory)][string]`$Operation
    )
    Assert-RealRouterServiceInterruptionAllowed @PSBoundParameters
}
Export-ModuleMember -Function Get-RouterCredential, Write-RouterFileAtomic, Enter-RouterLifecycleLock, Exit-RouterLifecycleLock, Enter-RouterConfigLock, Exit-RouterConfigLock, Assert-RouterServiceInterruptionAllowed
"@
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'scripts\CredentialStore.psm1'),
        $credentialStub,
        [Text.UTF8Encoding]::new($false))
    $proxyStub = @'
function Resolve-RouterProxySettings {
    param($ProxyConfig, $ProxyPassword)
    return [pscustomobject]@{
        ProxyUrl = $null
        NoProxy = 'localhost,127.0.0.1'
    }
}
Export-ModuleMember -Function Resolve-RouterProxySettings
'@
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'scripts\ProxyDiscovery.psm1'),
        $proxyStub,
        [Text.UTF8Encoding]::new($false))
    $adminStub = @"
function Get-RouterBaseUri { return 'http://127.0.0.1:$fakePort' }
Export-ModuleMember -Function Get-RouterBaseUri
"@
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'scripts\RouterAdmin.psm1'),
        $adminStub,
        [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'data\pids\sub2api-network.hmac'),
        'stale-network-fingerprint',
        [Text.Encoding]::ASCII)
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $startScript = Join-Path $fakeRoot 'scripts\Start-Router.ps1'
    $startInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$startScript`""
    $startInfo.WorkingDirectory = $fakeRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startAttempt = [Diagnostics.Process]::new()
    $startAttempt.StartInfo = $startInfo
    try {
        Assert-Test -Condition $startAttempt.Start() -Message 'could not run the isolated Start-Router script'
        Assert-Test -Condition $startAttempt.WaitForExit(10000) -Message 'isolated Start-Router did not finish'
        $startOutput = $startAttempt.StandardOutput.ReadToEnd() + $startAttempt.StandardError.ReadToEnd()
        Assert-Test -Condition ($startAttempt.ExitCode -ne 0) -Message 'active fingerprint change unexpectedly succeeded'
        Assert-Test `
            -Condition $startOutput.Contains('ROUTER_LIFECYCLE_DEFERRED') `
            -Message "active fingerprint change did not report the explicit deferred result: $startOutput"
    } finally {
        $startAttempt.Dispose()
    }
    Assert-Test `
        -Condition ((Get-Process -Id $fakePidBefore -ErrorAction Stop).Id -eq $fakePidBefore) `
        -Message 'the real Start-Router fingerprint path changed the active Sub2API PID'

    Copy-Item `
        -LiteralPath (Join-Path $PSScriptRoot 'Apply-Router.ps1') `
        -Destination (Join-Path $fakeRoot 'scripts\Apply-Router.ps1')
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'scripts\CodexIntegration.psm1'),
        "Export-ModuleMember",
        [Text.UTF8Encoding]::new($false))
    $applyInfo = [Diagnostics.ProcessStartInfo]::new()
    $applyInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $applyScript = Join-Path $fakeRoot 'scripts\Apply-Router.ps1'
    $applyInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$applyScript`""
    $applyInfo.WorkingDirectory = $fakeRoot
    $applyInfo.UseShellExecute = $false
    $applyInfo.CreateNoWindow = $true
    $applyInfo.RedirectStandardOutput = $true
    $applyInfo.RedirectStandardError = $true
    $applyAttempt = [Diagnostics.Process]::new()
    $applyAttempt.StartInfo = $applyInfo
    try {
        Assert-Test -Condition $applyAttempt.Start() -Message 'could not run the isolated Apply-Router script'
        Assert-Test -Condition $applyAttempt.WaitForExit(10000) -Message 'isolated Apply-Router did not finish'
        $applyOutput = $applyAttempt.StandardOutput.ReadToEnd() + $applyAttempt.StandardError.ReadToEnd()
        Assert-Test -Condition ($applyAttempt.ExitCode -ne 0) -Message 'active Apply-Router unexpectedly succeeded'
        Assert-Test `
            -Condition $applyOutput.Contains('ROUTER_LIFECYCLE_DEFERRED') `
            -Message "active Apply-Router did not report the explicit deferred result: $applyOutput"
    } finally {
        $applyAttempt.Dispose()
    }
    Assert-Test `
        -Condition ((Get-Process -Id $fakePidBefore -ErrorAction Stop).Id -eq $fakePidBefore) `
        -Message 'the real Apply-Router entry point changed the active Sub2API PID'

    $startSource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Start-Router.ps1'))
    $stopSource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Stop-Router.ps1'))
    $applySource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Apply-Router.ps1'))
    $ensureAdminSource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Ensure-Sub2ApiAdmin.ps1'))
    $monitorSource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Ensure-RouterHealthy.ps1'))
    $autostartSource = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'Register-Autostart.ps1'))
    $mainSource = [IO.File]::ReadAllText((Join-Path $routerRoot 'codex-router-gui-rust\src\main.rs'))
    $uiSource = [IO.File]::ReadAllText((Join-Path $routerRoot 'codex-router-gui-rust\src\ui.rs'))
    $logicSource = [IO.File]::ReadAllText((Join-Path $routerRoot 'codex-router-gui-rust\src\logic.rs'))
    $stopTokens = $null
    $stopParseErrors = $null
    $stopAst = [Management.Automation.Language.Parser]::ParseInput(
        $stopSource,
        [ref]$stopTokens,
        [ref]$stopParseErrors)
    Assert-Test -Condition ($stopParseErrors.Count -eq 0) -Message 'Stop-Router.ps1 has syntax errors'
    $oauthMatcherDefinitions = @($stopAst.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Test-ManagedOAuthProcess'
    }, $true))
    Assert-Test -Condition ($oauthMatcherDefinitions.Count -eq 1) -Message 'managed OAuth process matcher is missing'
    . ([ScriptBlock]::Create($oauthMatcherDefinitions[0].Extent.Text))
    $oauthScriptUnderTest = Join-Path $testRoot 'OAuth Owner\scripts\Start-ChatGPTOAuth.ps1'
    $expectedPowerShell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $matchingOAuthProcess = [pscustomobject]@{
        ExecutablePath = $expectedPowerShell
        CommandLine = "powershell.exe -NoProfile -File `"$oauthScriptUnderTest`" -TimeoutSeconds 600"
    }
    Assert-Test `
        -Condition (Test-ManagedOAuthProcess -Process $matchingOAuthProcess -ScriptPath $oauthScriptUnderTest) `
        -Message 'managed OAuth matcher rejected the exact portable script path'
    $foreignOAuthProcess = [pscustomobject]@{
        ExecutablePath = $expectedPowerShell
        CommandLine = 'powershell.exe -NoProfile -File "D:\Foreign Router\scripts\Start-ChatGPTOAuth.ps1"'
    }
    Assert-Test `
        -Condition (-not (Test-ManagedOAuthProcess -Process $foreignOAuthProcess -ScriptPath $oauthScriptUnderTest)) `
        -Message 'managed OAuth matcher accepted a foreign portable script path'
    $startPreflight = $startSource.IndexOf("`$pidFile = Join-Path `$dataRoot 'pids\sub2api.pid'")
    $dependencyStart = $startSource.IndexOf('$pgCtl = "$routerRoot\postgres\pgsql\bin\pg_ctl.exe"')
    Assert-Test `
        -Condition ($startPreflight -ge 0 -and $dependencyStart -gt $startPreflight) `
        -Message 'Start does not run the Sub2API safety preflight before dependency changes'
    $missingListenerGuard = $startSource.IndexOf(
        'if (-not $RepairUnhealthy)',
        $startSource.IndexOf('if ($null -eq $verifiedSub2Api)'))
    $missingListenerStop = $startSource.IndexOf('Stop-Process -Id $savedPid')
    Assert-Test `
        -Condition ($missingListenerGuard -ge 0 -and
            $missingListenerStop -gt $missingListenerGuard) `
        -Message 'missing-listener recovery is not gated by the explicit repair switch'
    Assert-Test `
        -Condition ($startSource.Contains('[switch]$RepairUnhealthy') -and
            $startSource.Contains('Test-PostgresReadyStable') -and
            $startSource.Contains('Stop-VerifiedPostgresTree') -and
            $startSource.Contains('$process.WaitForExit($TimeoutSeconds * 1000)')) `
        -Message 'Start is missing bounded, verified PostgreSQL recovery'
    Assert-Test `
        -Condition ($monitorSource.Contains('/v1/models') -and
            $monitorSource.Contains('-RepairUnhealthy')) `
        -Message 'health monitor does not probe the authenticated data path before recovery'
    Assert-Test `
        -Condition ($autostartSource.Contains("`$shortcut.Arguments = '--background'") -and
            $autostartSource.Contains('$shortcut.WindowStyle = 7') -and
            $autostartSource.Contains('DeleteTask($taskName, 0)') -and
            -not $autostartSource.Contains('Start-Process') -and
            $mainSource.Contains('CredReadW') -and
            $mainSource.Contains('GET /v1/models HTTP/1.1') -and
            $mainSource.Contains('CREATE_NO_WINDOW') -and
            $mainSource.Contains('.with_visible(!start_in_background)') -and
            $mainSource.Contains('enforce_background_start_hidden') -and
            $mainSource.Contains('hide_current_process_windows') -and
            $mainSource.Contains('legacy_autostart_shortcut_exists') -and
            $mainSource.Contains('health_probe_failures >= 3') -and
            $mainSource.Contains('tray_lightweight_mode') -and
            -not $mainSource.Contains('fn run_watchdog')) `
        -Message 'GUI-integrated lightweight forwarding protection is incomplete'
    Assert-Test `
        -Condition ($uiSource.Contains('Minimize on X') -and
            $uiSource.Contains('Silent startup') -and
            -not $uiSource.Contains('Start Router after Windows sign-in')) `
        -Message 'dashboard close/autostart controls are not the required two-row switch layout'
    $stopGuard = $stopSource.IndexOf('Assert-RouterServiceInterruptionAllowed')
    $stopTermination = $stopSource.IndexOf('Stop-Process -Id $sub2apiPid')
    Assert-Test `
        -Condition ($stopGuard -ge 0 -and $stopTermination -gt $stopGuard) `
        -Message 'Stop terminates Sub2API before checking active connections'
    Assert-Test `
        -Condition ($stopSource.Contains('[switch]$Force') -and
            $stopSource.Contains('[switch]$AdoptActivePortableOwner') -and
            $stopSource.Contains('$null -ne $sub2apiProcess -and -not $Force') -and
            $stopSource.Contains('$effectiveDependencyTimeoutSeconds') -and
            $stopSource.Contains("shutdown `$shutdownMode") -and
            $stopSource.Contains("if (`$Force) { 'immediate' } else { 'smart' }") -and
            $stopSource.Contains('Test-ManagedOAuthProcess') -and
            $stopSource.Contains('Stop-ManagedOAuthProcesses') -and
            $stopSource.Contains('ReadToEndAsync()') -and
            $stopSource.Contains('Stop-ProcessTree -ProcessId $process.Id') -and
            $stopSource.Contains('Stop-RemainingManagedProcesses') -and
            $stopSource.Contains('Assert-ManagedServiceStopped') -and
            $mainSource.Contains('ExitShutdownFinished') -and
            $mainSource.Contains('EXIT_CONFIG_LOCK_TIMEOUT') -and
            $mainSource.Contains('EXIT_SHUTDOWN_TIMEOUT') -and
            $mainSource.Contains('oauth_recovery_cancel') -and
            $mainSource.Contains('terminate_child_process_tree') -and
            $mainSource.Contains('codex_router_mode_configured') -and
            $mainSource.Contains('acquire_config_apply_lock') -and
            $mainSource.Contains('repaint.request_repaint()') -and
            $mainSource.Contains('.arg("-Force")') -and
            $mainSource.Contains('.arg("-AdoptActivePortableOwner")')) `
        -Message 'full GUI exit is not wired to a verified forced portable-backend shutdown'
    Assert-Test `
        -Condition (-not $stopSource.Contains('health-monitor.paused') -and
            -not $startSource.Contains('health-monitor.paused')) `
        -Message 'legacy watchdog pause markers remain in the Router lifecycle'
    $deploymentCancel = $logicSource.IndexOf('if cancel.load(Ordering::Acquire)')
    $deploymentCancelTreeKill = if ($deploymentCancel -ge 0) {
        $logicSource.IndexOf('terminate_deployment_process_tree(&mut child)', $deploymentCancel)
    } else { -1 }
    $deploymentTimeout = $logicSource.IndexOf('if started.elapsed() >= timeout', $deploymentCancel)
    $deploymentTimeoutShellKill = if ($deploymentTimeout -ge 0) {
        $logicSource.IndexOf('let _ = child.kill();', $deploymentTimeout)
    } else { -1 }
    Assert-Test `
        -Condition ($logicSource.Contains('taskkill.exe') -and
            $deploymentCancel -ge 0 -and
            $deploymentCancelTreeKill -gt $deploymentCancel -and
            $deploymentCancelTreeKill -lt $deploymentTimeout -and
            $deploymentTimeoutShellKill -gt $deploymentTimeout) `
        -Message 'deployment cancellation and timeout do not preserve their distinct process-termination scopes'
    $applyGuard = $applySource.IndexOf('Assert-RouterServiceInterruptionAllowed')
    $applyInitialization = $applySource.IndexOf("Write-Output '[1/7] Initializing")
    Assert-Test `
        -Condition ($applyGuard -ge 0 -and $applyInitialization -gt $applyGuard) `
        -Message 'Apply mutates Router state before checking active connections'
    $complianceCheck = $applySource.IndexOf('if ($compliance.required)')
    $ensureAdminCall = $applySource.IndexOf("Ensure-Sub2ApiAdmin.ps1")
    $refreshedAdminSession = if ($ensureAdminCall -ge 0) {
        $applySource.IndexOf('$session = New-RouterAdminSession', $ensureAdminCall + 1)
    } else {
        -1
    }
    $modelProvisioning = $applySource.IndexOf('$modelNames =')
    Assert-Test `
        -Condition ($complianceCheck -ge 0 -and
            $ensureAdminCall -gt $complianceCheck -and
            $refreshedAdminSession -gt $ensureAdminCall -and
            $modelProvisioning -gt $refreshedAdminSession -and
            -not $startSource.Contains('Ensure-Sub2ApiAdmin.ps1') -and
            $ensureAdminSource.Contains("CredentialStore.psm1")) `
        -Message 'fresh deployment does not accept compliance and refresh the administrator session in the required order'

    $fakeDependencyManifest = @{
        schemaVersion = 1
        components = @(
            @{
                name = 'Sub2API'
                executable = 'app/sub2api.exe'
                executableSha256 = (Get-FileHash -LiteralPath $fakeExe -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        )
    } | ConvertTo-Json -Depth 5
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'dependency-manifest.json'),
        $fakeDependencyManifest,
        [Text.UTF8Encoding]::new($false))
    $fakeReleaseManifest = ConvertTo-Json -Depth 4 -InputObject @(
        @{
            path = 'scripts/Stop-Router.ps1'
            sha256 = (Get-FileHash -LiteralPath $stopScript -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    )
    [IO.File]::WriteAllText(
        (Join-Path $fakeRoot 'release-manifest.json'),
        $fakeReleaseManifest,
        [Text.UTF8Encoding]::new($false))

    $callerRoot = Join-Path $testRoot 'newer-router'
    [IO.Directory]::CreateDirectory((Join-Path $callerRoot 'scripts')) | Out-Null
    foreach ($scriptName in @('CredentialStore.psm1', 'RouterAdmin.psm1', 'UserData.psm1', 'Stop-Router.ps1')) {
        Copy-Item `
            -LiteralPath (Join-Path $PSScriptRoot $scriptName) `
            -Destination (Join-Path (Join-Path $callerRoot 'scripts') $scriptName)
    }
    [IO.File]::WriteAllText(
        (Join-Path $callerRoot 'codex-router-config.json'),
        $fakeConfig,
        [Text.UTF8Encoding]::new($false))

    $forceStopInfo = [Diagnostics.ProcessStartInfo]::new()
    $forceStopInfo.FileName = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $callerStopScript = Join-Path $callerRoot 'scripts\Stop-Router.ps1'
    $forceStopInfo.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$callerStopScript`" -Force -AdoptActivePortableOwner -RedisPort 1 -PostgresPort 2"
    $forceStopInfo.WorkingDirectory = $callerRoot
    $forceStopInfo.UseShellExecute = $false
    $forceStopInfo.CreateNoWindow = $true
    $forceStopInfo.RedirectStandardOutput = $true
    $forceStopInfo.RedirectStandardError = $true
    $forceStopAttempt = [Diagnostics.Process]::new()
    $forceStopAttempt.StartInfo = $forceStopInfo
    try {
        Assert-Test -Condition $forceStopAttempt.Start() -Message 'could not run the isolated forced Stop-Router script'
        Assert-Test -Condition $forceStopAttempt.WaitForExit(10000) -Message 'isolated forced Stop-Router did not finish'
        $forceStopOutput = $forceStopAttempt.StandardOutput.ReadToEnd() + $forceStopAttempt.StandardError.ReadToEnd()
        Assert-Test `
            -Condition ($forceStopAttempt.ExitCode -eq 0) `
            -Message "cross-version forced full-exit shutdown failed: $forceStopOutput"
        Assert-Test `
            -Condition $forceStopOutput.Contains('active verified portable release') `
            -Message "cross-version shutdown did not report verified portable adoption: $forceStopOutput"
        $fakeSub2Api.WaitForExit(5000)
        Assert-Test -Condition $fakeSub2Api.HasExited -Message 'cross-version full-exit shutdown left Sub2API running'
    } finally {
        $forceStopAttempt.Dispose()
    }

    Write-Output 'PASS lifecycle lock serializes concurrent processes.'
    Write-Output 'PASS active Stop entry point is deferred and preserves the process PID.'
    Write-Output 'PASS active proxy fingerprint restart is deferred and preserves the process PID.'
    Write-Output 'PASS active Apply entry point is deferred before initialization and preserves the process PID.'
    Write-Output 'PASS deliberate full GUI exit adopts and stops its verified active portable backend.'
    Write-Output 'PASS transient health/listener handling and deployment timeout do not kill the service tree.'
    Write-Output 'PASS GUI-integrated deep health protection invokes only verified unhealthy-service recovery.'
    Write-Output 'PASS fresh deployment accepts compliance before administrator reconciliation and session refresh.'
} finally {
    if ($null -ne $fakeClient) { $fakeClient.Dispose() }
    if ($null -ne $fakeSub2Api) {
        if (-not $fakeSub2Api.HasExited) {
            $fakeSub2Api.Kill()
            $fakeSub2Api.WaitForExit()
        }
        $fakeSub2Api.Dispose()
    }
    if ($null -ne $acceptedClient) { $acceptedClient.Dispose() }
    if ($null -ne $client) { $client.Dispose() }
    if ($null -ne $listener) { $listener.Stop() }
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
