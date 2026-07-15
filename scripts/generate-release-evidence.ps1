param(
    [Parameter(Mandatory = $true)][string]$ArtifactRoot,
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [switch]$RequireSigning
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$version = (Get-Content -LiteralPath (Join-Path $projectRoot 'package.json') -Raw | ConvertFrom-Json).version
$application = Join-Path $ArtifactRoot 'hydra-gateway.exe'
$installer = Join-Path $ArtifactRoot "bundle\nsis\Basiliskos_${version}_x64-setup.exe"
$artifacts = @(
    if (Test-Path -LiteralPath $application -PathType Leaf) {
        Get-Item -LiteralPath $application
    }
    if (Test-Path -LiteralPath $installer -PathType Leaf) {
        Get-Item -LiteralPath $installer
    }
)
if ($artifacts.Count -ne 2) {
    throw "The Basiliskos $version application and canonical NSIS installer were not both found below $ArtifactRoot."
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

$manifest = foreach ($artifact in $artifacts) {
    $signature = Get-AuthenticodeSignature -LiteralPath $artifact.FullName
    if ($RequireSigning -and $signature.Status -ne 'Valid') {
        throw "$($artifact.Name) is not validly Authenticode signed: $($signature.Status)"
    }
    [ordered]@{
        name = $artifact.Name
        bytes = $artifact.Length
        sha256 = (Get-FileHash -LiteralPath $artifact.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        authenticode = $signature.Status.ToString()
        signer = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { $null }
    }
}
$json = $manifest | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText((Join-Path $OutputDirectory 'artifacts.json'), $json, [Text.UTF8Encoding]::new($false))
$checksumLines = $manifest | ForEach-Object { "$($_.sha256)  $($_.name)" }
[IO.File]::WriteAllLines((Join-Path $OutputDirectory 'SHA256SUMS.txt'), $checksumLines, [Text.UTF8Encoding]::new($false))

$licenseJson = & pnpm licenses list --json --prod 2>&1
if ($LASTEXITCODE -ne 0) { throw 'pnpm license inventory failed.' }
[IO.File]::WriteAllLines((Join-Path $OutputDirectory 'javascript-licenses.json'), $licenseJson, [Text.UTF8Encoding]::new($false))

Write-Output "Release evidence written to $OutputDirectory."
