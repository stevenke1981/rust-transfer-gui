; Rust Transfer GUI — Windows installer (NSIS 3, Unicode, x64)
;
; Build (from the repository root, after `cargo build --release --target x86_64-pc-windows-msvc`):
;   makensis /DVERSION=0.1.0 installer\windows\rust-transfer-gui.nsi
; Optional defines:
;   /DBINARY=<path to rust-transfer-gui.exe>   (default: MSVC release output)
;   /DOUTDIR=<existing output directory>       (default: dist; must exist)
;
; Installs to "Program Files\Rust Transfer GUI", creates Start Menu shortcuts and an
; optional desktop shortcut, and (optional, on by default) adds the install directory
; to the system PATH. The uninstaller removes everything, including the PATH entry.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "x64.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef BINARY
  !define BINARY "..\..\target\x86_64-pc-windows-msvc\release\rust-transfer-gui.exe"
!endif
!ifndef OUTDIR
  !define OUTDIR "..\..\dist"
!endif

!define APPNAME   "Rust Transfer GUI"
!define EXENAME   "rust-transfer-gui.exe"
!define PUBLISHER "Ke Sheng Da"
!define APPKEY    "Software\RustTransferGUI"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\RustTransferGUI"
!define PS_EXE    "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe"
!define PS_ARGS   "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File"

Name "${APPNAME} ${VERSION}"
OutFile "${OUTDIR}\rust-transfer-gui-${VERSION}-x64-setup.exe"
InstallDir "$PROGRAMFILES64\${APPNAME}"
InstallDirRegKey HKLM "${APPKEY}" "InstallDir"
RequestExecutionLevel admin
BrandingText "${APPNAME} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "(c) 2026 ${PUBLISHER} - MIT License"
VIAddVersionKey "FileDescription" "${APPNAME} installer"

; ---------------------------------------------------------------- pages
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXENAME}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch ${APPNAME}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "TradChinese"

; ---------------------------------------------------------------- sections
Section "!${APPNAME} (required)" SecMain
  SectionIn RO
  SetShellVarContext all
  SetOutPath "$INSTDIR"
  File "/oname=${EXENAME}" "${BINARY}"
  File "/oname=LICENSE.txt" "..\..\LICENSE"
  File "/oname=README.md" "..\..\README.md"
  File "path-helper.ps1"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk" "$INSTDIR\uninstall.exe"

  WriteRegStr HKLM "${APPKEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\${EXENAME}"
  WriteRegStr HKLM "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoRepair" 1
  SectionGetSize ${SecMain} $0
  WriteRegDWORD HKLM "${UNINSTKEY}" "EstimatedSize" $0
SectionEnd

Section "Add to system PATH" SecPath
  DetailPrint "Adding $INSTDIR to the system PATH..."
  ${DisableX64FSRedirection} ; use the 64-bit Windows PowerShell
  nsExec::ExecToLog '"${PS_EXE}" ${PS_ARGS} "$INSTDIR\path-helper.ps1" -Action Add -Dir "$INSTDIR"'
  Pop $0
  ${EnableX64FSRedirection}
  ${If} $0 == 0
    WriteRegDWORD HKLM "${APPKEY}" "AddedToPath" 1
    ; Tell running programs (Explorer, new terminals) that the environment changed.
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${Else}
    DetailPrint "Warning: could not update PATH (exit code $0)."
  ${EndIf}
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\${EXENAME}"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMain} "The application, Start Menu shortcuts and uninstaller."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPath} "Add the install folder to the system PATH so rust-transfer-gui can be started from any terminal."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Create a shortcut on the desktop."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------------------------------------------------------------- uninstaller
Section "Uninstall"
  SetShellVarContext all

  ; Remove the PATH entry (idempotent: the helper does nothing if it is absent).
  ${If} ${FileExists} "$INSTDIR\path-helper.ps1"
    DetailPrint "Removing $INSTDIR from the system PATH..."
    ${DisableX64FSRedirection}
    nsExec::ExecToLog '"${PS_EXE}" ${PS_ARGS} "$INSTDIR\path-helper.ps1" -Action Remove -Dir "$INSTDIR"'
    Pop $0
    ${EnableX64FSRedirection}
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}

  Delete "$INSTDIR\${EXENAME}"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\path-helper.ps1"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"
  Delete "$DESKTOP\${APPNAME}.lnk"

  DeleteRegKey HKLM "${UNINSTKEY}"
  DeleteRegKey HKLM "${APPKEY}"
SectionEnd

; ---------------------------------------------------------------- init
Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "${APPNAME} requires 64-bit Windows."
    Abort
  ${EndIf}
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd
