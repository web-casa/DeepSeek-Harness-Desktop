; Rebrand migration for installations created before the product was renamed
; from "DeepSeek Harness Desktop" to "DSH Desktop".
;
; NSIS identifies an existing installation by PRODUCTNAME. Without this hook,
; the renamed installer treats the old app as absent, installs side-by-side
; under %LOCALAPPDATA%\DSH Desktop, and the updater restart relaunches the old
; binary.
;
; PREINSTALL only redirects the install/output path. Destructive cleanup and
; replacement shortcuts are deferred to POSTINSTALL so a cancelled or failed
; install leaves the legacy installation intact. DSH_HOME lives outside the
; install directory and is not touched.

Var LegacyInstallMigrated

!macro NSIS_HOOK_PREINSTALL
  StrCpy $LegacyInstallMigrated "0"
  StrCpy $R8 ""
  ReadRegStr $R8 SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\DeepSeek Harness Desktop" "InstallLocation"
  ${If} $R8 != ""
    ; Legacy Tauri NSIS writes InstallLocation with surrounding quotes.
    StrCpy $R8 $R8 "" 1
    StrCpy $R8 $R8 -1
    StrCpy $INSTDIR "$R8"
    SetOutPath "$INSTDIR"
    StrCpy $LegacyInstallMigrated "1"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${If} $LegacyInstallMigrated == "1"
    DeleteRegKey SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\DeepSeek Harness Desktop"
    DeleteRegKey SHCTX "Software\${MANUFACTURER}\DeepSeek Harness Desktop"

    Delete "$SMPROGRAMS\DeepSeek Harness Desktop.lnk"
    Delete "$DESKTOP\DeepSeek Harness Desktop.lnk"

    ; Updater installs pass /UPDATE, which makes Tauri skip shortcut
    ; creation. Recreate both shortcuts explicitly after a migrated update.
    CreateDirectory "$SMPROGRAMS"
    CreateShortcut "$SMPROGRAMS\DSH Desktop.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\DSH Desktop.lnk"
    CreateShortcut "$DESKTOP\DSH Desktop.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$DESKTOP\DSH Desktop.lnk"
  ${EndIf}
!macroend
