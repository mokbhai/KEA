import { useCallback, useEffect, useState } from "react";
import { openLogFolder, tailLogs } from "../api";
import Banner from "../components/Banner";
import LogsViewer from "../components/LogsViewer";

const DEFAULT_MAX_BYTES = 64 * 1024;

export default function LogsPage() {
  const [content, setContent] = useState("");
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setStatus(null);
    try {
      const tail = await tailLogs(DEFAULT_MAX_BYTES);
      setContent(tail);
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const onOpenFolder = async () => {
    setStatus(null);
    try {
      await openLogFolder();
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Logs</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
          Tail of the rolling KEA application log (last{" "}
          {Math.round(DEFAULT_MAX_BYTES / 1024)} KB).
        </p>
      </header>

      {status && <Banner variant="error">{status}</Banner>}

      <div className="kea-toolbar">
        <button type="button" className="kea-btn" onClick={refresh} disabled={loading}>
          Refresh
        </button>
        <button type="button" className="kea-btn" onClick={onOpenFolder}>
          Open log folder
        </button>
      </div>
      <LogsViewer content={content} loading={loading} />
    </div>
  );
}
