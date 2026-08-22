[CmdletBinding()]
param()

# Guards the pinned CLIProxyAPI dependency.
#
#   1. Fails if the pin drifts between the build-time downloader
#      (scripts/prepare-gateway.ps1) and the runtime verifier
#      (src-tauri/src/gateway.rs constants). A mismatch means the binary we
#      download and verify at build time is not the binary the runtime checks at
#      start, which breaks the gateway the moment Basiliskos launches.
#   2. Reports whether the pinned version is behind the latest upstream release
#      so we know when to re-audit. This is informational and never fails the
#      gate — the pin is deliberate (we freeze until we re-audit).

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$preparePath = Join-Path $projectRoot 'scripts\prepare-gateway.ps1'
$gatewayPath = Join-Path $projectRoot 'src-tauri\src\gateway.rs'

function Read-Pin([string]$Path, [string]$Pattern) {
    $text = Get-Content -LiteralPath $Path -Raw
    $match = [regex]::Match($text, $Pattern)
    if (-not $match.Success) {
        throw "Could not read the pinned {$Pattern} from $Path"
    }
    return $match.Groups[1].Value
}

# prepare-gateway.ps1: $version = '<pin>' ; $exeSha256 = '...'
$prepareVersion = Read-Pin $preparePath "\`$version\s*=\s*'([^']+)'"
$prepareExeSha = Read-Pin $preparePath "\`$exeSha256\s*=\s*'([^']+)'"

# gateway.rs: const GATEWAY_VERSION: &str = "<pin>" ; GATEWAY_EXE_SHA256: &str = "..."
$runtimeVersion = Read-Pin $gatewayPath 'const GATEWAY_VERSION: &str = "([^"]+)"'
$runtimeExeSha = Read-Pin $gatewayPath 'const GATEWAY_EXE_SHA256: &str =\s*\n\s*"([^"]+)"'

$problems = @()
if ($prepareVersion -ne $runtimeVersion) {
    $problems += "CLIProxyAPI version pin drifted: prepare-gateway.ps1=$prepareVersion gateway.rs=$runtimeVersion"
}
if ($prepareExeSha -ne $runtimeExeSha) {
    $problems += "CLIProxyAPI exe SHA-256 pin drifted: prepare-gateway.ps1=$prepareExeSha gateway.rs=$runtimeExeSha"
}
if ($problems.Count -gt 0) {
    $problems | ForEach-Object { throw $_ }
}
Write-Output "CLIProxyAPI pin is consistent at v$runtimeVersion."

# Upstream awareness — informational, never a gate failure.
try {
    $release = Invoke-RestMethod -UseBasicParsing `
        -Uri 'https://api.github.com/repos/router-for-me/CLIProxyAPI/releases/latest' `
        -Headers @{ 'User-Agent' = 'Basiliskos-pin-guard' }
    $latest = [string]$release.tag_name
    $latest = $latest.TrimStart('v')
    if ($latest -and ($latest -ne $runtimeVersion)) {
        # Compare only full numeric major.minor.patch to avoid false "newer"
        # for pre-release suffixes.
        $isNewer = $false
        try {
            $cur = [version]$runtimeVersion
            $cand = [version]($latest -split '-')[0]
            $isNewer = $cand -gt $cur
        } catch {
            $isNewer = $latest -ne $runtimeVersion
        }
        if ($isNewer) {
            Write-Output "NOTE: CLIProxyAPI upstream has a newer release ($latest); pinned $runtimeVersion. Re-audit the contract before bumping."
        } else {
            Write-Output "NOTE: CLIProxyAPI upstream tag $latest is not newer than pinned $runtimeVersion."
        }
    } else {
        Write-Output "CLIProxyAPI upstream check: pinned $runtimeVersion is current."
    }
} catch {
    Write-Output "CLIProxyAPI upstream check skipped (network): $($_.Exception.Message)"
}
