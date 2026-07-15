param([Parameter(Mandatory = $true)][string]$FilePath)

$ErrorActionPreference = 'Stop'
$certificateBase64 = $env:BASILISKOS_SIGN_CERT_BASE64
$certificatePassword = $env:BASILISKOS_SIGN_CERT_PASSWORD
$required = $env:BASILISKOS_REQUIRE_SIGNING -eq '1'
$leafName = [IO.Path]::GetFileName($FilePath)

# Preserve the upstream dependency's published checksum. Basiliskos verifies
# these exact bytes again at runtime; the signed outer installer provides the
# publisher boundary for the bundled, independently attributed executable.
if ($leafName.Equals('cli-proxy-api.exe', [StringComparison]::OrdinalIgnoreCase)) {
    Write-Output 'Preserving the checksum-pinned CLIProxyAPI executable without re-signing it.'
    exit 0
}

if ([string]::IsNullOrWhiteSpace($certificateBase64)) {
    if ($required) {
        throw 'Release signing is required but BASILISKOS_SIGN_CERT_BASE64 is unavailable.'
    }
    Write-Output "Signing not configured; leaving $leafName unsigned."
    exit 0
}
if ([string]::IsNullOrWhiteSpace($certificatePassword)) {
    throw 'BASILISKOS_SIGN_CERT_PASSWORD is required when a signing certificate is configured.'
}

$tempRoot = Join-Path ([IO.Path]::GetFullPath($env:TEMP)) ('basiliskos-sign-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null
$certificatePath = Join-Path $tempRoot 'codesign.pfx'
$certificate = $null
try {
    [IO.File]::WriteAllBytes($certificatePath, [Convert]::FromBase64String($certificateBase64))
    $securePassword = ConvertTo-SecureString $certificatePassword -AsPlainText -Force
    $certificate = Import-PfxCertificate -FilePath $certificatePath -CertStoreLocation 'Cert:\CurrentUser\My' -Password $securePassword
    if ($null -eq $certificate -or -not $certificate.HasPrivateKey) {
        throw 'The configured Authenticode certificate could not be imported with its private key.'
    }
    $signTool = Get-Command signtool.exe -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
    if (-not $signTool) {
        $signTool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Filter signtool.exe -Recurse -File |
            Where-Object { $_.Directory.Name -eq 'x64' } |
            Sort-Object FullName -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }
    if (-not $signTool) {
        throw 'signtool.exe is unavailable.'
    }
    & $signTool sign /sha1 $certificate.Thumbprint /fd SHA256 /tr 'https://timestamp.digicert.com' /td SHA256 $FilePath
    if ($LASTEXITCODE -ne 0) {
        throw "signtool.exe failed with exit code $LASTEXITCODE."
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $FilePath
    if ($signature.Status -ne 'Valid') {
        throw "Authenticode verification failed with status $($signature.Status)."
    }
}
finally {
    if ($null -ne $certificate) {
        Remove-Item -LiteralPath ("Cert:\CurrentUser\My\" + $certificate.Thumbprint) -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $tempRoot) {
        $resolved = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $tempRoot).Path)
        $tempBase = [IO.Path]::GetFullPath($env:TEMP).TrimEnd('\') + '\'
        if (-not $resolved.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove signing temp path outside TEMP: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
