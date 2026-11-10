import { useCallback, useEffect, useState, type FormEvent } from "react";
import { getAction, listActions, type ActionDetail, type ActionRow } from "../../api";
import { toMessage } from "../../lib/format";
import { formatStoredDateTime } from "../../lib/meetingTime";
import HistoryPanel, { statusClass } from "../HistoryPanel";
import LoadingBlock from "../LoadingBlock";

const PAGE_SIZE = 25;

type Props = {
  /** Where load failures are reported. */
  onError: (message: string | null) => void;
};

/**
 * Every run of every feature, failed ones included, with the selected run's
 * details beside the table rather than under it.
 */
export default function ActivityLog({ onError }: Props) {
  const [query, setQuery] = useState("");
  const [searchInput, setSearchInput] = useState("");
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [actions, setActions] = useState<ActionRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [detail, setDetail] = useState<ActionDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      setActions(await listActions(query || undefined, limit));
    } catch (e) {
      onError(toMessage(e));
    } finally {
      setBusy(false);
    }
  }, [query, limit, onError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (selectedId == null) {
      setDetail(null);
      return;
    }
    let stale = false;
    setDetailLoading(true);
    getAction(selectedId)
      .then((d) => {
        if (!stale) setDetail(d);
      })
      .catch((e) => {
        if (stale) return;
        setDetail(null);
        onError(toMessage(e));
      })
      .finally(() => {
        if (!stale) setDetailLoading(false);
      });
    return () => {
      stale = true;
    };
  }, [selectedId, onError]);

  const onSearch = (e: FormEvent) => {
    e.preventDefault();
    setQuery(searchInput.trim());
    setLimit(PAGE_SIZE);
    setSelectedId(null);
  };

  return (
    <div className="kea-history-grid kea-history-grid--log">
      <div style={{ minWidth: 0 }}>
        <form onSubmit={onSearch} className="kea-toolbar">
          <input
            className="kea-input kea-toolbar__grow"
            aria-label="Search actions"
            value={searchInput}
            onChange={(e) => setSearchInput(e.target.value)}
            placeholder="Search feature, command, or engine…"
          />
          <button type="submit" className="kea-btn" disabled={busy}>
            Search
          </button>
          <button
            type="button"
            className="kea-btn"
            disabled={busy}
            onClick={() => {
              setSearchInput("");
              setQuery("");
              setLimit(PAGE_SIZE);
            }}
          >
            Clear
          </button>
        </form>
        <HistoryPanel actions={actions} selectedId={selectedId} onSelect={setSelectedId} />
        {actions.length >= limit && (
          <button
            type="button"
            className="kea-btn"
            style={{ marginTop: 12 }}
            disabled={busy}
            onClick={() => setLimit((n) => n + PAGE_SIZE)}
          >
            Load more
          </button>
        )}
      </div>

      <aside className="kea-card kea-history-detail">
        <h2 style={{ margin: "0 0 12px", fontSize: "1.05rem" }}>Detail</h2>
        {detailLoading && !detail ? (
          <LoadingBlock label="Loading detail…" minHeight={60} />
        ) : detail ? (
          <dl className="kea-detail-list">
            <dt>Feature</dt>
            <dd>{detail.feature_id}</dd>
            <dt>Command</dt>
            <dd>{detail.command}</dd>
            <dt>Engine</dt>
            <dd>{detail.engine_id}</dd>
            <dt>Model</dt>
            <dd>{detail.model ?? "—"}</dd>
            <dt>Provider</dt>
            <dd>{detail.provider_ref ?? "—"}</dd>
            <dt>Status</dt>
            <dd className={statusClass(detail.status)}>{detail.status}</dd>
            <dt>Started</dt>
            <dd>{formatStoredDateTime(detail.started_at)}</dd>
            <dt>Finished</dt>
            <dd>{detail.finished_at ? formatStoredDateTime(detail.finished_at) : "—"}</dd>
            {detail.error && (
              <>
                <dt>Error</dt>
                <dd className="kea-status--error">{detail.error}</dd>
              </>
            )}
          </dl>
        ) : (
          <p className="kea-muted" style={{ margin: 0 }}>
            Pick a run on the left to see what it used and whether it worked.
          </p>
        )}
      </aside>
    </div>
  );
}
