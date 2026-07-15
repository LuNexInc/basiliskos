[CmdletBinding()]
param(
    [string]$DataRoot = (Join-Path $HOME '.hydra-gateway'),
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

function Read-SharedText {
    param([Parameter(Mandatory = $true)][string]$Path)

    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $stream = [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
    try {
        $reader = New-Object IO.StreamReader($stream, [Text.UTF8Encoding]::new($false), $true)
        try {
            return $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }
    }
    finally {
        $stream.Dispose()
    }
}

function Add-SecretValues {
    param(
        $Value,
        [string]$PropertyName,
        [System.Collections.Generic.HashSet[string]]$Secrets
    )

    if ($null -eq $Value) {
        return
    }
    if ($Value -is [string]) {
        if ($PropertyName -match '(?i)token|key|secret|authorization|password' -and $Value.Length -ge 8) {
            [void]$Secrets.Add($Value)
        }
        return
    }
    if ($Value -is [System.Collections.IDictionary]) {
        foreach ($key in $Value.Keys) {
            Add-SecretValues -Value $Value[$key] -PropertyName ([string]$key) -Secrets $Secrets
        }
        return
    }
    if ($Value -is [pscustomobject]) {
        foreach ($property in $Value.PSObject.Properties) {
            Add-SecretValues -Value $property.Value -PropertyName $property.Name -Secrets $Secrets
        }
        return
    }
    if ($Value -is [System.Collections.IEnumerable]) {
        foreach ($item in $Value) {
            Add-SecretValues -Value $item -PropertyName $PropertyName -Secrets $Secrets
        }
    }
}

function Get-SecretValues {
    param([Parameter(Mandatory = $true)][string]$Root)

    $secrets = New-Object 'System.Collections.Generic.HashSet[string]'
    $jsonFiles = @()
    $controller = Join-Path $Root 'controller.json'
    if (Test-Path -LiteralPath $controller -PathType Leaf) {
        $jsonFiles += Get-Item -LiteralPath $controller
    }
    $authRoot = Join-Path (Join-Path $Root 'gateway') 'auth'
    if (Test-Path -LiteralPath $authRoot -PathType Container) {
        $jsonFiles += Get-ChildItem -LiteralPath $authRoot -File -Filter '*.json' -ErrorAction Stop
    }
    foreach ($file in $jsonFiles) {
        try {
            $value = Read-SharedText -Path $file.FullName | ConvertFrom-Json
            Add-SecretValues -Value $value -PropertyName '' -Secrets $secrets
        }
        catch {
            throw "Could not inspect credential fields in $($file.FullName): $($_.Exception.Message)"
        }
    }
    return $secrets
}

function Find-LogLeaks {
    param([Parameter(Mandatory = $true)][string]$Root)

    $secrets = Get-SecretValues -Root $Root
    if ($secrets.Count -eq 0) {
        return @()
    }
    $leaks = @()
    foreach ($file in Get-ChildItem -LiteralPath $Root -Recurse -File -Filter '*.log' -ErrorAction Stop) {
        if ($file.Length -gt 25MB) {
            continue
        }
        $content = Read-SharedText -Path $file.FullName
        $matched = $false
        foreach ($secret in $secrets) {
            if ($content.IndexOf($secret, [StringComparison]::Ordinal) -ge 0) {
                $matched = $true
                break
            }
        }
        if ($matched) {
            $rootPrefix = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
            $relative = $file.FullName.Substring($rootPrefix.Length).TrimStart([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
            $leaks += $relative
        }
    }
    return $leaks
}

function Test-LogScanner {
    $tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    $tempRoot = Join-Path $tempBase ('basiliskos-log-scan-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tempRoot | Out-Null
    try {
        $fakeSecret = 'fixture-' + [guid]::NewGuid().ToString('N')
        $controller = @{ api_key = $fakeSecret } | ConvertTo-Json
        [IO.File]::WriteAllText((Join-Path $tempRoot 'controller.json'), $controller, [Text.UTF8Encoding]::new($false))
        $activeLog = Join-Path $tempRoot 'leaky.log'
        [IO.File]::WriteAllText($activeLog, "synthetic=$fakeSecret", [Text.UTF8Encoding]::new($false))
        $activeWriter = [IO.FileStream]::new(
            $activeLog,
            [IO.FileMode]::Open,
            [IO.FileAccess]::ReadWrite,
            [IO.FileShare]::ReadWrite
        )
        try {
            $leaks = @(Find-LogLeaks -Root $tempRoot)
        }
        finally {
            $activeWriter.Dispose()
        }
        if ($leaks.Count -ne 1 -or $leaks[0] -ne 'leaky.log') {
            throw 'Runtime-log scanner self-test failed to inspect the active synthetic log.'
        }
        [IO.File]::WriteAllText((Join-Path $tempRoot 'leaky.log'), 'synthetic=<redacted>', [Text.UTF8Encoding]::new($false))
        if (@(Find-LogLeaks -Root $tempRoot).Count -ne 0) {
            throw 'Runtime-log scanner self-test still found the redacted fixture.'
        }
        Write-Output 'Runtime-log secret scanner self-test passed.'
    }
    finally {
        if (Test-Path -LiteralPath $tempRoot) {
            $resolved = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $tempRoot).Path)
            $separator = [IO.Path]::DirectorySeparatorChar
            if (-not $resolved.StartsWith($tempBase + $separator, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Refusing to remove temporary path outside the temp directory: $resolved"
            }
            Remove-Item -LiteralPath $resolved -Recurse -Force
        }
    }
}

if ($SelfTest) {
    Test-LogScanner
}

if (-not (Test-Path -LiteralPath $DataRoot -PathType Container)) {
    Write-Output "Runtime-log scan skipped because the data directory does not exist: $DataRoot"
    exit 0
}

$leaks = @(Find-LogLeaks -Root $DataRoot)
if ($leaks.Count -gt 0) {
    $leaks | Sort-Object | ForEach-Object { Write-Error "Credential value found in log file: $_" }
    throw "Runtime-log scan found credential values in $($leaks.Count) log file(s). Values were not printed."
}

Write-Output 'Runtime-log secret scan passed.'
