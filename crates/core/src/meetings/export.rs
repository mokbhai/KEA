//! Meeting → Markdown, as a function from data to a string.
//!
//! No I/O, no `AppHandle`, no filesystem: the app layer decides where the
//! bytes go (a file, the clipboard), and the whole export surface stays
//! testable with a snapshot assertion.

use crate::store::meetings::{ActionItem, ActionItemStatus, MeetingDetail, MeetingSpeaker};

use super::synthesis::{format_transcript_for_synthesis, MEETING_INTERIM_PROMPT_VERSION};

/// Render one meeting as a Markdown document.
///
/// `speakers` and `items` are passed rather than read off `detail` so a caller
/// can export a filtered view (open items only, say) without this function
/// growing options, and so the function is total over its inputs.
pub fn meeting_to_markdown(
    detail: &MeetingDetail,
    speakers: &[MeetingSpeaker],
    items: &[ActionItem],
) -> String {
    let meeting = &detail.meeting;
    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", meeting.title.trim()));
    out.push_str(&format!("- **Started:** {}\n", meeting.started_at));
    if let Some(ended) = meeting.ended_at.as_deref() {
        out.push_str(&format!("- **Ended:** {ended}\n"));
    }
    out.push_str(&format!("- **Status:** {}\n", meeting.status.as_str()));
    if !speakers.is_empty() {
        let names: Vec<&str> = speakers.iter().map(|s| s.display_name.as_str()).collect();
        out.push_str(&format!("- **Speakers:** {}\n", names.join(", ")));
    }

    if let Some(notes) = detail.notes.as_ref() {
        // A document exported mid-meeting says so. The interim notes are a
        // fold over passes, not a read of the whole transcript, and a reader
        // who pastes this into a wiki should not have to guess that.
        if notes.prompt_version == MEETING_INTERIM_PROMPT_VERSION {
            out.push_str(
                "- **Notes:** interim — this meeting had not finished when it was exported\n",
            );
        }
        out.push('\n');
        push_section(&mut out, "Summary", &notes.summary);
        push_section(&mut out, "Decisions", &notes.decisions);
    } else {
        out.push('\n');
    }

    push_action_items(
        &mut out,
        items,
        detail.notes.as_ref().map(|n| n.action_items.as_str()),
    );

    if let Some(notes) = detail.notes.as_ref() {
        push_section(&mut out, "Follow-ups", &notes.follow_ups);
        push_section(&mut out, "Open questions", &notes.open_questions);
    }

    out.push_str("## Transcript\n\n");
    let transcript = format_transcript_for_synthesis(&detail.segments, speakers);
    if transcript.is_empty() {
        // An empty heading reads as a broken export. Say what happened.
        out.push_str("_No transcript was recorded._\n");
    } else {
        for line in transcript.lines() {
            out.push_str(line);
            out.push_str("\n\n");
        }
    }

    out
}

fn push_section(out: &mut String, heading: &str, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    out.push_str(&format!("## {heading}\n\n{body}\n\n"));
}

/// The checklist, or the prose column for a meeting recorded before the table
/// existed, or nothing at all.
fn push_action_items(out: &mut String, items: &[ActionItem], prose: Option<&str>) {
    if items.is_empty() {
        if let Some(prose) = prose.map(str::trim).filter(|p| !p.is_empty()) {
            out.push_str(&format!("## Action items\n\n{prose}\n\n"));
        }
        return;
    }

    out.push_str("## Action items\n\n");
    for item in items {
        // GitHub-flavoured task list, which is also what Notion parses on
        // paste — the reason 16c can defer the Notion API entirely.
        let box_mark = match item.status {
            ActionItemStatus::Open => "[ ]",
            ActionItemStatus::Done => "[x]",
            // Dropped items are kept in the document but struck through: the
            // fact that somebody decided *not* to do a thing is part of the
            // record, and silently omitting it would misreport the meeting.
            ActionItemStatus::Dropped => "[ ]",
        };
        let mut text = item.text.trim().to_string();
        if item.status == ActionItemStatus::Dropped {
            text = format!("~~{text}~~");
        }
        if let Some(owner) = item
            .owner
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
        {
            text = format!("{text} — {owner}");
        }
        if let Some(due) = item
            .due_hint
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            text = format!("{text} ({due})");
        }
        out.push_str(&format!("- {box_mark} {text}\n"));
    }
    out.push('\n');
}

/// A file name for the exported document: `<slug>-<yyyy-mm-dd>.md`.
///
/// The date comes from `started_at`, which is stored as
/// `YYYY-MM-DD HH:MM:SS`, so the first ten characters are the date with no
/// date library and no timezone arithmetic. A row whose `started_at` is
/// unexpectedly short falls back to the slug alone rather than slicing out of
/// range.
pub fn markdown_file_name(title: &str, started_at: &str) -> String {
    let slug = slugify(title);
    let date: String = started_at.chars().take(10).collect();
    // A date is only a date if it looks like one; anything else is noise in a
    // file name.
    let dated = date.len() == 10 && date.chars().all(|c| c.is_ascii_digit() || c == '-');
    if dated {
        format!("{slug}-{date}.md")
    } else {
        format!("{slug}.md")
    }
}

/// Lowercase ASCII words joined by hyphens, capped so the name stays a name.
///
/// Non-ASCII is dropped rather than transliterated: a meeting titled entirely
/// in CJK would otherwise produce an empty slug, which is why the fallback
/// exists.
fn slugify(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "meeting".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::meetings::{
        CaptureMode, Meeting, MeetingNotes, MeetingSegment, MeetingStatus, SpeakerSource,
        TitleSource,
    };

    fn meeting() -> Meeting {
        Meeting {
            id: "m1".into(),
            title: "Q3 Roadmap Review".into(),
            started_at: "2026-09-19 10:00:00".into(),
            ended_at: Some("2026-09-19 10:30:00".into()),
            status: MeetingStatus::Completed,
            capture_mode: CaptureMode::MicAndSystem,
            stt_engine_id: Some("whisper".into()),
            llm_engine_id: Some("openai".into()),
            error: None,
            title_source: TitleSource::Calendar,
        }
    }

    fn detail(notes: Option<MeetingNotes>, segments: Vec<MeetingSegment>) -> MeetingDetail {
        MeetingDetail {
            meeting: meeting(),
            segments,
            notes,
            speakers: vec![],
            action_items: vec![],
        }
    }

    fn segment(sequence: i32, speaker: Option<&str>, text: &str) -> MeetingSegment {
        MeetingSegment {
            id: sequence as i64 + 1,
            meeting_id: "m1".into(),
            sequence,
            start_offset_ms: sequence as i64 * 5_000,
            end_offset_ms: sequence as i64 * 5_000 + 5_000,
            text: text.into(),
            speaker_key: speaker.map(str::to_string),
        }
    }

    fn notes(prompt_version: &str) -> MeetingNotes {
        MeetingNotes {
            meeting_id: "m1".into(),
            summary: "The launch slips a week.".into(),
            decisions: "Launch moves to the 14th.".into(),
            action_items: "Priya sends the deck".into(),
            follow_ups: "Confirm the venue".into(),
            open_questions: "".into(),
            prompt_version: prompt_version.into(),
            engine_id: Some("openai".into()),
            model: Some("gpt-4o-mini".into()),
        }
    }

    fn item(id: i64, text: &str, status: ActionItemStatus) -> ActionItem {
        ActionItem {
            id,
            meeting_id: "m1".into(),
            text: text.into(),
            owner: None,
            due_hint: None,
            source_seq: None,
            status,
        }
    }

    fn speaker(key: &str, name: &str) -> MeetingSpeaker {
        MeetingSpeaker {
            meeting_id: "m1".into(),
            speaker_key: key.into(),
            display_name: name.into(),
            source: SpeakerSource::Channel,
        }
    }

    #[test]
    fn renders_a_whole_meeting() {
        let speakers = vec![speaker("local", "You"), speaker("remote", "Priya")];
        let items = vec![
            ActionItem {
                owner: Some("Priya".into()),
                due_hint: Some("by Friday".into()),
                ..item(1, "Send the deck", ActionItemStatus::Open)
            },
            item(2, "Book the room", ActionItemStatus::Done),
            item(3, "Rewrite the pricing page", ActionItemStatus::Dropped),
        ];
        let detail = detail(
            Some(notes("meeting-notes-v3")),
            vec![
                segment(0, Some("local"), "shall we start"),
                segment(1, Some("remote"), "yes, go ahead"),
            ],
        );

        assert_eq!(
            meeting_to_markdown(&detail, &speakers, &items),
            "# Q3 Roadmap Review\n\
             \n\
             - **Started:** 2026-09-19 10:00:00\n\
             - **Ended:** 2026-09-19 10:30:00\n\
             - **Status:** completed\n\
             - **Speakers:** You, Priya\n\
             \n\
             ## Summary\n\
             \n\
             The launch slips a week.\n\
             \n\
             ## Decisions\n\
             \n\
             Launch moves to the 14th.\n\
             \n\
             ## Action items\n\
             \n\
             - [ ] Send the deck — Priya (by Friday)\n\
             - [x] Book the room\n\
             - [ ] ~~Rewrite the pricing page~~\n\
             \n\
             ## Follow-ups\n\
             \n\
             Confirm the venue\n\
             \n\
             ## Transcript\n\
             \n\
             [00:00-00:05] You: shall we start\n\
             \n\
             [00:05-00:10] Priya: yes, go ahead\n\
             \n"
        );
    }

    /// The case that produces a document full of empty headings if nobody
    /// checks: no notes, no action items, no speakers, no segments.
    #[test]
    fn an_empty_meeting_is_still_a_valid_document() {
        let out = meeting_to_markdown(&detail(None, vec![]), &[], &[]);
        assert_eq!(
            out,
            "# Q3 Roadmap Review\n\
             \n\
             - **Started:** 2026-09-19 10:00:00\n\
             - **Ended:** 2026-09-19 10:30:00\n\
             - **Status:** completed\n\
             \n\
             ## Transcript\n\
             \n\
             _No transcript was recorded._\n"
        );
        assert!(!out.contains("## Summary"));
        assert!(!out.contains("## Action items"));
    }

    /// A meeting recorded before the action-items table still exports its
    /// action items — from the prose column the rows are derived from.
    #[test]
    fn a_meeting_without_rows_falls_back_to_the_prose_column() {
        let out = meeting_to_markdown(&detail(Some(notes("meeting-notes-v2")), vec![]), &[], &[]);
        assert!(out.contains("## Action items\n\nPriya sends the deck\n"));
    }

    /// Exporting during a meeting must not hand someone a document that looks
    /// like the finished minutes.
    #[test]
    fn interim_notes_are_labelled_as_unfinished() {
        let out = meeting_to_markdown(
            &detail(Some(notes(MEETING_INTERIM_PROMPT_VERSION)), vec![]),
            &[],
            &[],
        );
        assert!(out.contains("interim — this meeting had not finished"));
    }

    #[test]
    fn file_names_are_slug_plus_date() {
        assert_eq!(
            markdown_file_name("Q3 Roadmap Review", "2026-09-19 10:00:00"),
            "q3-roadmap-review-2026-09-19.md"
        );
        // A title that slugifies to nothing still produces a usable name.
        assert_eq!(
            markdown_file_name("週次ミーティング", "2026-09-19 10:00:00"),
            "meeting-2026-09-19.md"
        );
        // Unexpected `started_at` shape: a name, not a panic or a nonsense date.
        assert_eq!(markdown_file_name("Standup", "soon"), "standup.md");
        // Slashes and dots cannot escape the directory they are written into.
        assert_eq!(
            markdown_file_name("../../etc/passwd", "2026-09-19 10:00:00"),
            "etc-passwd-2026-09-19.md"
        );
    }
}
