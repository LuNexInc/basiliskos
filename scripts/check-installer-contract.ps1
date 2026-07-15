$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$hookPath = Join-Path $projectRoot 'src-tauri\windows\installer-hooks.nsh'
$hook = [IO.File]::ReadAllText($hookPath)
$match = [regex]::Match(
    $hook,
    '(?ms)!macro NSIS_HOOK_PREINSTALL\s*(?<body>.*?)\s*!macroend'
)
if (-not $match.Success) {
    throw 'NSIS_HOOK_PREINSTALL is missing.'
}

$body = $match.Groups['body'].Value
$lastInstallDirectoryWrite = $body.LastIndexOf('StrCpy $INSTDIR', [StringComparison]::Ordinal)
$resetExtractionDirectory = $body.LastIndexOf('SetOutPath $INSTDIR', [StringComparison]::Ordinal)
if ($lastInstallDirectoryWrite -lt 0 -or $resetExtractionDirectory -lt $lastInstallDirectoryWrite) {
    throw 'The installer hook must reset SetOutPath after its final $INSTDIR mutation.'
}

foreach ($required in @(
    'ReadRegStr $R8 SHCTX "${UNINSTKEY}" "DisplayVersion"',
    'nsis_tauri_utils::SemverCompare "${VERSION}" $R8',
    '${If} $R7 = -1',
    'Abort "A newer Basiliskos version is already installed."',
    '$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}',
    '$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}',
    '$PROGRAMFILES64\${PRODUCTNAME}',
    '$LOCALAPPDATA\${PRODUCTNAME}'
)) {
    if ($body.IndexOf($required, [StringComparison]::Ordinal) -lt 0) {
        throw "The installer migration hook is missing required path coverage: $required"
    }
}

$postInstallMatch = [regex]::Match(
    $hook,
    '(?ms)!macro NSIS_HOOK_POSTINSTALL\s*(?<body>.*?)\s*!macroend'
)
if (-not $postInstallMatch.Success) {
    throw 'NSIS_HOOK_POSTINSTALL is missing.'
}

$postInstallBody = $postInstallMatch.Groups['body'].Value
foreach ($required in @(
    '${If} $NoShortcutMode = 0',
    'CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"',
    'CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"',
    '!insertmacro SetLnkAppUserModelId'
)) {
    if ($postInstallBody.IndexOf($required, [StringComparison]::Ordinal) -lt 0) {
        throw "The installer post-install hook is missing shortcut repair behavior: $required"
    }
}

Write-Output 'Installer migration contract passed.'
