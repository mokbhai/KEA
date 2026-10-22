# Meetings UI inventory

Every action, setting, control and piece of live state on the Meetings page as it
stands before the workspace split. Later UI tasks tick the items they move or
rebuild; task 6.1 requires that none is left unticked.

Sources, read in full: `ui/src/pages/MeetingsPage.tsx` and
`ui/src/components/MeetingDetail.tsx`. Child cards that live on the page
(`FeatureBanner`, `FeatureAiCard`, `NotionSettings`, `HotkeyRow`) are listed where
they appear.

Ticked items were moved by task 4.2 into `ui/src/components/meetings/MeetingSettingsPanel.tsx`
(the ⚙ slide-over). Unticked items stay on the page until a later task.

## Page header and banners (stay on the page)

- [ ] Page heading "Meetings" and the intro line (task 4.4 keeps the header; Record/Stop moves here in 4.6)
- [ ] FeatureBanner: one warning per unset or blocked slot (speech to text, notes writing), with its action button ("Open Models" or "Choose…")
- [ ] Banner "Grant permission" for system audio: shown when capture is ScreenCaptureKit and Screen Recording is not granted; disabled while busy
- [ ] Banner text "System audio — capturing the other side of a call needs Screen Recording permission."

## Added by task 4.2 (new, ticked)

- [x] ⚙ button in the page header ("Meeting settings"), `aria-expanded`, `aria-haspopup="dialog"`
- [x] Slide-over dialog "Meeting settings" (role dialog, aria-modal, labelled heading)
- [x] Close button in the slide-over
- [x] Closes on ⚙, Close, Escape and a press outside the panel; focus returns to ⚙
- [x] Tab stays inside the open panel
- [x] Unsaved local edits survive closing (state stays in the page)

## Settings (moved by 4.2 into the ⚙ slide-over, ticked)

- [x] Behavior heading and its grouped rows
- [x] Shortcut row: key recorder, "Re-record" button, "Saved ✓", registration problem hint (HotkeyRow, `toggle_meeting`)
- [x] "Record system audio too" toggle, with the current capability hint
- [x] "Notes while the meeting runs" toggle, with the cost hint (cadence and AI calls per hour)
- [x] "Name meetings from your calendar" toggle, with the privacy hint
- [x] "Grant access" button for calendar (shown when the toggle is on and access is not granted)
- [x] Calendar access requested when the toggle goes on (behaviour)
- [x] "Separate speakers by voice" toggle, with the voice hint
- [x] "Download speaker models" button (shown when enabling is pending and models are missing; "Downloading…" while running)
- [x] Voice pending status line ("Turning this on needs two speaker models…", role status)
- [x] Voice status or error line (role alert)
- [x] Voice enable stays pending until both models land, then turns on (behaviour)
- [x] "Transcribe every" number field (5–120 seconds): local edit on change, clamped and saved on blur
- [x] "Saved ✓" confirmation next to each setting (`savedKey`)
- [x] Settings error line (`meetingSettings.error`), and reverting a refused value (behaviour)
- [x] AI card "Meetings AI" (FeatureAiCard): per-slot "Change…" or "Choose…", "Use default again", "Not set", "Saved ✓"
- [x] AI card "Advanced" disclosure listing engine, model, provider and binding per slot
- [x] Defaults picker opened from the AI card (`ai.openPicker`)
- [x] Notion card (NotionSettings): "Integration secret" field (`Notion integration secret`), "Save", "Forget", "Saved ✓", error line
- [x] Notion card: "Page to export into" field (`Notion page link`), saved on blur or Enter, "Saved ✓"

## Try it section (stays until task 4.6 removes it)

- [ ] "Try it" heading
- [ ] State readout: Idle, Recording or Processing
- [ ] Level meter (LevelMeter), shown while recording
- [ ] "Run a 10-second test" button (disabled while busy, recording, processing or testing)
- [ ] Test capture auto-stops after 10 seconds (behaviour)
- [ ] "Start meeting" button (same disabling)
- [ ] "Stop meeting" button (disabled unless recording)
- [ ] Note "The test is a real capture — it is saved to your meetings like any other."
- [ ] Status line (`meetingStatus`): recording, saved, error and permission messages

## Live state (stays; moves to MeetingPane in task 4.6)

- [ ] "Notes so far" card, shown when interim notes exist: "Updated every few minutes…" line, summary, action items
- [ ] Notes so far cleared when a new meeting starts recording (behaviour)
- [ ] "Live transcript" card: `TranscriptPanel` with the live flag
- [ ] Live transcript empty message: "Listening — segments appear every few seconds…" or "Start a meeting to see live transcription."
- [ ] Live segments labelled You, Others or left unlabelled (`speakerDisplayName`)
- [ ] Live segments de-duplicated by meeting id and sequence (behaviour)
- [ ] Segments cleared on recording start and on Start meeting (behaviour)
- [ ] Recording state restored on mount: `getMeetingState` and the active meeting's segments (behaviour)
- [ ] Meeting error event: cancels the test stop and shows the error text (behaviour)
- [ ] Test stop skipped when capture is no longer recording (behaviour)
- [ ] Voice model download progress drives the pending voice enable (behaviour)

## Past meetings list (stays; moves to MeetingList in task 4.3)

- [ ] "Past meetings" heading
- [ ] "No meetings yet." empty text
- [ ] Meeting row button per meeting: title, and "started_at · status"
- [ ] Selected row highlight (border and background)
- [ ] Newest 50 meetings loaded (`listMeetings(50)`)
- [ ] List refreshed after a stop (behaviour)
- [ ] Selecting a meeting loads its detail (behaviour)
- [ ] After a stop, the stopped meeting becomes the selection (behaviour)

## Detail area and page status (stays; moves to MeetingPane in task 4.5)

- [ ] "Select a meeting to view notes and transcript." empty state
- [ ] "Loading meeting…" block while the detail loads
- [ ] Page status line (`listStatus`): errors, "Copied the meeting to the clipboard as Markdown.", "Saved to …", "Sent to Notion — …"
- [ ] Delete confirmation ("Delete this meeting and its transcript?") and the selection reset after delete (behaviour)

## Meeting detail actions (wired from the page into MeetingDetail)

- [ ] Rename the meeting title (`onRenameTitle`)
- [ ] Rename a speaker (`onRenameSpeaker`), then re-read the detail
- [ ] Tick or untick an action item (`onSetActionItemStatus`)
- [ ] Add an action item (`onAddActionItem`)
- [ ] "Copy Markdown" button (`onCopyMarkdown`)
- [ ] "Save as Markdown" button (`onSaveMarkdown`), which reveals the file
- [ ] "Send to Notion" button (`onExportNotion`), shown only once Notion is set up
- [ ] "Delete meeting" button (danger), with the confirmation above

## MeetingDetail (`ui/src/components/MeetingDetail.tsx`)

- [ ] "Meeting title" input: renames on blur or Enter; an empty or unchanged title is ignored
- [ ] "from Calendar" chip, shown only when the title came from the calendar
- [ ] Header meta line: started, ended, status and capture mode ("Mic + system" or "Mic only")
- [ ] Meeting error line (`meeting.error`)
- [ ] "Notes" card, shown when notes or action items exist
- [ ] Notes section "Summary" (read-only text; hidden when empty)
- [ ] Notes section "Decisions" (read-only; hidden when empty)
- [ ] "Action items" checklist: a checkbox per item (labelled with the item text), with owner and due hint
- [ ] Action items prose fallback for meetings recorded before the rows existed
- [ ] "Nobody agreed to do anything." empty state
- [ ] "Add what the notes missed" input and "Add item" button (also submits on Enter)
- [ ] Notes section "Follow-ups" (read-only; hidden when empty)
- [ ] Notes section "Open questions" (read-only; hidden when empty)
- [ ] "Speakers" legend: side label ("Your mic" or "System audio"), "inferred" chip, "Confirm" button, rename field per speaker
- [ ] Speakers legend footnote on how speakers are told apart
- [ ] "Transcript" section: speaker-labelled segments, or "No segments recorded for this meeting."
- [ ] Export group: "Copy Markdown", "Save as Markdown", "Send to Notion" (each shown only when wired)
- [ ] "Delete meeting" button, kept apart from export on purpose
