import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { searchMeetings, type Meeting } from "../../api";
import { toMessage } from "../../lib/format";
import {
  formatClock,
  formatDuration,
  formatElapsed,
  MEETING_GROUP_ORDER,
  meetingGroupFor,
  type MeetingGroupLabel,
} from "../../lib/meetingTime";

/** How long the search waits after the last keystroke before it runs. */
export const SEARCH_DEBOUNCE_MS = 200;

/** Results asked for per search. The list is a skim, not an archive. */
const SEARCH_LIMIT = 200;

type Props = {
  /** The recent meetings, newest first. The recording meeting is not in here. */
  meetings: Meeting[];
  /** The meeting being recorded, pinned above the groups. */
  recording: Meeting | null;
  selectedId: string | null;
  onSelect: (id: string) => void;
};

/** Elapsed time for a meeting that is still recording, ticking once a second. */
function RecordingElapsed({ startedAt }: { startedAt: string }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const started = Date.parse(startedAt);
  const elapsed = Number.isNaN(started) ? 0 : now - started;
  return <span style={{ fontVariantNumeric: "tabular-nums" }}>{formatElapsed(elapsed)}</span>;
}

/** The second line of a row: start time, then duration (or the live clock). */
function RowMeta({ meeting }: { meeting: Meeting }) {
  const started = new Date(meeting.started_at);
  const startLabel = Number.isNaN(started.getTime()) ? "" : formatClock(started);
  if (meeting.status === "recording") {
    return (
      <span className="kea-muted" style={{ fontSize: 11 }}>
        {startLabel && `${startLabel} · `}
        <RecordingElapsed startedAt={meeting.started_at} />
      </span>
    );
  }
  const ended = meeting.ended_at ? Date.parse(meeting.ended_at) : NaN;
  const duration = Number.isNaN(started.getTime()) || Number.isNaN(ended)
    ? null
    : formatDuration(ended - started.getTime());
  return (
    <span className="kea-muted" style={{ fontSize: 11 }}>
      {startLabel}
      {duration && ` · ${duration}`}
    </span>
  );
}

const rowStyle = (selected: boolean): CSSProperties => ({
  width: "100%",
  textAlign: "left",
  padding: "8px 10px",
  border: selected ? "2px solid var(--accent)" : "1px solid var(--border)",
  borderRadius: 6,
  background: selected ? "var(--surface-2)" : "var(--surface)",
  color: "var(--text)",
  cursor: "pointer",
  fontFamily: "inherit",
  display: "flex",
  flexDirection: "column",
  gap: 2,
});

export default function MeetingList({ meetings, recording, selectedId, onSelect }: Props) {
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [results, setResults] = useState<{ query: string; items: Meeting[] } | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const optionRefs = useRef(new Map<string, HTMLElement>());

  // A cleared box shows the normal list at once; typing waits for a pause.
  useEffect(() => {
    const trimmed = query.trim();
    if (!trimmed) {
      setDebounced("");
      return;
    }
    const timer = setTimeout(() => setDebounced(trimmed), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  useEffect(() => {
    if (!debounced) return;
    let cancelled = false;
    setSearchError(null);
    searchMeetings(debounced, SEARCH_LIMIT)
      .then((items) => {
        if (!cancelled) setResults({ query: debounced, items });
      })
      .catch((e) => {
        if (!cancelled) setSearchError(toMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [debounced]);

  const searching = debounced !== "";
  // Results for a query still being typed would show the previous answer.
  const searchItems = searching && results?.query === debounced ? results.items : null;
  const pinned = searching ? null : recording;

  // Grouped on every render, not memoised on the list: the day changes under a
  // list nobody has re-fetched, and a meeting must move groups at midnight.
  const now = new Date();
  const source = searching ? (searchItems ?? []) : meetings;
  const byLabel = new Map<MeetingGroupLabel, Meeting[]>();
  for (const m of source) {
    const started = new Date(m.started_at);
    if (Number.isNaN(started.getTime())) continue;
    const label = meetingGroupFor(started, now);
    const bucket = byLabel.get(label) ?? [];
    bucket.push(m);
    byLabel.set(label, bucket);
  }
  const groups = MEETING_GROUP_ORDER.flatMap((label) => {
    const items = byLabel.get(label);
    if (!items) return [];
    items.sort((a, b) => Date.parse(b.started_at) - Date.parse(a.started_at));
    return [{ label, items }];
  });

  // Keyboard order matches what is on screen: pinned row first, then groups.
  const options: Meeting[] = [...(pinned ? [pinned] : []), ...groups.flatMap((g) => g.items)];
  const indexById = new Map(options.map((m, i) => [m.id, i] as const));

  // Follow the selection when it moves from elsewhere (a stop, a new recording).
  // Keyed on the index, not the array, so arrow keys are not undone each render.
  const selectedIndex = options.findIndex((m) => m.id === selectedId);
  useEffect(() => {
    if (selectedIndex >= 0) setActiveIndex(selectedIndex);
  }, [selectedIndex]);

  const moveTo = (index: number) => {
    if (options.length === 0) return;
    const next = Math.max(0, Math.min(options.length - 1, index));
    setActiveIndex(next);
    optionRefs.current.get(options[next].id)?.focus();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (options.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveTo(activeIndex + 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveTo(activeIndex - 1);
    } else if (e.key === "Home") {
      e.preventDefault();
      moveTo(0);
    } else if (e.key === "End") {
      e.preventDefault();
      moveTo(options.length - 1);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const m = options[activeIndex];
      if (m) onSelect(m.id);
    }
  };

  const clearSearch = () => {
    setQuery("");
    setDebounced("");
    setResults(null);
  };

  const renderRow = (m: Meeting) => {
    const index = indexById.get(m.id) ?? 0;
    const selected = m.id === selectedId;
    const failed = m.status === "error";
    const isRecording = m.status === "recording";
    return (
      <div
        key={m.id}
        role="option"
        aria-selected={selected}
        id={`meeting-option-${m.id}`}
        tabIndex={index === activeIndex ? 0 : -1}
        ref={(el) => {
          if (el) optionRefs.current.set(m.id, el);
          else optionRefs.current.delete(m.id);
        }}
        onClick={() => {
          setActiveIndex(index);
          onSelect(m.id);
        }}
        onFocus={() => setActiveIndex(index)}
        style={{ ...rowStyle(selected), marginBottom: 4 }}
      >
        <span style={{ display: "flex", alignItems: "center", gap: 6, minWidth: 0 }}>
          {isRecording && (
            <span
              role="img"
              aria-label="Recording"
              title="Recording"
              style={{
                width: 8,
                height: 8,
                borderRadius: "50%",
                background: "var(--danger)",
                flex: "none",
              }}
            />
          )}
          <span
            style={{
              fontWeight: 600,
              fontSize: 13,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              flex: 1,
              minWidth: 0,
            }}
          >
            {m.title}
          </span>
          {failed && (
            <span
              title="This meeting ended with an error"
              style={{ color: "var(--danger)", fontSize: 11, flex: "none" }}
            >
              Failed
            </span>
          )}
        </span>
        <RowMeta meeting={m} />
      </div>
    );
  };

  const noMatch = searching && searchItems !== null && searchItems.length === 0;
  const hasAnything = options.length > 0;

  return (
    <div className="kea-card" style={{ padding: 12, display: "flex", flexDirection: "column", gap: 10 }}>
      <h2 style={{ margin: 0, fontSize: 15 }}>Past meetings</h2>
      <input
        type="search"
        className="kea-input"
        aria-label="Search meetings"
        placeholder="Search meetings"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape" && query) {
            e.stopPropagation();
            clearSearch();
          }
        }}
      />

      {searchError && (
        <p style={{ margin: 0, fontSize: 13, color: "var(--danger)" }}>{searchError}</p>
      )}

      {noMatch ? (
        <div>
          <p className="kea-muted" style={{ margin: "0 0 8px" }}>
            No meeting matches “{debounced}”.
          </p>
          <button type="button" className="kea-btn" onClick={clearSearch}>
            Clear search
          </button>
        </div>
      ) : !hasAnything && !searching ? (
        <p className="kea-muted" style={{ margin: 0 }}>
          No meetings yet.
        </p>
      ) : (
        <div
          role="listbox"
          aria-label="Meetings"
          onKeyDown={onKeyDown}
          style={{ maxHeight: 560, overflowY: "auto", paddingRight: 2 }}
        >
          {pinned && <div style={{ marginBottom: 10 }}>{renderRow(pinned)}</div>}
          {groups.map((group) => (
            <div key={group.label} role="group" aria-label={group.label} style={{ marginBottom: 8 }}>
              <div
                aria-hidden="true"
                className="kea-muted"
                style={{ fontSize: 11, fontWeight: 600, textTransform: "uppercase", margin: "4px 2px" }}
              >
                {group.label}
              </div>
              {group.items.map(renderRow)}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
