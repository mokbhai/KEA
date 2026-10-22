# Spec Delta

## Purpose

Lays out the Meetings page as a list of meetings beside the selected meeting, so the
notes of any meeting, including the one being recorded, are one click away and
settings stay out of the way until needed.

## ADDED Requirements

### Requirement: The page shows a meeting list beside the selected meeting
The Meetings page SHALL show a list of meetings next to a detail pane for the selected
meeting, within the existing Meetings sidebar entry. It MUST NOT add a sidebar entry.
The header SHALL hold the page title, the Record/Stop control and a settings (⚙)
button.

#### Scenario: Opening the page
- **WHEN** the user opens Meetings and at least one meeting exists
- **THEN** the list and the detail pane are both visible without scrolling, at the app's default window size

#### Scenario: No meetings yet
- **WHEN** the user opens Meetings and no meeting exists
- **THEN** the detail pane shows an empty state with the Record control and a short explanation, instead of an empty list and a blank pane

### Requirement: A sensible meeting is selected on open
When the page opens, the system SHALL select the meeting that is currently recording,
or if none is recording, the most recently started meeting. A meeting the user selects
SHALL stay selected while they move around the page, and SHALL be restored when they
return to the page in the same app session.

#### Scenario: Recording in progress
- **WHEN** the user opens Meetings while a meeting is recording
- **THEN** that meeting is selected and its live view is shown

#### Scenario: Nothing recording
- **WHEN** the user opens Meetings with no recording and three past meetings
- **THEN** the most recent meeting is selected with its Summary tab showing

#### Scenario: Coming back
- **WHEN** the user selects an older meeting, visits another page, and returns to Meetings
- **THEN** the older meeting is still selected

### Requirement: The list is grouped and searchable
The list SHALL group meetings under Today, Yesterday, This week and Earlier, by start
time in the user's local time zone, newest first within each group. A meeting that is
recording SHALL be pinned above the groups with a recording indicator. A search field
SHALL filter the list to meetings whose title or notes contain the query,
case-insensitively. Each list row SHALL show the title, start time and duration, and
mark a meeting whose notes failed.

#### Scenario: Search by content
- **WHEN** the user types "confluence" and a meeting's summary mentions Confluence
- **THEN** that meeting is in the filtered list even though its title does not contain the word

#### Scenario: Search finds nothing
- **WHEN** no meeting matches the query
- **THEN** the list says no meeting matches and offers to clear the search

#### Scenario: Recording pinned
- **WHEN** a meeting is recording
- **THEN** it appears above the date groups with a recording indicator and its elapsed time

### Requirement: The selected meeting has Summary, Transcript and Ask AI tabs
The detail pane SHALL show the meeting's title (renamable, as today) and three tabs:
**Summary** (notes sections, action items and speakers), **Transcript** (the timed,
speaker-labelled transcript), and **Ask AI** (see `meetings/meeting-chat`). Summary
SHALL be the default tab. Every action available on the meeting detail today SHALL
remain available from these tabs: rename, speaker rename and confirm, action-item
checkboxes and adding items, copy, export and delete.

#### Scenario: Switching tabs
- **WHEN** the user switches from Summary to Transcript and back
- **THEN** each tab keeps its scroll position, and the meeting stays selected

#### Scenario: Nothing lost
- **WHEN** the user looks for the export, delete, or speaker-rename actions
- **THEN** each is reachable from the selected meeting without leaving the page

### Requirement: A recording meeting shows its live view in the detail pane
While a meeting records, its detail pane SHALL show the live notes (when interim notes
are on) and the live transcript in the Summary and Transcript tabs, updating as
segments arrive. The separate "Try it" section MUST be removed. Stopping SHALL turn the
same pane into the finished meeting without the user re-selecting it.

#### Scenario: Live transcript
- **WHEN** a segment is transcribed during a recording
- **THEN** it appears at the bottom of the Transcript tab, which follows new lines unless the user has scrolled up

#### Scenario: Stopping
- **WHEN** the user stops the recording
- **THEN** the pane shows "Generating notes…" in the Summary tab and then the final notes, with the meeting still selected

### Requirement: Settings live in a slide-over
All meeting settings currently on the page SHALL move into a panel that slides over the
page from the right when ⚙ is pressed, and closes with ⚙, a close button, Escape, or a
click outside it. Every setting SHALL keep its current behaviour, saved confirmation
and hints. The panel MUST NOT discard an unsaved setting when it closes.

#### Scenario: Changing a setting
- **WHEN** the user opens ⚙, turns on interim notes, and closes the panel
- **THEN** the setting is saved exactly as it is today, and the list and detail are visible again

#### Scenario: Keyboard
- **WHEN** the panel is open and the user presses Escape
- **THEN** the panel closes and focus returns to the ⚙ button

### Requirement: The layout works in a narrow window
When the window is too narrow for both panes, the page SHALL show one pane at a time:
the list, then the selected meeting with a Back control returning to the list.

#### Scenario: Narrow window
- **WHEN** the window is narrower than the two-pane minimum and the user picks a meeting
- **THEN** the detail fills the page with a Back control, and Back returns to the list with the meeting still highlighted
