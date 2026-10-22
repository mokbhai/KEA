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

- [x] Page heading "Meetings" and the intro line (task 4.4 keeps the header; Record/Stop moves here in 4.6)
- [x] FeatureBanner: one warning per unset or blocked slot (speech to text, notes writing), with its action button ("Open Models" or "Choose…")
- [x] Banner "Grant permission" for system audio: shown when capture is ScreenCaptureKit and Screen Recording is not granted; disabled while busy
- [x] Banner text "System audio — capturing the other side of a call needs Screen Recording permission."

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

- [x] "Try it" heading
- [x] State readout: Idle, Recording or Processing
- [x] Level meter (LevelMeter), shown while recording
- [x] "Run a 10-second test" button (disabled while busy, recording, processing or testing)
- [x] Test capture auto-stops after 10 seconds (behaviour)
- [x] "Start meeting" button (same disabling)
- [x] "Stop meeting" button (disabled unless recording)
- [x] Note "The test is a real capture — it is saved to your meetings like any other."
- [x] Status line (`meetingStatus`): recording, saved, error and permission messages

## Live state (stays; moves to MeetingPane in task 4.6)

- [x] "Notes so far" card, shown when interim notes exist: "Updated every few minutes…" line, summary, action items
- [x] Notes so far cleared when a new meeting starts recording (behaviour)
- [x] "Live transcript" card: `TranscriptPanel` with the live flag
- [ ] Live transcript empty message: "Listening — segments appear every few seconds…" or "Start a meeting to see live transcription."
- [x] Live segments labelled You, Others or left unlabelled (`speakerDisplayName`)
- [x] Live segments de-duplicated by meeting id and sequence (behaviour)
- [x] Segments cleared on recording start and on Start meeting (behaviour)
- [x] Recording state restored on mount: `getMeetingState` and the active meeting's segments (behaviour)
- [x] Meeting error event: cancels the test stop and shows the error text (behaviour)
- [x] Test stop skipped when capture is no longer recording (behaviour)
- [x] Voice model download progress drives the pending voice enable (behaviour)

## Past meetings list (stays; moves to MeetingList in task 4.3)

- [x] "Past meetings" heading
- [x] "No meetings yet." empty text
- [x] Meeting row button per meeting: title, and "started_at · status"
- [x] Selected row highlight (border and background)
- [x] Newest 50 meetings loaded (`listMeetings(50)`)
- [x] List refreshed after a stop (behaviour)
- [x] Selecting a meeting loads its detail (behaviour)
- [x] After a stop, the stopped meeting becomes the selection (behaviour)

## Detail area and page status (stays; moves to MeetingPane in task 4.5)

- [x] "Select a meeting to view notes and transcript." empty state
- [x] "Loading meeting…" block while the detail loads
- [x] Page status line (`listStatus`): errors, "Copied the meeting to the clipboard as Markdown.", "Saved to …", "Sent to Notion — …"
- [x] Delete confirmation ("Delete this meeting and its transcript?") and the selection reset after delete (behaviour)

## Meeting detail actions (wired from the page into MeetingDetail)

- [x] Rename the meeting title (`onRenameTitle`)
- [x] Rename a speaker (`onRenameSpeaker`), then re-read the detail
- [x] Tick or untick an action item (`onSetActionItemStatus`)
- [x] Add an action item (`onAddActionItem`)
- [x] "Copy Markdown" button (`onCopyMarkdown`)
- [x] "Save as Markdown" button (`onSaveMarkdown`), which reveals the file
- [x] "Send to Notion" button (`onExportNotion`), shown only once Notion is set up
- [x] "Delete meeting" button (danger), with the confirmation above

## MeetingDetail (`ui/src/components/MeetingDetail.tsx`)

- [x] "Meeting title" input: renames on blur or Enter; an empty or unchanged title is ignored
- [x] "from Calendar" chip, shown only when the title came from the calendar
- [x] Header meta line: started, ended, status and capture mode ("Mic + system" or "Mic only")
- [x] Meeting error line (`meeting.error`)
- [x] "Notes" card, shown when notes or action items exist
- [x] Notes section "Summary" (read-only text; hidden when empty)
- [x] Notes section "Decisions" (read-only; hidden when empty)
- [x] "Action items" checklist: a checkbox per item (labelled with the item text), with owner and due hint
- [x] Action items prose fallback for meetings recorded before the rows existed
- [x] "Nobody agreed to do anything." empty state
- [x] "Add what the notes missed" input and "Add item" button (also submits on Enter)
- [x] Notes section "Follow-ups" (read-only; hidden when empty)
- [x] Notes section "Open questions" (read-only; hidden when empty)
- [x] "Speakers" legend: side label ("Your mic" or "System audio"), "inferred" chip, "Confirm" button, rename field per speaker
- [x] Speakers legend footnote on how speakers are told apart
- [x] "Transcript" section: speaker-labelled segments, or "No segments recorded for this meeting."
- [x] Export group: "Copy Markdown", "Save as Markdown", "Send to Notion" (each shown only when wired)
- [x] "Delete meeting" button, kept apart from export on purpose
