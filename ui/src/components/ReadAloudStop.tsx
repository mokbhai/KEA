import { useEffect, useState } from "react";

import { onTtsState, stopReadAloud } from "../api";
import { toMessage } from "../lib/format";

/**
 * The Stop button for a read-aloud run.
 *
 * Shown only while the backend reports a run in progress (`tts:state` is
 * "reading" from the moment the selection is captured until the audio has
 * ended or been stopped), so there is never a dead button on the page. Its own
 * component, not inline in the page, so it owns the event subscription and the
 * page's busy flag — which locks every other control for the length of a run —
 * cannot disable it.
 */
export default function ReadAloudStop() {
  const [reading, setReading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = onTtsState((state) => {
      setReading(state === "reading");
      if (state === "reading") setError(null);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  if (!reading && !error) return null;

  const stop = async () => {
    try {
      await stopReadAloud();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  return (
    <div
      className="kea-card"
      role="status"
      style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 24 }}
    >
      {reading ? (
        <>
          <span style={{ flex: 1 }}>Reading aloud…</span>
          <button type="button" className="kea-btn kea-btn--primary" onClick={() => void stop()}>
            Stop
          </button>
        </>
      ) : (
        <span style={{ color: "var(--danger)" }}>{error}</span>
      )}
    </div>
  );
}
