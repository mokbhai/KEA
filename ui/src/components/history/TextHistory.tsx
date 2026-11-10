import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  deleteConversation,
  listHistory,
  onDictationState,
  type HistoryEntry,
} from "../../api";
import { toMessage } from "../../lib/format";
import {
  formatClock,
  formatStoredDateTime,
  MEETING_GROUP_ORDER,
  meetingGroupFor,
  parseStoredTime,
  type MeetingGroupLabel,
} from "../../lib/meetingTime";
import LoadingBlock from "../LoadingBlock";

/** Entries asked for at a time; "Load more" adds another page. */
export const HISTORY_PAGE_SIZE = 50;

/** How long the search waits after the last keystroke before it runs. */
export const HISTORY_SEARCH_DEBOUNCE_MS = 200;

/** How long a Copy button reads "Copied" after it worked. */
const COPIED_MS = 1500;

const FEATURE_LABELS: Record<string, string> = {
  dictation: "Dictation",
  rewrite: "Rewrite",
  assistant: "Assistant",
};

export function featureLabel(featureId: string): string {
  return (
    FEATURE_LABELS[featureId] ??
    (featureId ? featureId.charAt(0).toUpperCase() + featureId.slice(1) : "Entry")
  );
}

/** The text an entry ended with: what was pasted, or what a rewrite produced. */
export function finalText(entry: HistoryEntry): string {
  return entry.output ?? entry.input ?? "";
}

type Section = { title: string; text: string };

/**
 * An entry's text as labelled parts, the finished one first. Both sides are
 * shown only when they differ: a clean-up that changed nothing is one text.
 */
export function sectionsFor(entry: HistoryEntry): Section[] {
  const { input, output } = entry;
  const both = input != null && output != null && input !== output;
  if (entry.feature_id === "dictation") {
    return both
      ? [
          { title: "Cleaned up", text: output },
          { title: "As you said it", text: input },
        ]
      : [{ title: "Transcript", text: output ?? input ?? "" }];
  }
  if (entry.feature_id === "rewrite") {
    if (both) {
      return [
        { title: "Rewritten", text: output },
        { title: "Original", text: input },
      ];
    }
    return output != null
      ? [{ title: "Rewritten", text: output }]
      : [{ title: "Original", text: input ?? "" }];
  }
  return both
    ? [
        { title: "Response", text: output },
        { title: "Request", text: input },
      ]
    : [{ title: "Text", text: output ?? input ?? "" }];
}

/** "14:05" for today, "9 Nov, 14:05" for anything older. */
function rowTime(entry: HistoryEntry, group: MeetingGroupLabel): string {
  const ms = parseStoredTime(entry.created_at);
  if (Number.isNaN(ms)) return entry.created_at;
  return group === "Today" || group === "Yesterday"
    ? formatClock(new Date(ms))
    : formatStoredDateTime(entry.created_at);
}

type Props = {
  /** The feature to show, e.g. "dictation"; null shows every feature's text. */
  feature: string | null;
  /** Whether new text is being saved; shapes the empty state only. */
  storing: boolean;
  /** Where load, copy and delete failures are reported. */
  onError: (message: string | null) => void;
};

/**
 * Past dictations and rewrites as a list of the words themselves, beside the
 * full text of whichever one is selected. Every row copies in one click.
 */
export default function TextHistory({ feature, storing, onError }: Props) {
  const [searchInput, setSearchInput] = useState("");
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(HISTORY_PAGE_SIZE);
  const [entries, setEntries] = useState<HistoryEntry[] | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const [deleting, setDeleting] = useState(false);
  const listRef = useRef<HTMLUListElement>(null);
  // Only the newest request may write to state: a slow answer to an earlier
  // search must not replace the results of the one typed after it.
  const requestSeq = useRef(0);

  const load = useCallback(async () => {
    const seq = ++requestSeq.current;
    try {
      const rows = await listHistory({
        feature: feature ?? undefined,
        query: query || undefined,
        limit,
      });
      if (seq !== requestSeq.current) return;
      setEntries(rows);
      onError(null);
    } catch (e) {
      if (seq !== requestSeq.current) return;
      setEntries((prev) => prev ?? []);
      onError(toMessage(e));
    }
  }, [feature, query, limit, onError]);

  // A different feature is a different list: start it from its first page.
  useEffect(() => {
    setLimit(HISTORY_PAGE_SIZE);
  }, [feature]);

  useEffect(() => {
    void load();
  }, [load]);

  // Debounce the search box into the query that is actually run.
  useEffect(() => {
    const next = searchInput.trim();
    if (next === query) return;
    const timer = setTimeout(() => {
      setQuery(next);
      setLimit(HISTORY_PAGE_SIZE);
    }, HISTORY_SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchInput, query]);

  // Text arrives while this page sits in the background (dictating into
  // another app is the whole point), so pick it up without a manual reload:
  // when a dictation finishes, and whenever the window comes back.
  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState !== "hidden") void load();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    let unlisten: (() => void) | undefined;
    let gone = false;
    onDictationState((state) => {
      if (state === "idle") void load();
    })
      .then((fn) => {
        if (gone) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      gone = true;
      unlisten?.();
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [load]);

  // Something is always open on the right: the newest entry to begin with,
  // and again whenever the selected one drops out of the list.
  useEffect(() => {
    if (!entries) return;
    if (entries.some((e) => e.id === selectedId)) return;
    setSelectedId(entries[0]?.id ?? null);
  }, [entries, selectedId]);

  useEffect(() => {
    if (!copiedKey) return;
    const timer = setTimeout(() => setCopiedKey(null), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copiedKey]);

  const copy = async (key: string, text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopiedKey(key);
      // Re-announce even when the same text is copied twice in a row.
      setAnnouncement("");
      requestAnimationFrame(() => setAnnouncement("Copied to the clipboard."));
      onError(null);
    } catch (e) {
      onError(`Could not copy: ${toMessage(e)}`);
    }
  };

  const remove = async (entry: HistoryEntry) => {
    if (!window.confirm("Delete this entry? Its text is removed from history.")) return;
    setDeleting(true);
    try {
      await deleteConversation(entry.id);
      await load();
    } catch (e) {
      onError(toMessage(e));
    } finally {
      setDeleting(false);
    }
  };

  const grouped = useMemo(() => {
    const now = new Date();
    const buckets = new Map<MeetingGroupLabel, HistoryEntry[]>();
    for (const entry of entries ?? []) {
      const ms = parseStoredTime(entry.created_at);
      const group = Number.isNaN(ms) ? "Earlier" : meetingGroupFor(new Date(ms), now);
      const bucket = buckets.get(group);
      if (bucket) bucket.push(entry);
      else buckets.set(group, [entry]);
    }
    return MEETING_GROUP_ORDER.filter((g) => buckets.has(g)).map((g) => ({
      group: g,
      items: buckets.get(g)!,
    }));
  }, [entries]);

  /** Up and Down walk the rows, opening each as it is reached. */
  const onListKeyDown = (e: KeyboardEvent<HTMLUListElement>) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const rows = Array.from(
      listRef.current?.querySelectorAll<HTMLButtonElement>("[data-history-row]") ?? [],
    );
    const at = rows.findIndex((row) => row === document.activeElement);
    if (at < 0) return;
    const next = rows[at + (e.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    e.preventDefault();
    next.focus();
    next.click();
  };

  const selected = entries?.find((e) => e.id === selectedId) ?? null;
  const filtering = feature !== null || query !== "";
  // With nothing to open there is no right-hand pane, so the list takes the row.
  const hasEntries = (entries?.length ?? 0) > 0;

  return (
    <div className="kea-history-grid">
      <section
        className="kea-card"
        aria-label="Past text"
        style={{ minWidth: 0, gridColumn: hasEntries ? undefined : "1 / -1" }}
      >
        <input
          type="search"
          className="kea-input"
          aria-label="Search history"
          placeholder="Search your words…"
          value={searchInput}
          onChange={(e) => setSearchInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && searchInput) {
              e.preventDefault();
              setSearchInput("");
            }
          }}
          style={{ width: "100%", maxWidth: "none", marginBottom: 4 }}
        />

        {entries === null ? (
          <LoadingBlock label="Loading history…" minHeight={120} />
        ) : entries.length === 0 ? (
          <p className="kea-muted" style={{ margin: "12px 2px" }}>
            {filtering
              ? "Nothing matches."
              : storing
                ? "Nothing here yet. Dictate or rewrite something and it shows up here."
                : "Nothing here yet. Saving text is turned off: turn on “Store conversation content” below to keep what you dictate and rewrite."}
          </p>
        ) : (
          <ul className="kea-history-list" ref={listRef} onKeyDown={onListKeyDown}>
            {grouped.map(({ group, items }) => (
              <li key={group}>
                <div className="kea-history-group">{group}</div>
                <ul style={{ listStyle: "none", margin: 0, padding: 0 }} aria-label={group}>
                  {items.map((entry) => {
                    const key = `row-${entry.id}`;
                    const when = rowTime(entry, group);
                    const label = featureLabel(entry.feature_id);
                    return (
                      <li
                        key={entry.id}
                        className="kea-history-row"
                        data-selected={entry.id === selectedId}
                      >
                        <button
                          type="button"
                          className="kea-history-row__main"
                          data-history-row
                          aria-current={entry.id === selectedId ? "true" : undefined}
                          onClick={() => setSelectedId(entry.id)}
                        >
                          <span className="kea-history-row__text">{finalText(entry)}</span>
                          <span className="kea-history-row__meta">
                            {label} · {when}
                          </span>
                        </button>
                        <button
                          type="button"
                          className="kea-btn kea-history-row__copy"
                          aria-label={`Copy ${label.toLowerCase()} from ${when}`}
                          onClick={() => void copy(key, finalText(entry))}
                        >
                          {copiedKey === key ? "Copied" : "Copy"}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </li>
            ))}
            {entries.length >= limit && (
              <li>
                <button
                  type="button"
                  className="kea-btn"
                  style={{ margin: "8px 0 2px" }}
                  onClick={() => setLimit((n) => n + HISTORY_PAGE_SIZE)}
                >
                  Load more
                </button>
              </li>
            )}
          </ul>
        )}
      </section>

      {hasEntries && (
      <section className="kea-card kea-history-detail" aria-label="Selected entry">
        {selected ? (
          <>
            <div
              style={{
                display: "flex",
                alignItems: "baseline",
                gap: 8,
                flexWrap: "wrap",
                marginBottom: 12,
              }}
            >
              <h2 style={{ margin: 0, fontSize: "1.05rem" }}>
                {featureLabel(selected.feature_id)}
              </h2>
              <span className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                {formatStoredDateTime(selected.created_at)}
              </span>
              <button
                type="button"
                className="kea-btn"
                style={{ marginLeft: "auto" }}
                disabled={deleting}
                onClick={() => void remove(selected)}
              >
                Delete
              </button>
            </div>
            <div className="kea-history-detail__body">
              {sectionsFor(selected).map((section, i) => {
                const key = `detail-${selected.id}-${i}`;
                return (
                  <div key={section.title} style={{ marginTop: i === 0 ? 0 : 20 }}>
                    <div
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        marginBottom: 6,
                      }}
                    >
                      <h3 style={{ margin: 0, fontSize: "0.8125rem" }} className="kea-muted">
                        {section.title}
                      </h3>
                      <button
                        type="button"
                        className="kea-btn"
                        style={{ marginLeft: "auto", padding: "4px 10px", fontSize: "0.75rem" }}
                        aria-label={`Copy ${section.title.toLowerCase()}`}
                        onClick={() => void copy(key, section.text)}
                      >
                        {copiedKey === key ? "Copied" : "Copy"}
                      </button>
                    </div>
                    <p className="kea-history-text">{section.text}</p>
                  </div>
                );
              })}
              <p className="kea-muted" style={{ margin: "16px 0 0", fontSize: "0.75rem" }}>
                {selected.engine_id}
                {selected.model ? ` · ${selected.model}` : ""}
              </p>
            </div>
          </>
        ) : (
          <p className="kea-muted" style={{ margin: 0 }}>
            Pick an entry on the left to read it in full.
          </p>
        )}
      </section>
      )}

      <div className="kea-visually-hidden" role="status" aria-live="polite">
        {announcement}
      </div>
    </div>
  );
}
