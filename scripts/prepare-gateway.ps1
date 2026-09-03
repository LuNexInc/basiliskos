param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$version = '7.2.148-dev-dacae582'
$sourceCommit = 'dacae582284283b05c54f9426c597e16a3d389c8'
$sourceArchiveSha256 = 'b53ef23e2db536fd121506097af342f1df09b2b632b69aab1448ac693d33cf9c'
$goVersion = '1.26.4'
$buildDate = '2026-09-02T15:27:56Z'
$exeSha256 = '49056caa627209618f15e80e6044c77630b226ae1c59e074f63b1fd1d4d18276'
$downloadUrl = "https://codeload.github.com/router-for-me/CLIProxyAPI/zip/$sourceCommit"
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
    $archive = Join-Path $tempRoot 'source.zip'
    $expanded = Join-Path $tempRoot 'expanded'

    Invoke-WebRequest -UseBasicParsing -Uri $downloadUrl -OutFile $archive
    $actualArchiveHash = Get-Sha256 $archive
    if ($actualArchiveHash -ne $sourceArchiveSha256) {
        throw "Gateway source archive checksum mismatch. Expected $sourceArchiveSha256, got $actualArchiveHash."
    }

    Expand-Archive -LiteralPath $archive -DestinationPath $expanded
    $sourceRoot = Get-ChildItem -LiteralPath $expanded -Directory | Select-Object -First 1 -ExpandProperty FullName
    if (-not $sourceRoot) {
        throw 'Gateway source archive did not contain a source directory.'
    }

    $env:GOTOOLCHAIN = 'local'
    $reportedGoVersion = (& go env GOVERSION).Trim()
    if ($LASTEXITCODE -ne 0 -or $reportedGoVersion -ne "go$goVersion") {
        throw "CLIProxyAPI source build requires Go $goVersion; found $reportedGoVersion."
    }
    $env:CGO_ENABLED = '1'
    $env:GOOS = 'windows'
    $env:GOARCH = 'amd64'
    $source = Join-Path $tempRoot 'cli-proxy-api.exe'
    Push-Location $sourceRoot
    try {
        & go build -trimpath -mod=readonly `
            "-ldflags=-s -w -X main.Version=$version -X main.Commit=dacae582 -X main.BuildDate=$buildDate" `
            -o $source ./cmd/server/
        if ($LASTEXITCODE -ne 0) {
            throw "CLIProxyAPI source build failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }

    $actualExeHash = Get-Sha256 $source
    if ($actualExeHash -ne $exeSha256) {
        throw "Gateway executable checksum mismatch. Expected $exeSha256, got $actualExeHash."
    }

    Copy-Item -LiteralPath $source -Destination $destination -Force
    Write-Output "Built and verified CLIProxyAPI $version from commit $sourceCommit."
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
