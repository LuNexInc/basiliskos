param(
    [Parameter(Mandatory = $true)][string]$CurrentInstaller,
    [Parameter(Mandatory = $true)][string]$PreviousInstaller
)

$ErrorActionPreference = 'Stop'
$target = Join-Path $env:ProgramFiles '3ReadyLab\Basiliskos'
$legacyMachine = Join-Path $env:ProgramFiles 'Basiliskos'
$legacyUser = Join-Path $env:LOCALAPPDATA 'Basiliskos'
$sentinelDir = Join-Path $env:USERPROFILE '.hydra-gateway'
$sentinel = Join-Path $sentinelDir 'installer-ci-sentinel.txt'
$shortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\3ReadyLab\Basiliskos.lnk'

function Invoke-Installer([string]$Path, [switch]$ExpectFailure) {
    $process = Start-Process -FilePath $Path -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
    if ($ExpectFailure) {
        if ($process.ExitCode -eq 0) { throw 'A downgrade unexpectedly succeeded.' }
    }
    elseif ($process.ExitCode -ne 0) {
        throw "$([IO.Path]::GetFileName($Path)) failed with exit code $($process.ExitCode)."
    }
}

foreach ($path in @($CurrentInstaller, $PreviousInstaller)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Installer fixture is missing: $path"
    }
}
if ((Test-Path -LiteralPath $target) -or (Test-Path -LiteralPath $legacyMachine) -or (Test-Path -LiteralPath $legacyUser)) {
    throw 'The installer lifecycle test requires a clean Windows runner.'
}

Invoke-Installer $PreviousInstaller
$legacyInstalled = (Test-Path -LiteralPath (Join-Path $legacyMachine 'hydra-gateway.exe')) -or
    (Test-Path -LiteralPath (Join-Path $legacyUser 'hydra-gateway.exe'))
if (-not $legacyInstalled) { throw 'The 1.1.5 upgrade fixture did not install.' }

New-Item -ItemType Directory -Force -Path $sentinelDir | Out-Null
[IO.File]::WriteAllText($sentinel, 'preserve-across-upgrade-and-uninstall')

Invoke-Installer $CurrentInstaller
$currentExe = Join-Path $target 'hydra-gateway.exe'
if (-not (Test-Path -LiteralPath $currentExe -PathType Leaf)) {
    throw "The current build was not installed at $target."
}
if ((Test-Path -LiteralPath (Join-Path $legacyMachine 'hydra-gateway.exe')) -or
    (Test-Path -LiteralPath (Join-Path $legacyUser 'hydra-gateway.exe'))) {
    throw 'A legacy Basiliskos binary survived the migration.'
}
if (-not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
    throw 'The upgrade removed Basiliskos credentials/profile data.'
}
if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) {
    throw 'The 3ReadyLab Start Menu shortcut was not created.'
}
$shell = New-Object -ComObject WScript.Shell
$shortcutObject = $shell.CreateShortcut($shortcut)
$shortcutTarget = $shortcutObject.TargetPath
if (-not $shortcutTarget.Equals($currentExe, [StringComparison]::OrdinalIgnoreCase)) {
    throw "The Start Menu shortcut points to an unexpected target: $shortcutTarget"
}
if (-not $shortcutObject.WorkingDirectory.Equals($target, [StringComparison]::OrdinalIgnoreCase)) {
    throw "The Start Menu shortcut has an unexpected working directory: $($shortcutObject.WorkingDirectory)"
}

$beforeRepair = (Get-FileHash -LiteralPath $currentExe -Algorithm SHA256).Hash
Invoke-Installer $CurrentInstaller
$afterRepair = (Get-FileHash -LiteralPath $currentExe -Algorithm SHA256).Hash
if ($beforeRepair -ne $afterRepair) { throw 'Repair changed the installed executable unexpectedly.' }

$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Basiliskos'
$installedVersion = (Get-ItemProperty -LiteralPath $uninstallKey -Name DisplayVersion).DisplayVersion
try {
    Set-ItemProperty -LiteralPath $uninstallKey -Name DisplayVersion -Value '999.0.0'
    Invoke-Installer $CurrentInstaller -ExpectFailure
}
finally {
    Set-ItemProperty -LiteralPath $uninstallKey -Name DisplayVersion -Value $installedVersion
}
if (-not (Test-Path -LiteralPath $currentExe -PathType Leaf) -or
    (Get-FileHash -LiteralPath $currentExe -Algorithm SHA256).Hash -ne $beforeRepair) {
    throw 'The rejected rollback damaged the current installation.'
}

$uninstaller = Join-Path $target 'uninstall.exe'
$process = Start-Process -FilePath $uninstaller -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
if ($process.ExitCode -ne 0) { throw "Uninstall failed with exit code $($process.ExitCode)." }
if (Test-Path -LiteralPath $currentExe) { throw 'The installed executable survived uninstall.' }
if (Test-Path -LiteralPath $shortcut) { throw 'The Start Menu shortcut survived uninstall.' }
if (-not (Test-Path -LiteralPath $sentinel)) { throw 'Uninstall removed retained profile data.' }

Write-Output 'Clean install, 1.1.5 upgrade/migration, repair, rejected rollback, shortcut, profile preservation, and uninstall checks passed.'
