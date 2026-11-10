import { useEffect, useState } from "react";
import PageHeader from "../components/PageHeader";
import { getSetting, setSetting } from "../api";
import { toMessage } from "../lib/format";
import Banner from "../components/Banner";
import ActivityLog from "../components/history/ActivityLog";
import TextHistory from "../components/history/TextHistory";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import Toggle from "../components/Toggle";

/**
 * The page's views. The first three are the same list of saved text, narrowed
 * to one feature; the last is every run of every feature, text or not.
 */
const VIEWS = [
  { id: "all", label: "All", feature: null },
  { id: "dictation", label: "Dictation", feature: "dictation" },
  { id: "rewrite", label: "Rewrite", feature: "rewrite" },
  { id: "log", label: "Activity log", feature: null },
] as const;

type ViewId = (typeof VIEWS)[number]["id"];

export default function HistoryPage() {
  const [view, setView] = useState<ViewId>("all");
  const [status, setStatus] = useState<string | null>(null);
  const [storeConversations, setStoreConversations] = useState(true);
  const [storeBusy, setStoreBusy] = useState(false);

  useEffect(() => {
    getSetting("history.store_conversations")
      .then((v) => {
        if (v === "false") setStoreConversations(false);
      })
      .catch(() => {});
  }, []);

  const onStoreConversationsChange = async (enabled: boolean) => {
    const prev = storeConversations;
    setStoreConversations(enabled);
    setStoreBusy(true);
    try {
      await setSetting("history.store_conversations", String(enabled));
    } catch (e) {
      setStoreConversations(prev);
      setStatus(toMessage(e));
    } finally {
      setStoreBusy(false);
    }
  };

  return (
    <div>
      <PageHeader
        title="History"
        tight
        summary="What you dictated and rewrote. Copy any of it, or open it to read in full."
      />

      {status && <Banner variant="error">{status}</Banner>}

      <div
        className="kea-toolbar"
        role="group"
        aria-label="History view"
        style={{ marginBottom: 12 }}
      >
        {VIEWS.map((v) => (
          <button
            key={v.id}
            type="button"
            className="kea-segment"
            aria-pressed={view === v.id}
            // The log is a different kind of list, so it stands apart.
            style={v.id === "log" ? { marginLeft: "auto" } : undefined}
            onClick={() => setView(v.id)}
          >
            {v.label}
          </button>
        ))}
      </div>

      {view === "log" ? (
        <ActivityLog onError={setStatus} />
      ) : (
        <TextHistory
          feature={VIEWS.find((v) => v.id === view)?.feature ?? null}
          storing={storeConversations}
          onError={setStatus}
        />
      )}

      {/* Below the lists: a setting changed once should not push the thing the
          page is for down the screen. */}
      <div style={{ marginTop: 24 }}>
        <RowGroup aria-label="History settings">
          <Row
            label="Store conversation content"
            hint="Keeps the text you dictate and rewrite on this Mac so it can be found here."
          >
            {storeBusy && <Spinner size={14} />}
            <Toggle
              label="Store conversation content"
              checked={storeConversations}
              disabled={storeBusy}
              onChange={(next) => void onStoreConversationsChange(next)}
            />
          </Row>
        </RowGroup>
      </div>
    </div>
  );
}
