param([switch]$NoSign)

$ErrorActionPreference = 'Stop'
$required = $env:BASILISKOS_REQUIRE_SIGNING -eq '1'
$signTool = $env:TAURI_WINDOWS_SIGNTOOL_PATH

if ([string]::IsNullOrWhiteSpace($signTool) -or -not (Test-Path -LiteralPath $signTool -PathType Leaf)) {
    $signTool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Filter signtool.exe -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

if ($signTool) {
    $env:TAURI_WINDOWS_SIGNTOOL_PATH = $signTool
}
elseif ($required) {
    throw 'Release signing is required but the x64 Windows SDK signtool.exe is unavailable.'
}
else {
    $NoSign = $true
    Write-Output 'signtool.exe is unavailable; using Tauri unsigned-build mode.'
}

$arguments = @('exec', 'tauri', 'build')
if ($NoSign) { $arguments += '--no-sign' }
& pnpm @arguments
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
