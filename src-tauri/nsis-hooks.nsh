!macro NSIS_HOOK_POSTINSTALL
  ; Recreate shortcuts with the bundled icon so Windows does not retain an old executable icon.
  Delete "$DESKTOP\${PRODUCTNAME}.lnk"
  CreateShortCut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\assets\icons\icon.ico" 0
  Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  CreateShortCut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\assets\icons\icon.ico" 0
!macroend
