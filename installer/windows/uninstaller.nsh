; NSIS custom uninstaller script for Codex OmniBridge Electron App
; Ensures zero-residual clean uninstallation

!macro customUnInstall
  DetailPrint "Stopping Codex OmniBridge processes..."
  nsExec::Exec 'taskkill /F /IM codex-mp.exe'
  nsExec::Exec 'taskkill /F /IM "Codex OmniBridge.exe"'

  DetailPrint "Restoring Codex configuration and cleaning credentials..."
  ; 如果存在受管的 codex-mp.exe，调用 uninstall 干净还原
  IfFileExists "$INSTDIR\resources\bin\codex-mp.exe" 0 +3
    nsExec::ExecToLog '"$INSTDIR\resources\bin\codex-mp.exe" uninstall'
    Goto DoneRestore

  IfFileExists "$INSTDIR\bin\codex-mp.exe" 0 +3
    nsExec::ExecToLog '"$INSTDIR\bin\codex-mp.exe" uninstall'
    Goto DoneRestore

  DoneRestore:
    DetailPrint "Codex OmniBridge uninstalled cleanly."
!macroend
