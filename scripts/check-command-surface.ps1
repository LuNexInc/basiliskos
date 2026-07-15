[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$frontendPath = Join-Path $projectRoot 'src\App.tsx'
$backendPath = Join-Path $projectRoot 'src-tauri\src\lib.rs'
$cargoPath = Join-Path $projectRoot 'src-tauri\Cargo.toml'
$packagePath = Join-Path $projectRoot 'package.json'
$capabilityPath = Join-Path $projectRoot 'src-tauri\capabilities\default.json'
$frontend = Get-Content -LiteralPath $frontendPath -Raw
$backend = Get-Content -LiteralPath $backendPath -Raw
$gatewayBackend = Get-Content -LiteralPath (Join-Path $projectRoot 'src-tauri\src\gateway.rs') -Raw

$expected = @(
    'cancel_provider_login'
    'gateway_snapshot'
    'get_gateway_account_usage'
    'launch_hydra_claude'
    'launch_provider_login'
    'open_diagnostics_folder'
    'remove_gateway_account'
    'rename_gateway_account'
    'select_gateway_account'
    'set_gateway_route'
    'start_gateway'
    'stop_gateway'
    'stop_hydra_claude'
) | Sort-Object -Unique

$handler = [regex]::Match(
    $backend,
    '(?s)tauri::generate_handler!\[(?<commands>.*?)\]'
)
if (-not $handler.Success) {
    throw 'Could not locate the Tauri command registration block.'
}
$registered = @(
    [regex]::Matches($handler.Groups['commands'].Value, 'gateway::(?<name>[a-z_]+)') |
        ForEach-Object { $_.Groups['name'].Value }
) | Sort-Object -Unique

$difference = @(Compare-Object -ReferenceObject $expected -DifferenceObject $registered)
if ($difference.Count -gt 0) {
    $details = $difference | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" }
    throw "Registered command surface does not match the Basiliskos allowlist: $($details -join ', ')"
}

foreach ($command in $expected) {
    if (-not $frontend.Contains('"' + $command + '"')) {
        throw "Registered command is not referenced by the frontend: $command"
    }
}

$forbidden = @(
    'profiles.json'
    '.grok'
    'grok.exe'
    'taskkill'
    'import_profile_file'
    'launch_grok'
    'refresh_profile'
)
foreach ($marker in $forbidden) {
    if ($backend.IndexOf($marker, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "Removed legacy backend surface reappeared in lib.rs: $marker"
    }
}

$singleInstanceIndex = $backend.IndexOf('.plugin(tauri_plugin_single_instance::init', [StringComparison]::Ordinal)
$openerIndex = $backend.IndexOf('.plugin(tauri_plugin_opener::init', [StringComparison]::Ordinal)
if ($singleInstanceIndex -lt 0 -or $openerIndex -lt 0 -or $singleInstanceIndex -gt $openerIndex) {
    throw 'The single-instance plugin must be registered before every other Tauri plugin.'
}

$dependencySurface = (
    (Get-Content -LiteralPath $cargoPath -Raw) +
    (Get-Content -LiteralPath $packagePath -Raw) +
    (Get-Content -LiteralPath $capabilityPath -Raw)
)
if ($dependencySurface.IndexOf('plugin-dialog', [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
    $dependencySurface.IndexOf('dialog:allow-open', [StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw 'The unused dialog plugin or capability has returned.'
}

$tauriConfig = Get-Content -LiteralPath (Join-Path $projectRoot 'src-tauri\tauri.conf.json') -Raw
if ($tauriConfig.Contains('"csp": null') -or $dependencySurface.Contains('opener:default')) {
    throw 'The restrictive CSP or trusted-origin opener scope regressed.'
}
if (-not $gatewayBackend.Contains('request-retry: 0') -or -not $gatewayBackend.Contains('bootstrap-retries: 0')) {
    throw 'The no-replay backend policy regressed.'
}

Write-Output "Command-surface gate passed with $($registered.Count) frontend-backed commands."
