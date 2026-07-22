import type { MeetingDetail as MeetingDetailData } from "../api";
import TranscriptPanel from "./TranscriptPanel";

type Props = {
  detail: MeetingDetailData;
  onDelete: (id: string) => void;
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

export default function MeetingDetail({ detail, onDelete, busy = false }: Props) {
  const { meeting, segments, notes } = detail;

  return (
    <div>
      <header style={{ marginBottom: 16 }}>
        <h3 style={{ margin: "0 0 4px" }}>{meeting.title}</h3>
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
          <h4 style={{ margin: "0 0 12px" }}>Notes</h4>
          <NotesSection label="Summary" content={notes.summary} />
          <NotesSection label="Decisions" content={notes.decisions} />
          <NotesSection label="Action items" content={notes.action_items} />
          <NotesSection label="Follow-ups" content={notes.follow_ups} />
          <NotesSection label="Open questions" content={notes.open_questions} />
        </section>
      )}

      <section style={{ marginBottom: 16 }}>
        <h4 style={{ margin: "0 0 8px" }}>Transcript</h4>
        <TranscriptPanel
          segments={segments}
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
