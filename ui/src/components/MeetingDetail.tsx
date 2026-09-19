import { useState } from "react";
import type { MeetingDetail as MeetingDetailData, MeetingSegment } from "../api";
import TranscriptPanel, {
  speakerDisplayName,
  type MeetingSpeakerRow,
} from "./TranscriptPanel";

/**
 * What the backend actually sends today. `api.ts` has yet to declare
 * `speaker_key` on a segment or `speakers` on a detail, so they are widened in
 * here rather than read off an out-of-date shared type.
 */
type AttributedSegment = MeetingSegment & { speaker_key?: string | null };
type DetailWithSpeakers = Omit<MeetingDetailData, "segments"> & {
  segments: AttributedSegment[];
  speakers?: MeetingSpeakerRow[];
};

/**
 * Which physical source a speaker key stands for.
 *
 * Shown beside the editable name so the legend stays readable after a rename:
 * "Priya" on its own says nothing about why KEA thinks Priya said something,
 * but "System audio — Priya" does.
 */
const SIDE_LABELS: Record<string, string> = {
  local: "Your mic",
  remote: "System audio",
};

type Props = {
  detail: MeetingDetailData;
  onDelete: (id: string) => void;
  /**
   * Persist a new name for one side of this meeting. Omitted renders the
   * legend read-only — the names still show, they just cannot be changed.
   */
  onRenameSpeaker?: (speakerKey: string, displayName: string) => void | Promise<void>;
  busy?: boolean;
};

function NotesSection({ label, content }: { label: string; content: string }) {
  if (!content.trim()) return null;
  return (
    <section style={{ marginBottom: 16 }}>
      <h4 style={{ margin: "0 0 6px", fontSize: 14 }}>{label}</h4>
      <p style={{ margin: 0, whiteSpace: "pre-wrap", color: "var(--text)" }}>{content}</p>
    </section>
  );
}

function SpeakerLegend({
  speakers,
  onRename,
  busy,
}: {
  speakers: MeetingSpeakerRow[];
  onRename?: Props["onRenameSpeaker"];
  busy: boolean;
}) {
  // Keyed by speaker so an unsaved edit on one side is not wiped by a
  // re-render caused by saving the other.
  const [drafts, setDrafts] = useState<Record<string, string>>({});

  if (speakers.length === 0) return null;

  const commit = (speaker: MeetingSpeakerRow) => {
    const next = (drafts[speaker.speaker_key] ?? speaker.display_name).trim();
    setDrafts((prev) => {
      const { [speaker.speaker_key]: _dropped, ...rest } = prev;
      return rest;
    });
    // An empty name is a slip, not a request to have no name, and renaming a
    // side to what it is already called is not a write.
    if (!next || next === speaker.display_name) return;
    void onRename?.(speaker.speaker_key, next);
  };

  return (
    <section style={{ marginBottom: 12 }}>
      <h4 style={{ margin: "0 0 6px", fontSize: 14 }}>Speakers</h4>
      <ul
        style={{
          margin: 0,
          padding: 0,
          listStyle: "none",
          display: "flex",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        {speakers.map((speaker) => {
          const side = SIDE_LABELS[speaker.speaker_key] ?? speaker.speaker_key;
          return (
            <li
              key={speaker.speaker_key}
              style={{ display: "flex", alignItems: "center", gap: 6 }}
            >
              <span className="kea-muted" style={{ fontSize: 12 }}>
                {side}
              </span>
              {onRename ? (
                <input
                  className="kea-input"
                  aria-label={`Name for ${side}`}
                  value={drafts[speaker.speaker_key] ?? speaker.display_name}
                  disabled={busy}
                  onChange={(e) =>
                    setDrafts((prev) => ({
                      ...prev,
                      [speaker.speaker_key]: e.target.value,
                    }))
                  }
                  onBlur={() => commit(speaker)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") e.currentTarget.blur();
                  }}
                  style={{ width: 140 }}
                />
              ) : (
                <strong style={{ fontSize: 13 }}>{speaker.display_name}</strong>
              )}
            </li>
          );
        })}
      </ul>
      <p className="kea-muted" style={{ margin: "6px 0 0", fontSize: "0.75rem" }}>
        Speakers are told apart by which device the audio arrived on, which is
        reliable on headphones. On speakers the mic also hears the call, and
        those stretches are left unlabelled rather than guessed.
      </p>
    </section>
  );
}

export default function MeetingDetail({
  detail,
  onDelete,
  onRenameSpeaker,
  busy = false,
}: Props) {
  const { meeting, segments, notes, speakers = [] } = detail as DetailWithSpeakers;

  return (
    <div>
      <header style={{ marginBottom: 16 }}>
        <h2 style={{ margin: "0 0 4px" }}>{meeting.title}</h2>
        <p className="kea-muted" style={{ margin: 0 }}>
          {meeting.started_at}
          {meeting.ended_at ? ` — ${meeting.ended_at}` : ""}
          {" · "}
          {meeting.status}
          {" · "}
          {meeting.capture_mode === "mic_and_system" ? "Mic + system" : "Mic only"}
        </p>
        {meeting.error && (
          <p style={{ margin: "8px 0 0", fontSize: 13, color: "var(--danger)" }}>
            {meeting.error}
          </p>
        )}
      </header>

      {notes && (
        <section className="kea-card" style={{ marginBottom: 16 }}>
          <h3 style={{ margin: "0 0 12px" }}>Notes</h3>
          <NotesSection label="Summary" content={notes.summary} />
          <NotesSection label="Decisions" content={notes.decisions} />
          <NotesSection label="Action items" content={notes.action_items} />
          <NotesSection label="Follow-ups" content={notes.follow_ups} />
          <NotesSection label="Open questions" content={notes.open_questions} />
        </section>
      )}

      <section style={{ marginBottom: 16 }}>
        <h3 style={{ margin: "0 0 8px" }}>Transcript</h3>
        <SpeakerLegend
          speakers={speakers}
          onRename={onRenameSpeaker}
          busy={busy}
        />
        <TranscriptPanel
          segments={segments.map((seg) => ({
            ...seg,
            speaker: speakerDisplayName(seg.speaker_key, speakers),
          }))}
          emptyMessage="No segments recorded for this meeting."
        />
      </section>

      <button
        type="button"
        className="kea-btn"
        onClick={() => onDelete(meeting.id)}
        disabled={busy}
        style={{ color: "var(--danger)", borderColor: "var(--danger)" }}
      >
        Delete meeting
      </button>
    </div>
  );
}
