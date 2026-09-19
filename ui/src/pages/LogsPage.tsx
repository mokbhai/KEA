import { useCallback, useEffect, useRef, useState } from "react";
import { openLogFolder, tailLogs } from "../api";
import Banner from "../components/Banner";
import LogsViewer from "../components/LogsViewer";
import { toMessage } from "../lib/format";

const DEFAULT_MAX_BYTES = 64 * 1024;

/**
 * How often the tail is re-read while "Live" is on.
 *
 * The failures this page exists to catch — a paste that goes nowhere, a
 * clipboard write that does not stick — happen once in a while and cannot be
 * reproduced on demand. Having to press Refresh means you only ever see the
 * log *after* deciding something went wrong, by which point the interesting
 * lines may have scrolled past the tail window. One second is well inside the
 * cost of a file read and slow enough not to fight with selecting text.
 */
const LIVE_INTERVAL_MS = 1000;

export default function LogsPage() {
  const [content, setContent] = useState("");
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [live, setLive] = useState(false);
  // Polling must not flash the spinner on every tick, or the pane is unreadable.
  const quiet = useRef(false);

  const refresh = useCallback(async () => {
    if (!quiet.current) setLoading(true);
    setStatus(null);
    try {
      const tail = await tailLogs(DEFAULT_MAX_BYTES);
      setContent(tail);
    } catch (e) {
      setStatus(toMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!live) return;
    const id = setInterval(() => {
      quiet.current = true;
      void refresh().finally(() => {
        quiet.current = false;
      });
    }, LIVE_INTERVAL_MS);
    return () => clearInterval(id);
  }, [live, refresh]);

  const onOpenFolder = async () => {
    setStatus(null);
    try {
      await openLogFolder();
    } catch (e) {
      setStatus(toMessage(e));
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Logs</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
          Tail of the rolling KEA application log (last{" "}
          {Math.round(DEFAULT_MAX_BYTES / 1024)} KB). Turn on Live and dictate to
          watch a run as it happens.
        </p>
      </header>

      {status && <Banner variant="error">{status}</Banner>}

      <div className="kea-toolbar">
        <button type="button" className="kea-btn" onClick={refresh} disabled={loading}>
          Refresh
        </button>
        <button
          type="button"
          className="kea-btn"
          aria-pressed={live}
          onClick={() => setLive((on) => !on)}
        >
          {live ? "Stop live" : "Live"}
        </button>
        <button type="button" className="kea-btn" onClick={onOpenFolder}>
          Open log folder
        </button>
      </div>
      <LogsViewer content={content} loading={loading} follow={live} />
    </div>
  );
}
