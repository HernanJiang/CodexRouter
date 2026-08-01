$ports = @(
    @{Name='Adaptive Proxy'; Port=17897},
    @{Name='PostgreSQL'; Port=15432},
    @{Name='Redis'; Port=16379},
    @{Name='Sub2API'; Port=18080},
    @{Name='Codex Auth Adapter'; Port=18081}
)

foreach ($item in $ports) {
    $client = [Net.Sockets.TcpClient]::new()
    $running = $false
    try {
        $task = $client.ConnectAsync('127.0.0.1', $item.Port)
        $running = $task.Wait(500) -and $client.Connected
    } catch { } finally { $client.Dispose() }
    $listener = if ($running) { Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $item.Port -State Listen -ErrorAction SilentlyContinue } else { $null }
    [pscustomobject]@{
        Component = $item.Name
        Endpoint = "127.0.0.1:$($item.Port)"
        Running = $running
        ProcessId = if ($listener) { ($listener.OwningProcess -join ',') } else { '' }
    }
}

try {
    $response = Invoke-WebRequest -Uri 'http://127.0.0.1:18080/health' -TimeoutSec 3 -UseBasicParsing
    [pscustomobject]@{ Component='Health'; Endpoint='/health'; Running=($response.StatusCode -eq 200); ProcessId='' }
} catch {
    [pscustomobject]@{ Component='Health'; Endpoint='/health'; Running=$false; ProcessId='' }
}
