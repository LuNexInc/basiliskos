param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$version = '7.2.77'
$archiveName = "CLIProxyAPI_${version}_windows_amd64.zip"
$archiveSha256 = 'daa8277af35e2a5c7bbc9a71e768dceed5bdb31e66d48ac36be63b3d52338d1d'
$exeSha256 = '0f2b23b5b533c92c2ce86bb37e2bb7bd7472b81b3f63bf8cc19950aca0a0cc2c'
$downloadUrl = "https://github.com/router-for-me/CLIProxyAPI/releases/download/v$version/$archiveName"
$projectRoot = Split-Path -Parent $PSScriptRoot
$resourceDir = Join-Path $projectRoot 'src-tauri\resources\gateway'
$destination = Join-Path $resourceDir 'cli-proxy-api.exe'

function Get-Sha256([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            return ([BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        }
        finally {
            $sha.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

New-Item -ItemType Directory -Force -Path $resourceDir | Out-Null

if (-not $Force -and (Test-Path -LiteralPath $destination)) {
    $existing = Get-Sha256 $destination
    if ($existing -eq $exeSha256) {
        Write-Output "CLIProxyAPI v$version is already prepared and verified."
        exit 0
    }
}

$tempBase = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\')
$tempRoot = Join-Path $tempBase ("hydra-gateway-prepare-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    $archive = Join-Path $tempRoot $archiveName
    $expanded = Join-Path $tempRoot 'expanded'

    Invoke-WebRequest -UseBasicParsing -Uri $downloadUrl -OutFile $archive
    $actualArchiveHash = Get-Sha256 $archive
    if ($actualArchiveHash -ne $archiveSha256) {
        throw "Gateway archive checksum mismatch. Expected $archiveSha256, got $actualArchiveHash."
    }

    Expand-Archive -LiteralPath $archive -DestinationPath $expanded
    $source = Join-Path $expanded 'cli-proxy-api.exe'
    $actualExeHash = Get-Sha256 $source
    if ($actualExeHash -ne $exeSha256) {
        throw "Gateway executable checksum mismatch. Expected $exeSha256, got $actualExeHash."
    }

    Copy-Item -LiteralPath $source -Destination $destination -Force
    Write-Output "Prepared and verified CLIProxyAPI v$version."
}
finally {
    if (Test-Path -LiteralPath $tempRoot) {
        $resolved = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $tempRoot).Path)
        if (-not $resolved.StartsWith($tempBase + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove temporary path outside TEMP: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
