; Cosmog NSIS installer hooks
; Runs during uninstallation to offer deletion of leftover app data.

!macro customUnInstall
  MessageBox MB_YESNO|MB_ICONQUESTION \
    "Do you want to delete Cosmog local data?$\n$\n\
    This will remove account configurations, transfer history, \
    search indexes, and logs from:$\n\
    $APPDATA\com.sonus.cosmog$\n$\n\
    NOTE: Credentials stored in Windows Credential Manager are NOT \
    removed by this step. For a complete wipe including credentials, \
    use Settings > Danger zone > Clear all data before uninstalling." \
    IDNO done
    RMDir /r "$APPDATA\com.sonus.cosmog"
  done:
!macroend
