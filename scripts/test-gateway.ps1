$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'prepare-gateway.ps1')

$projectRoot = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $projectRoot 'src-tauri\resources\gateway\cli-proxy-api.exe'
$tempBase = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$tempRoot = Join-Path $tempBase ("hydra-gateway-test-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
$listener.Stop()

$apiKey = 'hydra-test-' + [guid]::NewGuid().ToString('N')
$authDir = (Join-Path $tempRoot 'auth').Replace('\', '/')
$config = Join-Path $tempRoot 'config.yaml'
$stdout = Join-Path $tempRoot 'gateway.stdout.log'
$stderr = Join-Path $tempRoot 'gateway.stderr.log'
$configText = @"
host: "127.0.0.1"
port: $port
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
plugins:
  enabled: false
"@
[IO.File]::WriteAllText($config, $configText, [Text.UTF8Encoding]::new($false))

$process = $null
try {
    $process = Start-Process -FilePath $exe `
        -ArgumentList @('-config', $config, '-local-model') `
        -WorkingDirectory $tempRoot `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdout `
        -RedirectStandardError $stderr `
        -PassThru

    $baseUrl = "http://127.0.0.1:$port"
    $ready = $false
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        if ($process.HasExited) {
            throw "Gateway exited during startup. See $stderr"
        }
        try {
            $response = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/v1/models" -Headers @{ 'x-api-key' = $apiKey } -TimeoutSec 1
            if ($response.StatusCode -eq 200) {
                $ready = $true
                break
            }
        }
        catch {
            Start-Sleep -Milliseconds 100
        }
    }
    if (-not $ready) {
        throw 'Gateway did not become ready within eight seconds.'
    }

    $unauthorized = $false
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/v1/models" -TimeoutSec 2 | Out-Null
    }
    catch {
        if ($_.Exception.Response.StatusCode.value__ -eq 401) {
            $unauthorized = $true
        }
    }
    if (-not $unauthorized) {
        throw 'Gateway accepted an unauthenticated model-list request.'
    }

    $payload = Invoke-RestMethod -Uri "$baseUrl/v1/models" -Headers @{ 'x-api-key' = $apiKey } -TimeoutSec 2
    if ($null -eq $payload.data) {
        throw 'Gateway model-list response did not contain a data array.'
    }

    Stop-Process -Id $process.Id -Force
    $process.WaitForExit(5000) | Out-Null
    foreach ($logPath in @($stdout, $stderr)) {
        if ((Test-Path -LiteralPath $logPath -PathType Leaf) -and
            [IO.File]::ReadAllText($logPath).Contains($apiKey)) {
            throw "Gateway diagnostic log exposed the fixture API key: $([IO.Path]::GetFileName($logPath))"
        }
    }

    Write-Output "Gateway smoke and log-redaction tests passed on loopback port $port."
}
finally {
    if ($null -ne $process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(5000) | Out-Null
    }
    if (Test-Path -LiteralPath $tempRoot) {
        $resolved = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $tempRoot).Path)
        if (-not $resolved.StartsWith($tempBase + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove temporary path outside TEMP: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
