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
import Banner from "../components/Banner";
import HistoryPanel, { statusClass } from "../components/HistoryPanel";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import Toggle from "../components/Toggle";

const PAGE_SIZE = 25;

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
      <header>
        <h1 style={{ marginTop: 0 }}>History</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
          Browse recorded feature actions and rewrite conversations from data.db.
        </p>
      </header>

      {status && <Banner variant="error">{status}</Banner>}

      <RowGroup aria-label="History settings">
        <Row
          label="Store conversation content"
          hint="Keeps transcripts and LLM exchanges in data.db so they can be reviewed here."
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

      <div className="kea-toolbar" style={{ marginTop: 16 }}>
        <button
          type="button"
          className="kea-segment"
          aria-pressed={tab === "actions"}
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
          className="kea-segment"
          aria-pressed={tab === "conversations"}
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
            <div className="kea-card">
              <p className="kea-muted" style={{ margin: 0 }}>
                No conversations stored yet. Enable "Store conversation content"
                above, then run a rewrite or dictation with audio refinement to see
                transcripts and LLM exchanges here.
              </p>
            </div>
          ) : (
            <div className="kea-table-wrap">
              <table className="kea-table">
                <caption className="kea-visually-hidden">Stored conversations</caption>
                <thead>
                  <tr>
                    <th scope="col">ID</th>
                    <th scope="col">Feature</th>
                    <th scope="col">Engine</th>
                    <th scope="col">Model</th>
                    <th scope="col">Created</th>
                    <th scope="col" className="kea-table__actions">
                      <span className="kea-visually-hidden">Actions</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {conversations.map((conv) => (
                    <tr
                      key={conv.id}
                      aria-current={selectedConversationId === conv.id ? "true" : undefined}
                      onClick={() => setSelectedConversationId(conv.id)}
                    >
                      <td>
                        <button
                          type="button"
                          className="kea-table__select"
                          aria-label={`Show conversation ${conv.id}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            setSelectedConversationId(conv.id);
                          }}
                        >
                          {conv.id}
                        </button>
                      </td>
                      <td>{conv.feature_id}</td>
                      <td>{conv.engine_id}</td>
                      <td>{conv.model ?? "—"}</td>
                      <td>{conv.created_at}</td>
                      <td className="kea-table__actions">
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Delete conversation ${conv.id}`}
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
            </div>
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
        <aside className="kea-card" style={{ marginTop: 24 }}>
          <h2 style={{ margin: "0 0 12px" }}>Detail</h2>
          {busy && !actionDetail ? (
            <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 60 }}>
              <Spinner size={16} />
              <span className="kea-muted">Loading detail…</span>
            </div>
          ) : (
            <>
          {actionDetail && (
            <dl className="kea-detail-list">
              <dt>Feature</dt>
              <dd>{actionDetail.feature_id}</dd>
              <dt>Command</dt>
              <dd>{actionDetail.command}</dd>
              <dt>Engine</dt>
              <dd>{actionDetail.engine_id}</dd>
              <dt>Model</dt>
              <dd>{actionDetail.model ?? "—"}</dd>
              <dt>Provider</dt>
              <dd>{actionDetail.provider_ref ?? "—"}</dd>
              <dt>Status</dt>
              <dd className={statusClass(actionDetail.status)}>{actionDetail.status}</dd>
              <dt>Started</dt>
              <dd>{actionDetail.started_at}</dd>
              <dt>Finished</dt>
              <dd>{actionDetail.finished_at ?? "—"}</dd>
              {actionDetail.error && (
                <>
                  <dt>Error</dt>
                  <dd className="kea-status--error">{actionDetail.error}</dd>
                </>
              )}
            </dl>
          )}
          {selectedConversation && !actionDetail && (
            <>
              <dl className="kea-detail-list">
                <dt>Conversation</dt>
                <dd>#{selectedConversation.id}</dd>
                <dt>Feature</dt>
                <dd>{selectedConversation.feature_id}</dd>
                <dt>Engine</dt>
                <dd>{selectedConversation.engine_id}</dd>
                <dt>Model</dt>
                <dd>{selectedConversation.model ?? "—"}</dd>
                <dt>Created</dt>
                <dd>{selectedConversation.created_at}</dd>
                <dt>Linked action</dt>
                <dd>{selectedConversation.action_id ?? "—"}</dd>
              </dl>
              {linkedAction && (
                <div style={{ marginTop: 16 }}>
                  <h3 style={{ margin: "0 0 8px" }}>Linked action</h3>
                  <p className="kea-muted" style={{ margin: 0 }}>
                    {linkedAction.feature_id} / {linkedAction.command} —{" "}
                    <span className={statusClass(linkedAction.status)}>
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
                  <div className="kea-messages">
                    {messages.map((msg) => (
                      <div key={msg.id} className="kea-message">
                        <div className="kea-message__meta">
                          <span className="kea-message__role">{msg.role}</span>
                          <span className="kea-muted">{msg.created_at}</span>
                        </div>
                        <div className="kea-message__body">{msg.content}</div>
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
    </div>
  );
}
