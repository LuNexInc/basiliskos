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
  ; Tauri's per-machine default is Program Files\Basiliskos. Basiliskos ships
  ; under the shared 3ReadyLab publisher directory. Preserve a genuinely custom
  ; user-selected directory, but migrate either historical default first so the
  ; machine cannot retain duplicate binaries or stale shortcuts.
  ${If} "$INSTDIR" == "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $INSTDIR "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    StrCpy $INSTDIR "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
  ${ElseIf} "$INSTDIR" == "$LOCALAPPDATA\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
    ${If} ${RunningX64}
      StrCpy $INSTDIR "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
    ${Else}
      StrCpy $INSTDIR "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
    ${EndIf}
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES64\3ReadyLab\${PRODUCTNAME}"
    ; A repair or an interrupted migration can already have persisted the new
    ; install directory while leaving a historical binary behind.
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES64\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
  ${ElseIf} "$INSTDIR" == "$PROGRAMFILES\3ReadyLab\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$PROGRAMFILES\${PRODUCTNAME}"
    !insertmacro BASILISKOS_REMOVE_LEGACY "$LOCALAPPDATA\${PRODUCTNAME}"
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
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  ${EndIf}
!macroend
