import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import type {
  ActionItemStatus,
  Meeting,
  MeetingDetail as MeetingDetailData,
  MeetingNotes,
  TitleSource,
} from "../../api";
import LoadingBlock from "../LoadingBlock";
import { speakerDisplayName, type TranscriptSegment } from "../TranscriptPanel";
import AskAiTab from "./AskAiTab";
import SummaryTab from "./SummaryTab";
import TranscriptTab, { type TranscriptTabHandle } from "./TranscriptTab";

export type PaneTab = "summary" | "transcript" | "ask";

const TABS: readonly { id: PaneTab; label: string }[] = [
  { id: "summary", label: "Summary" },
  { id: "transcript", label: "Transcript" },
  { id: "ask", label: "Ask AI" },
];

/** What the page can call on the pane from outside it. */
export type MeetingPaneHandle = {
  /** Switch to the Transcript tab and scroll to the line covering `ms`. */
  jumpTo: (ms: number) => void;
};

export type MeetingPaneProps = {
  /** The header's data: the list row, or the live row for the recording meeting. */
  meeting: Meeting;
  /** The finished meeting's detail. Ignored unless it is this meeting's. */
  detail: MeetingDetailData | null;
  /** This meeting is recording: Summary and Transcript read the live state. */
  live: boolean;
  liveSegments: TranscriptSegment[];
  liveNotes: MeetingNotes | null;
  /** Whether live (interim) notes are switched on. */
  interimNotes?: boolean;
  /** The meeting has stopped and its final notes are still being written. */
  generating: boolean;
  /** The detail is being fetched. */
  loading: boolean;
  busy: boolean;
  /** Shown only in the narrow layout, where the list is the other pane. */
  onBack?: () => void;
  /** Omitted while the meeting records, which cannot be deleted. */
  onDelete?: (id: string) => void;
  onRenameTitle?: (title: string) => void | Promise<void>;
  onRenameSpeaker?: (speakerKey: string, displayName: string) => void | Promise<void>;
  onSetActionItemStatus?: (id: number, status: ActionItemStatus) => void | Promise<void>;
  onAddActionItem?: (text: string) => void | Promise<void>;
  onCopyMarkdown?: () => void | Promise<void>;
  onSaveMarkdown?: () => void | Promise<void>;
  onExportNotion?: () => void | Promise<void>;
  /** Whether an LLM is bound for meetings. null while the check runs. */
  aiReady?: boolean | null;
  /** Why the LLM is not ready, shown on the Ask AI tab. */
  aiBlockedReason?: string | null;
  /** Opens the Models page from the Ask AI tab. */
  onOpenModels?: () => void;
  /** An applied proposal, a hand edit or an undo returned the fresh detail. */
  onNotesChanged?: (detail: MeetingDetailData) => void;
};

/**
 * How far into the meeting a citation may point: the longer of the wall-clock
 * span (once it has ended) and the last line spoken. A recording meeting has
 * no end yet, so only its spoken lines count.
 */
function meetingDurationMs(meeting: Meeting, segments: TranscriptSegment[]): number {
  const spoken = segments.reduce((max, seg) => Math.max(max, seg.end_offset_ms), 0);
  if (!meeting.ended_at) return spoken;
  const wall = Date.parse(meeting.ended_at) - Date.parse(meeting.started_at);
  return Number.isFinite(wall) ? Math.max(wall, spoken) : spoken;
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
  onRename?: MeetingPaneProps["onRenameTitle"];
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
 * The selected meeting: its title, then three tabs.
 *
 * Each tab keeps its own scroll position, so going to the transcript and back
 * lands where the summary was left. The pane is keyed by meeting by the page,
 * so a new selection always opens on Summary.
 */
const MeetingPane = forwardRef<MeetingPaneHandle, MeetingPaneProps>(function MeetingPane(
  {
    meeting,
    detail,
    live,
    liveSegments,
    liveNotes,
    interimNotes = true,
    generating,
    loading,
    busy,
    onBack,
    onDelete,
    onRenameTitle,
    onRenameSpeaker,
    onSetActionItemStatus,
    onAddActionItem,
    onCopyMarkdown,
    onSaveMarkdown,
    onExportNotion,
    aiReady = true,
    aiBlockedReason,
    onOpenModels,
    onNotesChanged,
  },
  ref,
) {
  const [tab, setTab] = useState<PaneTab>("summary");
  // The Ask AI tab mounts on first open and then stays mounted (hidden when
  // another tab shows), so a draft or a reply in progress survives a trip to
  // the transcript for a citation.
  const [askMounted, setAskMounted] = useState(false);
  const tabRef = useRef<PaneTab>("summary");
  const tabButtons = useRef<Partial<Record<PaneTab, HTMLButtonElement | null>>>({});
  const bodyRef = useRef<HTMLDivElement>(null);
  const scrollMemory = useRef<Record<PaneTab, number>>({ summary: 0, transcript: 0, ask: 0 });
  const transcriptRef = useRef<TranscriptTabHandle>(null);
  const pendingJump = useRef<number | null>(null);

  // The detail is only this meeting's once it has landed; until then the
  // previous meeting's would show under this one's title.
  const current = detail && detail.meeting.id === meeting.id ? detail : null;

  const selectTab = (next: PaneTab) => {
    if (bodyRef.current) scrollMemory.current[tabRef.current] = bodyRef.current.scrollTop;
    tabRef.current = next;
    setTab(next);
  };

  // Restores the tab's own position. Runs before the passive effects, so the
  // transcript's follow-the-live-line scroll still wins on the Transcript tab.
  useLayoutEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = scrollMemory.current[tab];
  }, [tab]);

  const onBodyScroll = () => {
    if (bodyRef.current) scrollMemory.current[tabRef.current] = bodyRef.current.scrollTop;
  };

  // A jump asked for while another tab was showing waits for the transcript
  // to mount, then runs once.
  useEffect(() => {
    if (tab !== "transcript" || pendingJump.current === null) return;
    const ms = pendingJump.current;
    pendingJump.current = null;
    transcriptRef.current?.jumpTo(ms);
  }, [tab]);

  // Switches to the Transcript tab and lands on the line covering `ms`. Used by
  // the page's handle and by the Ask AI citations; touches refs and state only.
  const jumpTo = (ms: number) => {
    if (tabRef.current === "transcript") {
      transcriptRef.current?.jumpTo(ms);
    } else {
      pendingJump.current = ms;
      selectTab("transcript");
    }
  };

  useEffect(() => {
    if (tab === "ask") setAskMounted(true);
  }, [tab]);

  useImperativeHandle(
    ref,
    () => ({
      jumpTo,
    }),
    // jumpTo only touches refs and setState, so the handle never goes stale.
    [],
  );

  const onTablistKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const index = TABS.findIndex((t) => t.id === tab);
    let next = -1;
    if (e.key === "ArrowRight") next = (index + 1) % TABS.length;
    else if (e.key === "ArrowLeft") next = (index - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = TABS.length - 1;
    if (next < 0) return;
    e.preventDefault();
    selectTab(TABS[next].id);
    tabButtons.current[TABS[next].id]?.focus();
  };

  const transcriptSegments: TranscriptSegment[] = live
    ? liveSegments
    : (current?.segments ?? []).map((seg) => ({
        ...seg,
        speaker: speakerDisplayName(seg.speaker_key, current?.speakers),
      }));

  const renderTab = () => {
    if (tab === "summary") {
      if (!live && !current && loading) return <LoadingBlock label="Loading meeting…" />;
      return (
        <SummaryTab
          detail={live ? null : current}
          live={live}
          liveNotes={liveNotes}
          interimNotes={interimNotes}
          generating={generating}
          busy={busy}
          onRenameSpeaker={onRenameSpeaker}
          onSetActionItemStatus={onSetActionItemStatus}
          onAddActionItem={onAddActionItem}
          onCopyMarkdown={onCopyMarkdown}
          onSaveMarkdown={onSaveMarkdown}
          onExportNotion={onExportNotion}
          onDelete={live ? undefined : onDelete}
          onNotesChanged={live ? undefined : onNotesChanged}
        />
      );
    }
    if (tab === "transcript") {
      if (!live && !current && loading) return <LoadingBlock label="Loading meeting…" />;
      return (
        <TranscriptTab
          ref={transcriptRef}
          segments={transcriptSegments}
          live={live}
          scrollRef={bodyRef}
          emptyMessage={
            live
              ? "Listening — segments appear every few seconds…"
              : "No segments recorded for this meeting."
          }
        />
      );
    }
    return null;
  };

  // Rendered outside renderTab so it can stay mounted while hidden.
  const askTab = askMounted ? (
    <div hidden={tab !== "ask"}>
      <AskAiTab
        meetingId={meeting.id}
        recording={live || generating}
        durationMs={meetingDurationMs(meeting, transcriptSegments)}
        notes={current?.notes ?? null}
        onJumpTo={jumpTo}
        onNotesChanged={live ? undefined : onNotesChanged}
        aiReady={aiReady}
        aiBlockedReason={aiBlockedReason}
        onOpenModels={onOpenModels}
      />
    </div>
  ) : null;

  const started = meeting.started_at;
  const ended = meeting.ended_at;

  return (
    <div>
      <header style={{ marginBottom: 16 }}>
        {onBack && (
          <button type="button" className="kea-btn" onClick={onBack} style={{ marginBottom: 12 }}>
            Back
          </button>
        )}
        <MeetingTitle
          title={meeting.title}
          source={meeting.title_source}
          onRename={onRenameTitle}
          busy={busy}
        />
        <p className="kea-muted" style={{ margin: 0 }}>
          {started}
          {ended ? ` — ${ended}` : ""}
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

      <div
        role="tablist"
        aria-label="Meeting sections"
        className="kea-meeting-tabs"
        onKeyDown={onTablistKeyDown}
      >
        {TABS.map((t) => {
          const selected = tab === t.id;
          return (
            <button
              key={t.id}
              ref={(el) => {
                tabButtons.current[t.id] = el;
              }}
              type="button"
              role="tab"
              id={`meeting-tab-${t.id}`}
              aria-selected={selected}
              aria-controls="meeting-tabpanel"
              tabIndex={selected ? 0 : -1}
              className="kea-meeting-tab"
              onClick={() => selectTab(t.id)}
            >
              {t.label}
            </button>
          );
        })}
      </div>

      <div
        ref={bodyRef}
        role="tabpanel"
        id="meeting-tabpanel"
        aria-labelledby={`meeting-tab-${tab}`}
        className="kea-meeting-body"
        onScroll={onBodyScroll}
      >
        {renderTab()}
        {askTab}
      </div>
    </div>
  );
});

export default MeetingPane;
