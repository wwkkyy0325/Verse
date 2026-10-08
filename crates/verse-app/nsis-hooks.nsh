; What the NSIS uninstaller does beyond what Tauri's template already does.
;
; Tauri's uninstaller already puts a **Delete app data** checkbox on its confirm
; page, and ticking it removes `%APPDATA%\<bundle id>`. This application writes
; nothing there. Its data lives under `%LOCALAPPDATA%\Verse`, where the cache,
; the resume checkpoints, the transcript history and the downloaded model
; weights are — so the stock checkbox removed an empty directory and left about
; 1.2 GB behind, which is exactly what it was supposed to ask about.
;
; `$DeleteAppDataCheckboxState` is declared by the template, set when the
; confirm page is left, and `POSTUNINSTALL` is expanded after the block that
; reads it, so the answer is available here. Unticked it is 0; in a silent
; uninstall the page never runs, so it is 0 there too. **Nothing is deleted
; unless somebody ticked the box**, which is the whole design.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; The whole data root rather than only `models`, because that is what the
    ; checkbox says and what `verse-store` treats as one thing: weights, cache,
    ; checkpoints and the list of past transcriptions. The transcripts
    ; themselves are not here — those were written to the user's Documents, and
    ; an uninstall has no business reaching into them.
    ;
    ; The folder name is also `verse-store`'s `dirs::FOLDER`. The two have to
    ; agree; neither can see the other. `data_dir` is what to change if the
    ; product is ever renamed.
    RMDir /r "$LOCALAPPDATA\Verse"
  ${EndIf}
!macroend
