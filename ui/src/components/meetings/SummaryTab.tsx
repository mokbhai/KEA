import { useState } from "react";
import {
  editNotesSection,
  undoNotesEdit,
  type ActionItemStatus,
  type MeetingActionItem,
  type MeetingDetail as MeetingDetailData,
  type MeetingNotes,
  type NotesSection as NotesSectionKey,
} from "../../api";
import { toMessage } from "../../lib/format";
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
  /** Whether live (interim) notes are switched on; decides the placeholder. */
  interimNotes?: boolean;
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
  /**
   * An edit, or an undo, returned the fresh detail. Its presence is what offers
   * Edit and Undo on a finished meeting.
   */
  onNotesChanged?: (detail: MeetingDetailData) => void;
};

/** What a prose section needs to offer and run an edit. */
type SectionEdit = {
  section: NotesSectionKey;
  editing: boolean;
  draft: string;
  busy: boolean;
  onStart: (section: NotesSectionKey) => void;
  onDraftChange: (value: string) => void;
  onSave: () => void;
  onCancel: () => void;
};

/**
 * One prose section of the notes. Without `edit` it is the read-only text it
 * always was, hidden when empty; with it, an Edit button is offered and the
 * section stays visible when empty so there is something to edit.
 */
function NotesSection({
  label,
  content,
  edit,
}: {
  label: string;
  content: string;
  edit?: SectionEdit;
}) {
  if (edit?.editing) {
    return (
      <section style={{ marginBottom: 16 }}>
        <h4 style={{ margin: "0 0 6px", fontSize: 14 }}>{label}</h4>
        <textarea
          className="kea-input"
          aria-label={`${label} text`}
          value={edit.draft}
          rows={4}
          disabled={edit.busy}
          onChange={(e) => edit.onDraftChange(e.target.value)}
          style={{ width: "100%", resize: "vertical" }}
        />
        <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={edit.onSave}
            disabled={edit.busy}
          >
            Save
          </button>
          <button type="button" className="kea-btn" onClick={edit.onCancel} disabled={edit.busy}>
            Cancel
          </button>
        </div>
      </section>
    );
  }
  if (!content.trim() && !edit) return null;
  return (
    <section style={{ marginBottom: 16 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, margin: "0 0 6px" }}>
        <h4 style={{ margin: 0, fontSize: 14 }}>{label}</h4>
        {edit && (
          <button
            type="button"
            className="kea-btn"
            aria-label={`Edit ${label.toLowerCase()}`}
            disabled={edit.busy}
            onClick={() => edit.onStart(edit.section)}
            style={{ fontSize: 12, padding: "2px 8px" }}
          >
            Edit
          </button>
        )}
      </div>
      {content.trim() ? (
        <p style={{ margin: 0, whiteSpace: "pre-wrap", color: "var(--text)" }}>{content}</p>
      ) : (
        <p className="kea-muted" style={{ margin: 0, fontSize: 13 }}>
          Nothing written.
        </p>
      )}
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
function LiveNotes({ notes, interimNotes }: { notes: MeetingNotes | null; interimNotes: boolean }) {
  if (!notes) {
    // With live notes off, nothing will arrive until the meeting stops, and
    // the placeholder must not promise otherwise.
    return (
      <p className="kea-muted" style={{ margin: 0 }}>
        {interimNotes
          ? "Notes appear here as the meeting runs."
          : "Notes are written when the meeting ends. Turn on live notes in ⚙ to see them as it runs."}
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
  interimNotes = true,
  generating,
  busy = false,
  onRenameSpeaker,
  onSetActionItemStatus,
  onAddActionItem,
  onCopyMarkdown,
  onSaveMarkdown,
  onExportNotion,
  onDelete,
  onNotesChanged,
}: SummaryTabProps) {
  const [editing, setEditing] = useState<NotesSectionKey | null>(null);
  const [draft, setDraft] = useState("");
  const [editBusy, setEditBusy] = useState(false);
  const [editError, setEditError] = useState<string | null>(null);

  if (live) {
    return (
      <div>
        <LiveNotes notes={liveNotes} interimNotes={interimNotes} />
        {onNotesChanged && (
          <p className="kea-muted" style={{ margin: "0 0 16px", fontSize: 13 }}>
            Editing is available when the meeting ends.
          </p>
        )}
      </div>
    );
  }
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
  // Not while the final notes are still being written: that pass rewrites the
  // notes, the backend refuses changes until it finishes, and an Edit button
  // that can only fail is worse than none.
  const canEdit = Boolean(onNotesChanged) && Boolean(notes) && !generating;
  const editBusyNow = busy || editBusy;

  const startEdit = (section: NotesSectionKey) => {
    setDraft(notes?.[section] ?? "");
    setEditError(null);
    setEditing(section);
  };

  const cancelEdit = () => {
    setEditing(null);
    setEditError(null);
  };

  // A save that changes nothing is not a version: it would fill the undo stack
  // with a no-op, so it just closes the editor.
  const saveEdit = async () => {
    if (!editing) return;
    if (draft === (notes?.[editing] ?? "")) {
      cancelEdit();
      return;
    }
    setEditBusy(true);
    setEditError(null);
    try {
      const next = await editNotesSection(meeting.id, editing, draft);
      setEditing(null);
      onNotesChanged?.(next);
    } catch (e) {
      setEditError(toMessage(e));
    } finally {
      setEditBusy(false);
    }
  };

  const undoEdit = async () => {
    setEditBusy(true);
    setEditError(null);
    try {
      const next = await undoNotesEdit(meeting.id);
      setEditing(null);
      onNotesChanged?.(next);
    } catch (e) {
      setEditError(toMessage(e));
    } finally {
      setEditBusy(false);
    }
  };

  const sectionEdit = (section: NotesSectionKey): SectionEdit | undefined =>
    canEdit
      ? {
          section,
          editing: editing === section,
          draft,
          // One section edits at a time: opening another would throw away the
          // unsaved text in the open one.
          busy: editBusyNow || (editing !== null && editing !== section),
          onStart: startEdit,
          onDraftChange: setDraft,
          onSave: () => void saveEdit(),
          onCancel: cancelEdit,
        }
      : undefined;

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
            {canEdit && detail.can_undo && (
              <button
                type="button"
                className="kea-btn"
                title="Restore the notes as they were before the last change."
                disabled={editBusyNow}
                onClick={() => void undoEdit()}
                style={{ fontSize: 12, padding: "2px 8px", fontWeight: 400 }}
              >
                Undo
              </button>
            )}
          </h3>
          {editError && (
            <p role="alert" style={{ margin: "0 0 12px", fontSize: 13, color: "var(--danger)" }}>
              {editError}
            </p>
          )}
          <NotesSection label="Summary" content={notes?.summary ?? ""} edit={sectionEdit("summary")} />
          <NotesSection
            label="Decisions"
            content={notes?.decisions ?? ""}
            edit={sectionEdit("decisions")}
          />
          <ActionItems
            items={actionItems}
            prose={notes?.action_items ?? ""}
            onSetStatus={onSetActionItemStatus}
            onAdd={onAddActionItem}
            busy={busy}
          />
          <NotesSection
            label="Follow-ups"
            content={notes?.follow_ups ?? ""}
            edit={sectionEdit("follow_ups")}
          />
          <NotesSection
            label="Open questions"
            content={notes?.open_questions ?? ""}
            edit={sectionEdit("open_questions")}
          />
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
