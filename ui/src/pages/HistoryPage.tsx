import { useCallback, useEffect, useState } from "react";
import {
  getAction,
  listActions,
  listConversations,
  listMessages,
  deleteConversation,
  getSetting,
  setSetting,
  type ActionDetail,
  type ActionRow,
  type ConversationSummary,
  type Message,
} from "../api";
import HistoryPanel from "../components/HistoryPanel";
import Spinner from "../components/Spinner";

const PAGE_SIZE = 25;

function statusColor(status: string): string {
  if (status === "ok" || status === "success") return "var(--accent)";
  if (status === "error" || status === "failed") return "var(--danger)";
  return "var(--text-muted)";
}

export default function HistoryPage() {
  const [tab, setTab] = useState<"actions" | "conversations">("actions");
  const [query, setQuery] = useState("");
  const [searchInput, setSearchInput] = useState("");
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [actions, setActions] = useState<ActionRow[]>([]);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [selectedActionId, setSelectedActionId] = useState<number | null>(null);
  const [selectedConversationId, setSelectedConversationId] = useState<number | null>(
    null,
  );
  const [actionDetail, setActionDetail] = useState<ActionDetail | null>(null);
  const [linkedAction, setLinkedAction] = useState<ActionDetail | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [messagesLoading, setMessagesLoading] = useState(false);
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
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setStoreBusy(false);
    }
  };

  const refreshActions = useCallback(async () => {
    try {
      const rows = await listActions(query || undefined, limit);
      setActions(rows);
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    }
  }, [query, limit]);

  const refreshConversations = useCallback(async () => {
    try {
      const rows = await listConversations(limit);
      setConversations(rows);
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    }
  }, [limit]);

  useEffect(() => {
    setBusy(true);
    void Promise.all([refreshActions(), refreshConversations()]).finally(() =>
      setBusy(false),
    );
  }, [refreshActions, refreshConversations]);

  useEffect(() => {
    if (!selectedActionId) {
      setActionDetail(null);
      return;
    }
    setBusy(true);
    getAction(selectedActionId)
      .then(setActionDetail)
      .catch((e) => {
        setActionDetail(null);
        setStatus(e instanceof Error ? e.message : String(e));
      })
      .finally(() => setBusy(false));
  }, [selectedActionId]);

  useEffect(() => {
    if (!selectedConversationId) {
      setLinkedAction(null);
      return;
    }
    const conv = conversations.find((c) => c.id === selectedConversationId);
    if (!conv?.action_id) {
      setLinkedAction(null);
      return;
    }
    getAction(conv.action_id)
      .then(setLinkedAction)
      .catch((e) => {
        setLinkedAction(null);
        setStatus(e instanceof Error ? e.message : String(e));
      });
  }, [selectedConversationId, conversations]);

  useEffect(() => {
    if (!selectedConversationId) {
      setMessages([]);
      return;
    }
    setMessagesLoading(true);
    let stale = false;
    listMessages(selectedConversationId)
      .then((msgs) => {
        if (!stale) setMessages(msgs);
      })
      .catch((e) => {
        if (stale) return;
        setMessages([]);
        setStatus(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!stale) setMessagesLoading(false);
      });
    return () => {
      stale = true;
    };
  }, [selectedConversationId]);

  const onSearch = (e: React.FormEvent) => {
    e.preventDefault();
    setQuery(searchInput.trim());
    setLimit(PAGE_SIZE);
    setSelectedActionId(null);
  };

  const tabStyle = (active: boolean): React.CSSProperties => ({
    padding: "6px 12px",
    border: "1px solid var(--border)",
    borderRadius: 6,
    background: active ? "var(--surface-2)" : "var(--surface)",
    color: "var(--text)",
    fontWeight: active ? 600 : 400,
    cursor: "pointer",
    fontFamily: "inherit",
  });

  const selectedConversation = conversations.find((c) => c.id === selectedConversationId);

  const handleDeleteConversation = async (id: number) => {
    if (!window.confirm(`Delete conversation #${id} and all its messages?`)) return;
    try {
      await deleteConversation(id);
      if (selectedConversationId === id) {
        setSelectedConversationId(null);
      }
      await refreshConversations();
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div>
      <h1 style={{ marginTop: 0 }}>History</h1>
      <p className="kea-muted" style={{ marginTop: 0 }}>
        Browse recorded feature actions and rewrite conversations from data.db.
      </p>

      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          marginBottom: 16,
          fontSize: 14,
        }}
      >
        <input
          type="checkbox"
          checked={storeConversations}
          disabled={storeBusy}
          onChange={(e) => onStoreConversationsChange(e.target.checked)}
        />
        <span>Store conversation content</span>
      </label>

      <div style={{ display: "flex", gap: 8, marginBottom: 16 }}>
        <button
          type="button"
          style={tabStyle(tab === "actions")}
          onClick={() => {
            // Clear the other tab's selection so its stale detail can't
            // shadow this tab's (the conversation detail branch is gated on
            // !actionDetail).
            setSelectedConversationId(null);
            setTab("actions");
          }}
        >
          Actions
        </button>
        <button
          type="button"
          style={tabStyle(tab === "conversations")}
          onClick={() => {
            setSelectedActionId(null);
            setTab("conversations");
          }}
        >
          Conversations
        </button>
      </div>

      {tab === "actions" && (
        <>
          <form onSubmit={onSearch} style={{ display: "flex", gap: 8, marginBottom: 16 }}>
            <input
              className="kea-input"
              value={searchInput}
              onChange={(e) => setSearchInput(e.target.value)}
              placeholder="Search feature, command, or engine…"
              style={{ flex: 1, maxWidth: 400 }}
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
          <HistoryPanel
            actions={actions}
            selectedId={selectedActionId}
            onSelect={setSelectedActionId}
          />
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
        </>
      )}

      {tab === "conversations" && (
        <>
          {conversations.length === 0 ? (
            <p className="kea-muted">
              No conversations stored yet. Enable "Store conversation content"
              above, then run a rewrite or dictation with audio refinement to see
              transcripts and LLM exchanges here.
            </p>
          ) : (
            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
              <thead>
                <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
                  <th style={{ padding: "8px 4px" }}>ID</th>
                  <th style={{ padding: "8px 4px" }}>Feature</th>
                  <th style={{ padding: "8px 4px" }}>Engine</th>
                  <th style={{ padding: "8px 4px" }}>Model</th>
                  <th style={{ padding: "8px 4px" }}>Created</th>
                  <th style={{ padding: "8px 4px", width: 40 }} />
                </tr>
              </thead>
              <tbody>
                {conversations.map((conv) => (
                  <tr
                    key={conv.id}
                    onClick={() => setSelectedConversationId(conv.id)}
                    style={{
                      borderBottom: "1px solid var(--border)",
                      cursor: "pointer",
                      background:
                        selectedConversationId === conv.id ? "var(--surface-2)" : undefined,
                    }}
                  >
                    <td style={{ padding: "10px 4px" }}>{conv.id}</td>
                    <td style={{ padding: "10px 4px" }}>{conv.feature_id}</td>
                    <td style={{ padding: "10px 4px" }}>{conv.engine_id}</td>
                    <td style={{ padding: "10px 4px" }}>{conv.model ?? "—"}</td>
                    <td style={{ padding: "10px 4px" }}>{conv.created_at}</td>
                    <td style={{ padding: "10px 4px" }}>
                      <button
                        type="button"
                        className="kea-btn"
                        style={{ fontSize: 12, padding: "2px 8px" }}
                        disabled={busy}
                        onClick={(e) => {
                          e.stopPropagation();
                          handleDeleteConversation(conv.id);
                        }}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {conversations.length >= limit && (
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
        </>
      )}

      {(actionDetail || selectedConversation || (busy && selectedActionId && tab === "actions")) && (
        <aside
          className="kea-card"
          style={{
            marginTop: 24,
            background: "var(--surface-2)",
          }}
        >
          <h2 style={{ margin: "0 0 12px" }}>Detail</h2>
          {busy && !actionDetail ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 60 }}>
              <Spinner size={16} />
              <span className="kea-muted">Loading detail…</span>
            </div>
          ) : (
            <>
          {actionDetail && (
            <dl style={{ margin: 0, display: "grid", gridTemplateColumns: "120px 1fr", gap: 8 }}>
              <dt style={{ fontWeight: 600 }}>Feature</dt>
              <dd style={{ margin: 0 }}>{actionDetail.feature_id}</dd>
              <dt style={{ fontWeight: 600 }}>Command</dt>
              <dd style={{ margin: 0 }}>{actionDetail.command}</dd>
              <dt style={{ fontWeight: 600 }}>Engine</dt>
              <dd style={{ margin: 0 }}>{actionDetail.engine_id}</dd>
              <dt style={{ fontWeight: 600 }}>Model</dt>
              <dd style={{ margin: 0 }}>{actionDetail.model ?? "—"}</dd>
              <dt style={{ fontWeight: 600 }}>Provider</dt>
              <dd style={{ margin: 0 }}>{actionDetail.provider_ref ?? "—"}</dd>
              <dt style={{ fontWeight: 600 }}>Status</dt>
              <dd style={{ margin: 0, color: statusColor(actionDetail.status) }}>
                {actionDetail.status}
              </dd>
              <dt style={{ fontWeight: 600 }}>Started</dt>
              <dd style={{ margin: 0 }}>{actionDetail.started_at}</dd>
              <dt style={{ fontWeight: 600 }}>Finished</dt>
              <dd style={{ margin: 0 }}>{actionDetail.finished_at ?? "—"}</dd>
              {actionDetail.error && (
                <>
                  <dt style={{ fontWeight: 600 }}>Error</dt>
                  <dd style={{ margin: 0, color: "var(--danger)" }}>{actionDetail.error}</dd>
                </>
              )}
            </dl>
          )}
          {selectedConversation && !actionDetail && (
            <>
              <dl
                style={{ margin: 0, display: "grid", gridTemplateColumns: "120px 1fr", gap: 8 }}
              >
                <dt style={{ fontWeight: 600 }}>Conversation</dt>
                <dd style={{ margin: 0 }}>#{selectedConversation.id}</dd>
                <dt style={{ fontWeight: 600 }}>Feature</dt>
                <dd style={{ margin: 0 }}>{selectedConversation.feature_id}</dd>
                <dt style={{ fontWeight: 600 }}>Engine</dt>
                <dd style={{ margin: 0 }}>{selectedConversation.engine_id}</dd>
                <dt style={{ fontWeight: 600 }}>Model</dt>
                <dd style={{ margin: 0 }}>{selectedConversation.model ?? "—"}</dd>
                <dt style={{ fontWeight: 600 }}>Created</dt>
                <dd style={{ margin: 0 }}>{selectedConversation.created_at}</dd>
                <dt style={{ fontWeight: 600 }}>Linked action</dt>
                <dd style={{ margin: 0 }}>
                  {selectedConversation.action_id ?? "—"}
                </dd>
              </dl>
              {linkedAction && (
                <div style={{ marginTop: 16 }}>
                  <h3 style={{ margin: "0 0 8px" }}>Linked action</h3>
                  <p className="kea-muted" style={{ margin: 0 }}>
                    {linkedAction.feature_id} / {linkedAction.command} —{" "}
                    <span style={{ color: statusColor(linkedAction.status) }}>
                      {linkedAction.status}
                    </span>
                  </p>
                </div>
              )}
              <div style={{ marginTop: 16 }}>
                <h3 style={{ margin: "0 0 8px" }}>
                  Messages
                </h3>
                {messagesLoading ? (
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <Spinner size={14} />
                    <span className="kea-muted">Loading messages…</span>
                  </div>
                ) : messages.length === 0 ? (
                  <p className="kea-muted" style={{ margin: 0 }}>
                    No messages in this conversation.
                  </p>
                ) : (
                  <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    {messages.map((msg) => (
                      <div
                        key={msg.id}
                        style={{
                          padding: "8px 12px",
                          borderRadius: 6,
                          background: "var(--surface)",
                          border: "1px solid var(--border)",
                          fontSize: 13,
                        }}
                      >
                        <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4 }}>
                          <span style={{ fontWeight: 600, color: "var(--accent)" }}>
                            {msg.role}
                          </span>
                          <span className="kea-muted">{msg.created_at}</span>
                        </div>
                        <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                          {msg.content}
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
              <div style={{ marginTop: 16 }}>
                <button
                  type="button"
                  className="kea-btn"
                  disabled={busy}
                  onClick={() => handleDeleteConversation(selectedConversation.id)}
                >
                  Delete conversation
                </button>
              </div>
            </>
          )}
          </>
        )}
        </aside>
      )}

      {status && (
        <p style={{ marginTop: 12, fontSize: 13, color: "var(--danger)" }}>{status}</p>
      )}
    </div>
  );
}
