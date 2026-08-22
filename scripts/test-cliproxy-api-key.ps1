[CmdletBinding()]
param()

# End-to-end validation of the `api-key-entries` provider block against the
# pinned CLIProxyAPI runtime. Proves (no paid key needed):
#   1. the api-key provider block is ACCEPTED by the pinned binary,
#   2. the api-key provider is REGISTERED (not silently "zero clients"),
#   3. a chat completion for that provider ROUTES to its configured base-url —
#      a loopback mock returns a distinctive 401, proving upstream forwarding.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot

& (Join-Path $PSScriptRoot 'prepare-gateway.ps1')

$exe = Join-Path $projectRoot 'src-tauri\resources\gateway\cli-proxy-api.exe'
$tempBase = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$tempRoot = Join-Path $tempBase ("hydra-gateway-api-key-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

function Get-FreePort {
    $l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $l.Start(); $p = ([Net.IPEndPoint]$l.LocalEndpoint).Port; $l.Stop(); return $p
}

# --- loopback mock upstream (in-process thread; Stop() unblocks accept) ------
$mockPort = Get-FreePort
$mockHitsFile = Join-Path $tempRoot 'mock-hits.txt'
$mockListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $mockPort)
$mockListener.Start()
$mockThread = [System.Threading.Thread]::new([System.Threading.ThreadStart]{
    try {
        while ($true) {
            $client = $mockListener.AcceptTcpClient()   # throws when listener stops
            try {
                $stream = $client.GetStream()
                $buffer = New-Object byte[] 65536
                $stream.Read($buffer, 0, $buffer.Length) | Out-Null
                Add-Content -Path $mockHitsFile -Value 'hit'
                $body = '{"error":{"message":"mock-upstream-401","type":"invalid_request_error"}}'
                $resp = "HTTP/1.1 401 Unauthorized`r`nContent-Type: application/json`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n$body"
                $respBytes = [Text.Encoding]::UTF8.GetBytes($resp)
                $stream.Write($respBytes, 0, $respBytes.Length)
                $stream.Flush()
            } finally { $client.Close() }
        }
    } catch {
        # Listener stopped -> exit.
    }
})
$mockThread.IsBackground = $true
$mockThread.Start()

$gatewayPort = Get-FreePort
$authDir = (Join-Path $tempRoot 'auth').Replace('\', '/')
New-Item -ItemType Directory -Path $authDir | Out-Null
$apiKey = 'hydra-api-key-' + [guid]::NewGuid().ToString('N')
$config = Join-Path $tempRoot 'config.yaml'
$stdout = Join-Path $tempRoot 'g.stdout.log'
$stderr = Join-Path $tempRoot 'g.stderr.log'

$configText = @"
host: "127.0.0.1"
port: $gatewayPort
remote-management:
  allow-remote: false
  secret-key: ""
  disable-control-panel: true
auth-dir: "$authDir"
api-keys:
  - "$apiKey"
debug: false
logging-to-file: false
request-log: false
request-retry: 0
max-retry-credentials: 1
plugins:
  enabled: false
# Basiliskos render_config api-key block (the corrected openai-compatibility shape).
openai-compatibility:
  - name: "deepseek"
    base-url: "http://127.0.0.1:$mockPort"
    api-key-entries:
      - api-key: "sk-test-key"
    models:
      - name: "deepseek-chat"
      - name: "deepseek-reasoner"
"@
[IO.File]::WriteAllText($config, $configText, [Text.UTF8Encoding]::new($false))

$process = $null
try {
    # -local-model uses the embedded models.json (so the api-key provider's
    # catalog is available offline) while the api-key block points it at the mock.
    $process = Start-Process -FilePath $exe `
        -ArgumentList @('-config', $config, '-local-model') `
        -WorkingDirectory $tempRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr `
        -PassThru

    $baseUrl = "http://127.0.0.1:$gatewayPort"
    $ready = $false
    $r = $null
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        if ($process.HasExited) { throw "CLIProxyAPI exited during startup. See $stderr" }
        try {
            $r = Invoke-RestMethod -Uri "$baseUrl/v1/models" -Headers @{ 'x-api-key' = $apiKey } -TimeoutSec 1
            if ($null -ne $r.data) { $ready = $true; break }
        } catch { Start-Sleep -Milliseconds 100 }
    }
    if (-not $ready) { throw 'CLIProxyAPI did not become ready.' }
    $modelCount = @($r.data).Count
    Write-Output "CLIProxyAPI accepted the api-key config; /v1/models returned $modelCount model(s)."

    $body = @{ model = 'deepseek-chat'; messages = @(@{ role = 'user'; content = 'hi' }) } | ConvertTo-Json -Depth 5
    $status = $null; $respBody = $null
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -Method Post -Uri "$baseUrl/v1/chat/completions" `
            -Headers @{ 'x-api-key' = $apiKey; 'Content-Type' = 'application/json' } `
            -Body $body -TimeoutSec 8
        $status = $resp.StatusCode; $respBody = $resp.Content
    } catch {
        $status = $_.Exception.Response.StatusCode.value__
        $respBody = $_.ErrorDetails.Message
        if ($null -eq $status) { throw "Chat completion failed before an HTTP response: $($_.Exception.Message)" }
    }

    if ($status -ne 401) {
        throw "Expected 401 from the mock upstream, got $status. Body: $respBody"
    }
    if (-not $respBody.Contains('mock-upstream-401')) {
        throw "Mock upstream body not relayed; got: $respBody"
    }
    Start-Sleep -Milliseconds 300
    $hits = if (Test-Path -LiteralPath $mockHitsFile) { @(Get-Content -LiteralPath $mockHitsFile).Count } else { 0 }
    if ($hits -lt 1) { throw 'The api-key provider request never reached the configured base-url (routing failed).' }
    Write-Output "api-key-entries block accepted and routed (mock hit=$hits, status=$status)."
    Write-Output "CLIProxyAPI api-key contract test passed."
}
finally {
    try { $mockListener.Stop() } catch {}
    try { $mockThread.Join(2000) | Out-Null } catch {}
    if ($null -ne $process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $tempRoot) {
        $resolved = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $tempRoot).Path)
        if (-not $resolved.StartsWith($tempBase + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'Refusing to remove temp outside TEMP' }
        Remove-Item -LiteralPath $resolved -Recurse -Force -ErrorAction SilentlyContinue
    }
}
