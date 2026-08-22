!macro BASILISKOS_REMOVE_LEGACY LEGACYDIR
  ${If} ${FileExists} "${LEGACYDIR}\uninstall.exe"
    DetailPrint "Migrating the previous Basiliskos installation from ${LEGACYDIR}"
    ExecWait '"${LEGACYDIR}\uninstall.exe" /S _?=${LEGACYDIR}' $R9
    ${If} $R9 <> 0
      Abort "The previous Basiliskos installation could not be removed safely (exit $R9)."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Tauri's generated downgrade comparison is populated by a UI page that does
  ; not run for /S installs. Enforce the same policy before touching any files
  ; so unattended deployment cannot replace a newer BasiliskOS installation.
  ReadRegStr $R8 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${If} $R8 != ""
    nsis_tauri_utils::SemverCompare "${VERSION}" $R8
    Pop $R7
    ${If} $R7 = -1
      Abort "A newer BasiliskOS version is already installed."
    ${EndIf}
  ${EndIf}

  ; Tauri's per-machine default is Program Files\BasiliskOS. BasiliskOS ships
  ; under the shared 3ReadyLab publisher directory. Preserve a genuinely custom
  ; user-selected directory, but migrate either historical default first so the
  ; machine cannot retain duplicate binaries or stale shortcuts.
  StrCpy $R9 0
  ${If} "$INSTDIR" == "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $INSTDIR "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
    StrCpy $R9 1
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $INSTDIR "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
    StrCpy $R9 1
  ${ElseIf} "$INSTDIR" == "$LOCALAPPDATA\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    ${If} ${RunningX64}
      StrCpy $INSTDIR "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
    ${Else}
      StrCpy $INSTDIR "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
    ${EndIf}
    StrCpy $R9 1
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
    ; A repair or an interrupted migration can already have persisted the new
    ; install directory while leaving a historical binary behind.
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $R9 1
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $R9 1
  ${EndIf}

  ; The product renamed from "Basiliskos" to "BasiliskOS". Windows paths are
  ; case-insensitive, so an upgrade reuses the historical directory but NTFS
  ; keeps its original on-disk casing. When installing into a PRODUCTNAME path,
  ; read the real casing back via GetLongPathNameW and bounce the directory
  ; through a temporary name so Program Files shows "BasiliskOS". A locked
  ; directory keeps the old casing - cosmetic only, never fatal.
  ${If} $R9 = 1
    System::Call 'kernel32::GetLongPathNameW(t "$INSTDIR", t .R7, i ${NSIS_MAX_STRLEN}) i .R6'
    ${If} $R6 > 0
      StrCmpS "$R7" "$INSTDIR" basiliskos_recase_done 0
      Rename "$INSTDIR" "$INSTDIR._basiliskos-recase"
      Rename "$INSTDIR._basiliskos-recase" "$INSTDIR"
      basiliskos_recase_done:
    ${EndIf}
  ${EndIf}

  ; Tauri calls SetOutPath before this hook. If the hook changes $INSTDIR,
  ; reset NSIS's extraction directory or the executable is written to the old
  ; folder while registry entries and shortcuts point to the new one.
  SetOutPath $INSTDIR
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Migrating a historical install runs its uninstaller after Tauri has already
  ; entered update mode. That removes the old shortcut, while Tauri's normal
  ; update path deliberately skips creating a replacement. Restore the shortcut
  ; after installation unless the caller explicitly requested /NS.
  ${If} $NoShortcutMode = 0
    CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\${MAINBINARYNAME}.exe" 0
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\${MAINBINARYNAME}.exe" 0
    !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
  ${EndIf}

  ; The GUI finish page re-runs CreateOrUpdateDesktopShortcut after this hook,
  ; and that template function recreates the desktop .lnk WITHOUT an explicit
  ; icon path, wiping the binding above (silent/passive installs skip the finish
  ; page, which is why the icon was only broken in interactive installs). Mark
  ; shortcuts as handled so the finish page skips its icon-less re-creation.
  StrCpy $NoShortcutMode 1
!macroend
