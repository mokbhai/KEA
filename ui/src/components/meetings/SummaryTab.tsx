import { useState } from "react";
import type {
  ActionItemStatus,
  MeetingActionItem,
  MeetingDetail as MeetingDetailData,
  MeetingNotes,
} from "../../api";
import type { MeetingSpeakerRow } from "../TranscriptPanel";

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

export type SummaryTabProps = {
  /** The finished meeting. Null while the meeting is recording or loading. */
  detail: MeetingDetailData | null;
  /** This meeting is recording: the tab shows the live notes instead. */
  live: boolean;
  liveNotes: MeetingNotes | null;
  /** The meeting has stopped and its final notes are still being written. */
  generating: boolean;
  busy?: boolean;
  /** Persist a new name for one side of this meeting. */
  onRenameSpeaker?: (speakerKey: string, displayName: string) => void | Promise<void>;
  /** Tick an action item off, or put it back. */
  onSetActionItemStatus?: (id: number, status: ActionItemStatus) => void | Promise<void>;
  /** Add what the model missed. */
  onAddActionItem?: (text: string) => void | Promise<void>;
  /** Put the meeting on the clipboard as Markdown. */
  onCopyMarkdown?: () => void | Promise<void>;
  /** Write the meeting to a file and reveal it. */
  onSaveMarkdown?: () => void | Promise<void>;
  /**
   * Send the meeting to Notion as a new page. Omitted when Notion is not set
   * up, so the button never appears as something that will just fail.
   */
  onExportNotion?: () => void | Promise<void>;
  /** Delete the meeting. Omitted while it records, which cannot be deleted. */
  onDelete?: (id: string) => void;
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

/**
 * Action items as a checklist.
 *
 * Falls back to the prose the model wrote for a meeting recorded before the
 * rows existed — the column is still populated on every write, so both
 * renderings stay correct forever.
 */
function ActionItems({
  items,
  prose,
  onSetStatus,
  onAdd,
  busy,
}: {
  items: MeetingActionItem[];
  prose: string;
  onSetStatus?: SummaryTabProps["onSetActionItemStatus"];
  onAdd?: SummaryTabProps["onAddActionItem"];
  busy: boolean;
}) {
  const [draft, setDraft] = useState("");

  if (items.length === 0) {
    // No rows and no prose: the meeting genuinely had no action items, and an
    // empty heading reads as something broken.
    if (!prose.trim() && !onAdd) return null;
    return (
      <section style={{ marginBottom: 16 }}>
        <h4 style={{ margin: "0 0 6px", fontSize: 14 }}>Action items</h4>
        {prose.trim() ? (
          <p style={{ margin: 0, whiteSpace: "pre-wrap", color: "var(--text)" }}>{prose}</p>
        ) : (
          <p className="kea-muted" style={{ margin: 0 }}>
            Nobody agreed to do anything.
          </p>
        )}
        {onAdd && <AddItemRow draft={draft} setDraft={setDraft} onAdd={onAdd} busy={busy} />}
      </section>
    );
  }

  return (
    <section style={{ marginBottom: 16 }}>
      <h4 style={{ margin: "0 0 6px", fontSize: 14 }}>Action items</h4>
      <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
        {items.map((item) => {
          const done = item.status === "done";
          const dropped = item.status === "dropped";
          return (
            <li
              key={item.id}
              style={{ display: "flex", alignItems: "baseline", gap: 8, marginBottom: 4 }}
            >
              <input
                type="checkbox"
                aria-label={item.text}
                checked={done}
                disabled={busy || !onSetStatus}
                onChange={(e) =>
                  void onSetStatus?.(item.id, e.target.checked ? "done" : "open")
                }
              />
              <span
                style={{
                  color: done || dropped ? "var(--text-muted)" : "var(--text)",
                  textDecoration: done || dropped ? "line-through" : undefined,
                }}
              >
                {item.text}
              </span>
              {item.owner && (
                <span className="kea-muted" style={{ fontSize: 12 }}>
                  — {item.owner}
                </span>
              )}
              {item.due_hint && (
                <span className="kea-muted" style={{ fontSize: 12 }}>
                  ({item.due_hint})
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {onAdd && <AddItemRow draft={draft} setDraft={setDraft} onAdd={onAdd} busy={busy} />}
    </section>
  );
}

function AddItemRow({
  draft,
  setDraft,
  onAdd,
  busy,
}: {
  draft: string;
  setDraft: (value: string) => void;
  onAdd: NonNullable<SummaryTabProps["onAddActionItem"]>;
  busy: boolean;
}) {
  const submit = () => {
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    void onAdd(text);
  };

  return (
    <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
      <input
        className="kea-input"
        aria-label="New action item"
        placeholder="Add what the notes missed"
        value={draft}
        disabled={busy}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
        }}
        style={{ flex: 1, minWidth: 0 }}
      />
      <button
        type="button"
        className="kea-btn"
        onClick={submit}
        disabled={busy || !draft.trim()}
      >
        Add item
      </button>
    </div>
  );
}

function SpeakerLegend({
  speakers,
  onRename,
  busy,
}: {
  speakers: MeetingSpeakerRow[];
  onRename?: SummaryTabProps["onRenameSpeaker"];
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
              {speaker.source === "inferred" && (
                <>
                  <span
                    className="kea-muted"
                    title="Proposed from something said in the meeting. Confirm it or rename it."
                    style={{
                      padding: "2px 8px",
                      borderRadius: 999,
                      border: "1px solid var(--border)",
                      background: "var(--surface-2)",
                      fontSize: 11,
                      whiteSpace: "nowrap",
                    }}
                  >
                    inferred
                  </span>
                  {onRename && (
                    // Confirming saves the name as typed: the same write as a
                    // rename, which is what makes it a user name the transcript
                    // will never overwrite.
                    <button
                      type="button"
                      className="kea-btn"
                      disabled={busy}
                      onClick={() => void onRename(speaker.speaker_key, speaker.display_name)}
                    >
                      Confirm
                    </button>
                  )}
                </>
              )}
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

/** The live notes card: interim passes, labelled as unfinished. */
function LiveNotes({ notes }: { notes: MeetingNotes | null }) {
  if (!notes) {
    return (
      <p className="kea-muted" style={{ margin: 0 }}>
        Notes appear here as the meeting runs.
      </p>
    );
  }
  return (
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <h2 style={{ margin: "0 0 8px", fontSize: 15 }}>Notes so far</h2>
      {/*
        Said plainly, because a stale summary read as a final one is the
        way this feature misleads: the fold can compress out a point
        mentioned once, and the full-transcript pass at stop is what
        fixes it.
      */}
      <p className="kea-muted" style={{ margin: "0 0 8px", fontSize: "0.8125rem" }}>
        Updated every few minutes while the meeting runs. The full notes are
        written when you stop.
      </p>
      {notes.summary.trim() && (
        <p style={{ margin: "0 0 8px", whiteSpace: "pre-wrap" }}>{notes.summary}</p>
      )}
      {notes.action_items.trim() && (
        <>
          <h3 style={{ margin: "0 0 4px", fontSize: 14 }}>Action items</h3>
          <p style={{ margin: 0, whiteSpace: "pre-wrap" }}>{notes.action_items}</p>
        </>
      )}
    </section>
  );
}

/**
 * The Summary tab: the notes, action items and speakers of a finished meeting,
 * or the live notes of one that is recording.
 *
 * Export and delete sit at the foot of the tab. Export is apart from delete on
 * purpose: the two are adjacent in intent ("I am done with this meeting") and
 * opposite in consequence, and a mis-click must not be the destructive one.
 */
export default function SummaryTab({
  detail,
  live,
  liveNotes,
  generating,
  busy = false,
  onRenameSpeaker,
  onSetActionItemStatus,
  onAddActionItem,
  onCopyMarkdown,
  onSaveMarkdown,
  onExportNotion,
  onDelete,
}: SummaryTabProps) {
  if (live) return <LiveNotes notes={liveNotes} />;
  if (!detail) {
    // Stopped, and the stored meeting has not been read back yet.
    return generating ? (
      <p role="status" className="kea-muted" style={{ margin: 0 }}>
        Generating notes…
      </p>
    ) : null;
  }

  const { meeting, notes, speakers, action_items: actionItems } = detail;
  const canExport = Boolean(onCopyMarkdown || onSaveMarkdown || onExportNotion);
  const hasNotes = Boolean(notes) || actionItems.length > 0;

  return (
    <div>
      {generating && (
        <p role="status" className="kea-muted" style={{ margin: "0 0 12px" }}>
          Generating notes…
        </p>
      )}

      {hasNotes ? (
        <section className="kea-card" style={{ marginBottom: 16 }}>
          <h3 style={{ margin: "0 0 12px", display: "flex", alignItems: "center", gap: 8 }}>
            Notes
            {notes?.edited && (
              <span
                className="kea-muted"
                title="You changed these notes after they were generated."
                style={{
                  padding: "2px 8px",
                  borderRadius: 999,
                  border: "1px solid var(--border)",
                  background: "var(--surface-2)",
                  fontSize: 11,
                  fontWeight: 400,
                  whiteSpace: "nowrap",
                }}
              >
                edited
              </span>
            )}
          </h3>
          <NotesSection label="Summary" content={notes?.summary ?? ""} />
          <NotesSection label="Decisions" content={notes?.decisions ?? ""} />
          <ActionItems
            items={actionItems}
            prose={notes?.action_items ?? ""}
            onSetStatus={onSetActionItemStatus}
            onAdd={onAddActionItem}
            busy={busy}
          />
          <NotesSection label="Follow-ups" content={notes?.follow_ups ?? ""} />
          <NotesSection label="Open questions" content={notes?.open_questions ?? ""} />
        </section>
      ) : (
        !generating && (
          <p className="kea-muted" style={{ margin: "0 0 16px" }}>
            No notes were written for this meeting.
          </p>
        )
      )}

      <SpeakerLegend speakers={speakers} onRename={onRenameSpeaker} busy={busy} />

      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        {canExport && (
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            {onCopyMarkdown && (
              <button
                type="button"
                className="kea-btn"
                onClick={() => void onCopyMarkdown()}
                disabled={busy}
              >
                Copy Markdown
              </button>
            )}
            {onSaveMarkdown && (
              <button
                type="button"
                className="kea-btn"
                onClick={() => void onSaveMarkdown()}
                disabled={busy}
              >
                Save as Markdown
              </button>
            )}
            {onExportNotion && (
              <button
                type="button"
                className="kea-btn"
                onClick={() => void onExportNotion()}
                disabled={busy}
              >
                Send to Notion
              </button>
            )}
          </div>
        )}
        <div style={{ flex: 1 }} />
        {onDelete && (
          <button
            type="button"
            className="kea-btn"
            onClick={() => onDelete(meeting.id)}
            disabled={busy}
            style={{ color: "var(--danger)", borderColor: "var(--danger)" }}
          >
            Delete meeting
          </button>
        )}
      </div>
    </div>
  );
}
