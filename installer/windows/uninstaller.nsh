; NSIS custom uninstaller script for Codex OmniBridge Electron App
; Ensures zero-residual clean uninstallation

!macro customUnInstall
  DetailPrint "Stopping Codex OmniBridge processes..."
  ; nsExec pushes the process exit code onto the NSIS stack; always pop it so the
  ; uninstall section's own stack usage stays balanced. taskkill returns non-zero
  ; when the process is not running, which is not an error here.
  nsExec::Exec 'taskkill /F /IM codex-mp.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /IM "Codex OmniBridge.exe"'
  Pop $0

  DetailPrint "Restoring Codex configuration and cleaning credentials..."
  ; The managed codex-mp.exe is shipped to {app}\bin\codex-mp.exe by the
  ; "win.extraFiles" entry in package.json; there is no resources\bin copy, so
  ; that branch was dead code and has been removed.
  IfFileExists "$INSTDIR\bin\codex-mp.exe" 0 CodexMpRestoreSkipped
    nsExec::ExecToLog '"$INSTDIR\bin\codex-mp.exe" uninstall'
    ; ExecToLog pushes exactly one result: the process exit code, or "error"
    ; when the process could not be launched, or "timeout". Leaving it on the
    ; stack made a failed config restore indistinguishable from a successful
    ; one, so the uninstaller still claimed it had removed everything.
    Pop $0
    StrCmp $0 "0" CodexMpRestoreSucceeded
      DetailPrint "Codex configuration restore failed (exit code $0)."
      MessageBox MB_ICONSTOP|MB_OK "Codex OmniBridge could not restore your Codex configuration (codex-mp uninstall exit code $0). Your Codex config may still contain Codex MultiProvider entries and Codex may stop working. Run '$\"$INSTDIR\bin\codex-mp.exe$\" uninstall' manually, then uninstall again." /SD IDOK
      Abort "Codex configuration restore failed; the uninstall did not complete cleanly."
    CodexMpRestoreSucceeded:
      DetailPrint "Codex configuration restored."
      Goto CodexMpRestoreDone
  CodexMpRestoreSkipped:
    DetailPrint "Managed codex-mp.exe not found; skipping Codex configuration restore."

  CodexMpRestoreDone:
    DetailPrint "Codex OmniBridge uninstalled cleanly."
!macroend
