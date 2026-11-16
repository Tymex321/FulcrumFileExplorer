; Installer hooks for the optional Fulcrum Index Service (fx-helper.exe).
;
; The service is only registered when the user turns on instant search inside the app.
; While it runs it holds fx-helper.exe open, so an upgrade stops it before the files are
; replaced and starts it again afterwards. Uninstalling removes the service and its index
; data. Every command is a no-op when the service was never installed.

!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'net stop FulcrumIndex'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec 'net start FulcrumIndex'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$INSTDIR\fx-helper.exe" 0 +3
    nsExec::Exec '"$INSTDIR\fx-helper.exe" uninstall'
    Pop $0
!macroend
