import { useState } from "react";
import type {
  ActionItemStatus,
  MeetingActionItem,
  MeetingDetail as MeetingDetailData,
  TitleSource,
} from "../api";
import TranscriptPanel, {
  speakerDisplayName,
  type MeetingSpeakerRow,
} from "./TranscriptPanel";

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
  /**
   * Rename the meeting itself. Omitted renders the title as plain text.
   *
   * This matters more than renaming a speaker: a calendar title is sometimes
   * right about the event and wrong about what the recording contains, and the
   * user needs a one-click way to fix it without deleting anything.
   */
  onRenameTitle?: (title: string) => void | Promise<void>;
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

/**
 * The meeting title, with where it came from and a way to fix it.
 *
 * The chip is only shown for a calendar title: "from Calendar" answers the
 * question a surprising title raises, while a chip on every title would be
 * noise.
 */
function MeetingTitle({
  title,
  source,
  onRename,
  busy,
}: {
  title: string;
  source?: TitleSource;
  onRename?: Props["onRenameTitle"];
  busy: boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);

  const commit = () => {
    const next = (draft ?? title).trim();
    setDraft(null);
    // An empty title is a slip mid-edit, and a title unchanged is not a write.
    if (!next || next === title) return;
    void onRename?.(next);
  };

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
      {onRename ? (
        <input
          className="kea-input"
          aria-label="Meeting title"
          value={draft ?? title}
          disabled={busy}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          style={{ fontSize: 20, fontWeight: 600, flex: 1, minWidth: 0 }}
        />
      ) : (
        <h2 style={{ margin: 0 }}>{title}</h2>
      )}
      {source === "calendar" && (
        <span
          className="kea-muted"
          style={{
            padding: "2px 8px",
            borderRadius: 999,
            border: "1px solid var(--border)",
            background: "var(--surface-2)",
            fontSize: 11,
            whiteSpace: "nowrap",
          }}
        >
          from Calendar
        </span>
      )}
    </div>
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
  onSetStatus?: Props["onSetActionItemStatus"];
  onAdd?: Props["onAddActionItem"];
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
  onAdd: NonNullable<Props["onAddActionItem"]>;
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
  onRenameTitle,
  onSetActionItemStatus,
  onAddActionItem,
  onCopyMarkdown,
  onSaveMarkdown,
  onExportNotion,
  busy = false,
}: Props) {
  const { meeting, segments, notes, speakers, action_items: actionItems } = detail;

  const canExport = Boolean(onCopyMarkdown || onSaveMarkdown || onExportNotion);

  return (
    <div>
      <header style={{ marginBottom: 16 }}>
        <MeetingTitle
          title={meeting.title}
          source={meeting.title_source}
          onRename={onRenameTitle}
          busy={busy}
        />
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

      {(notes || actionItems.length > 0) && (
        <section className="kea-card" style={{ marginBottom: 16 }}>
          <h3 style={{ margin: "0 0 12px" }}>Notes</h3>
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

      {/*
        Export sits apart from delete on purpose: the two are adjacent in
        intent ("I am done with this meeting") and opposite in consequence, and
        a mis-click must not be the destructive one.
      */}
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
        {canExport && (
          <div style={{ display: "flex", gap: 8 }}>
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
    </div>
  );
}
