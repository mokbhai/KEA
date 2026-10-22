import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import {
  applyNotesProposal,
  clearMeetingChat,
  discardNotesProposal,
  getMeetingChat,
  sendMeetingChat,
  type MeetingChatMessage,
  type MeetingDetail,
  type MeetingNotes,
  type NotesSection,
  type ProposalMeta,
} from "../../api";
import { toMessage } from "../../lib/format";

/** Matches a `[mm:ss]` citation. Groups are the minutes and the seconds. */
const CITATION_PATTERN = /\[(\d{1,3}):([0-5]\d)\]/;

/** The notes sections a proposal can change, labelled as the Summary tab labels them. */
const SECTION_LABELS: Record<NotesSection, string> = {
  summary: "Summary",
  decisions: "Decisions",
  follow_ups: "Follow-ups",
  open_questions: "Open questions",
};

/** The one-click prompts above the input. Each sends a normal chat message. */
const QUICK_PROMPTS: readonly { label: string; text: string }[] = [
  { label: "Shorter", text: "Make the notes shorter." },
  { label: "Email-ready", text: "Rewrite the notes as an email-ready summary." },
  { label: "Only decisions", text: "Reduce the notes to the decisions only." },
];

export type ReplyPart =
  | { kind: "text"; text: string }
  | { kind: "cite"; label: string; ms: number };

/**
 * Splits a reply into plain text and `[mm:ss]` citations.
 *
 * Only the text is ever shown as text: nothing in a reply is parsed as markup,
 * so a model that writes HTML gets it shown as written.
 */
export function splitCitations(reply: string): ReplyPart[] {
  const parts = reply.split(new RegExp(CITATION_PATTERN.source, "g"));
  const out: ReplyPart[] = [];
  // split with two capture groups yields [text, mm, ss, text, mm, ss, text, …].
  for (let i = 0; i < parts.length; i += 3) {
    if (parts[i]) out.push({ kind: "text", text: parts[i] });
    if (i + 2 < parts.length) {
      const minutes = Number(parts[i + 1]);
      const seconds = Number(parts[i + 2]);
      out.push({
        kind: "cite",
        label: `[${parts[i + 1]}:${parts[i + 2]}]`,
        ms: (minutes * 60 + seconds) * 1000,
      });
    }
  }
  return out;
}

/** Shows a reply as plain text with line breaks; in-range citations become buttons. */
function ReplyText({
  reply,
  durationMs,
  onJumpTo,
}: {
  reply: string;
  durationMs: number;
  onJumpTo: (ms: number) => void;
}) {
  return (
    <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
      {splitCitations(reply).map((part, index): ReactNode => {
        if (part.kind === "text") return <span key={index}>{part.text}</span>;
        // A time past the end of the meeting is not something to jump to.
        if (part.ms > durationMs) return <span key={index}>{part.label}</span>;
        return (
          <button
            key={index}
            type="button"
            onClick={() => onJumpTo(part.ms)}
            style={{
              background: "none",
              border: "none",
              padding: 0,
              font: "inherit",
              color: "var(--accent)",
              textDecoration: "underline",
              cursor: "pointer",
            }}
          >
            {part.label}
          </button>
        );
      })}
    </div>
  );
}

type ProposalCardProps = {
  proposal: ProposalMeta;
  current: MeetingNotes | null;
  /** Edits are not offered while recording: the card then shows only its state. */
  recording: boolean;
  busy: boolean;
  onApply: () => void;
  onDiscard: () => void;
};

/** A proposed notes change: before and after per changed section, with Apply and Discard. */
function ProposalCard({ proposal, current, recording, busy, onApply, onDiscard }: ProposalCardProps) {
  const pending = proposal.state === "pending";
  if (pending && recording) return null;
  if (!pending) {
    return (
      <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: 12 }}>
        {proposal.state === "applied" ? "Applied to the notes." : "Discarded."}
      </p>
    );
  }
  return (
    <div
      role="group"
      aria-label="Proposed notes change"
      style={{
        marginTop: 8,
        padding: 10,
        border: "1px solid var(--border)",
        borderRadius: 6,
        background: "var(--surface-2)",
      }}
    >
      <p className="kea-muted" style={{ margin: "0 0 8px", fontSize: 12 }}>
        Proposed change to the notes. Nothing changes until you apply it.
      </p>
      {proposal.changed.map((section) => (
        <section key={section} style={{ marginBottom: 10 }}>
          <h4 style={{ margin: "0 0 4px", fontSize: 13 }}>{SECTION_LABELS[section]}</h4>
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
            <div>
              <span className="kea-muted" style={{ fontSize: 11 }}>
                Now
              </span>
              <p style={{ margin: 0, whiteSpace: "pre-wrap", fontSize: 13 }}>
                {current?.[section]?.trim() || "(empty)"}
              </p>
            </div>
            <div>
              <span className="kea-muted" style={{ fontSize: 11 }}>
                Proposed
              </span>
              <p style={{ margin: 0, whiteSpace: "pre-wrap", fontSize: 13 }}>
                {proposal.proposal[section].trim() || "(empty)"}
              </p>
            </div>
          </div>
        </section>
      ))}
      <div style={{ display: "flex", gap: 8 }}>
        <button type="button" className="kea-btn kea-btn--primary" onClick={onApply} disabled={busy}>
          Apply
        </button>
        <button type="button" className="kea-btn" onClick={onDiscard} disabled={busy}>
          Discard
        </button>
      </div>
    </div>
  );
}

export type AskAiTabProps = {
  meetingId: string;
  /** The meeting is still recording: no edit proposals, and a note says so. */
  recording: boolean;
  /** How far a citation may point into the meeting, in milliseconds. */
  durationMs: number;
  /** The meeting's current notes: the "now" side of a proposal. */
  notes: MeetingNotes | null;
  /** Switches to the Transcript tab at a time (a citation was clicked). */
  onJumpTo: (ms: number) => void;
  /** An applied proposal returned the fresh detail. */
  onNotesChanged?: (detail: MeetingDetail) => void;
  /** Whether an LLM is bound for meetings. null while the check is running. */
  aiReady?: boolean | null;
  /** Why the LLM is not ready, in the banner's words. */
  aiBlockedReason?: string | null;
  /** Opens the Models page, where a model is bound. */
  onOpenModels?: () => void;
};

/**
 * The Ask AI tab: a chat about one meeting, grounded in its notes and transcript.
 *
 * History loads from the meeting's saved chat. Replies are whole, not streamed,
 * so a turn shows a pending bubble and Send is off until it lands.
 */
export default function AskAiTab({
  meetingId,
  recording,
  durationMs,
  notes,
  onJumpTo,
  onNotesChanged,
  aiReady = true,
  aiBlockedReason,
  onOpenModels,
}: AskAiTabProps) {
  const [messages, setMessages] = useState<MeetingChatMessage[] | null>(null);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [pendingText, setPendingText] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [translating, setTranslating] = useState(false);
  const [language, setLanguage] = useState("");
  const [confirmingClear, setConfirmingClear] = useState(false);
  const [clearError, setClearError] = useState<string | null>(null);
  const [proposalBusy, setProposalBusy] = useState<number | null>(null);
  const [proposalError, setProposalError] = useState<string | null>(null);

  // Read by every async callback, so a reply that lands after the user has
  // moved to another meeting is dropped instead of written into this one.
  const currentMeeting = useRef(meetingId);
  currentMeeting.current = meetingId;
  // Synchronous guards: a second Enter or click before a re-render must not
  // start a second turn or a second apply.
  const turnInFlight = useRef(false);
  const proposalsInFlight = useRef(new Set<number>());
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let current = true;
    setMessages(null);
    setHistoryError(null);
    setTruncated(false);
    setSendError(null);
    setPendingText(null);
    getMeetingChat(meetingId).then(
      (list) => {
        if (current) setMessages(list);
      },
      (e) => {
        if (current) setHistoryError(toMessage(e));
      },
    );
    return () => {
      current = false;
    };
  }, [meetingId]);

  useEffect(() => {
    endRef.current?.scrollIntoView?.({ block: "end" });
  }, [messages?.length, pendingText]);

  /** Re-reads the saved chat, the only record of proposal states. */
  const reloadChat = async (id: string) => {
    const list = await getMeetingChat(id);
    if (currentMeeting.current === id) setMessages(list);
  };

  const send = async (text: string, fromDraft: boolean) => {
    const trimmed = text.trim();
    if (!trimmed || turnInFlight.current || aiReady !== true) return;
    const id = meetingId;
    turnInFlight.current = true;
    setPendingText(trimmed);
    setSendError(null);
    try {
      const result = await sendMeetingChat(id, trimmed);
      if (currentMeeting.current !== id) return;
      setTruncated(result.truncated);
      if (fromDraft) setDraft("");
      await reloadChat(id);
    } catch (e) {
      // The typed text stays in the box, so a retry does not start from scratch.
      if (currentMeeting.current === id) setSendError(toMessage(e));
    } finally {
      turnInFlight.current = false;
      if (currentMeeting.current === id) setPendingText(null);
    }
  };

  const onInputKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter sends; Shift+Enter is a new line. IME composition keeps its Enter.
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send(draft, true);
    }
  };

  const sendTranslate = () => {
    const lang = language.trim();
    if (!lang) return;
    setLanguage("");
    setTranslating(false);
    void send(`Translate the notes into ${lang}.`, false);
  };

  const applyProposal = async (message: MeetingChatMessage) => {
    if (proposalsInFlight.current.has(message.id)) return;
    proposalsInFlight.current.add(message.id);
    const id = meetingId;
    setProposalBusy(message.id);
    setProposalError(null);
    try {
      const detail = await applyNotesProposal(id, message.id);
      if (currentMeeting.current !== id) return;
      onNotesChanged?.(detail);
      await reloadChat(id);
    } catch (e) {
      if (currentMeeting.current === id) setProposalError(toMessage(e));
    } finally {
      proposalsInFlight.current.delete(message.id);
      if (currentMeeting.current === id) setProposalBusy(null);
    }
  };

  const discardProposal = async (message: MeetingChatMessage) => {
    if (proposalsInFlight.current.has(message.id)) return;
    proposalsInFlight.current.add(message.id);
    const id = meetingId;
    setProposalBusy(message.id);
    setProposalError(null);
    try {
      await discardNotesProposal(id, message.id);
      if (currentMeeting.current === id) await reloadChat(id);
    } catch (e) {
      if (currentMeeting.current === id) setProposalError(toMessage(e));
    } finally {
      proposalsInFlight.current.delete(message.id);
      if (currentMeeting.current === id) setProposalBusy(null);
    }
  };

  const clearChat = async () => {
    const id = meetingId;
    setClearError(null);
    try {
      await clearMeetingChat(id);
      if (currentMeeting.current !== id) return;
      setMessages([]);
      setTruncated(false);
      setConfirmingClear(false);
    } catch (e) {
      if (currentMeeting.current === id) setClearError(toMessage(e));
    }
  };

  const busy = pendingText !== null || proposalBusy !== null;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      {historyError && (
        <p role="alert" style={{ margin: 0, fontSize: 13, color: "var(--danger)" }}>
          {historyError}
        </p>
      )}

      {messages === null && !historyError && (
        <p className="kea-muted" style={{ margin: 0 }}>
          Loading chat…
        </p>
      )}

      {messages !== null && messages.length === 0 && pendingText === null && (
        <p className="kea-muted" style={{ margin: 0 }}>
          Ask anything about this meeting. Answers cite the transcript, and each cited
          time jumps to that line.
        </p>
      )}

      {messages !== null && messages.length > 0 && (
        <div role="log" aria-live="polite" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {messages.map((message) => {
            const user = message.role === "user";
            return (
              <div
                key={message.id}
                style={{ display: "flex", justifyContent: user ? "flex-end" : "flex-start" }}
              >
                <div
                  className="kea-message"
                  style={{ maxWidth: "85%", background: user ? "var(--surface-2)" : "var(--surface)" }}
                >
                  <span className="kea-visually-hidden">{user ? "You: " : "Answer: "}</span>
                  {user ? (
                    <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                      {message.content}
                    </div>
                  ) : (
                    <ReplyText reply={message.content} durationMs={durationMs} onJumpTo={onJumpTo} />
                  )}
                  {!user && message.proposal && (
                    <ProposalCard
                      proposal={message.proposal}
                      current={notes}
                      recording={recording}
                      busy={proposalBusy === message.id}
                      onApply={() => void applyProposal(message)}
                      onDiscard={() => void discardProposal(message)}
                    />
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {pendingText !== null && (
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <div style={{ display: "flex", justifyContent: "flex-end" }}>
            <div className="kea-message" style={{ maxWidth: "85%", background: "var(--surface-2)" }}>
              <div style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>{pendingText}</div>
            </div>
          </div>
          <div style={{ display: "flex", justifyContent: "flex-start" }}>
            <div role="status" className="kea-message kea-muted" style={{ maxWidth: "85%" }}>
              Thinking…
            </div>
          </div>
        </div>
      )}

      <div ref={endRef} />

      {truncated && (
        <p className="kea-muted" role="note" style={{ margin: 0, fontSize: 12 }}>
          Answers are based on the first part of a long transcript.
        </p>
      )}

      {aiReady === null && (
        <p className="kea-muted" style={{ margin: 0 }}>
          Checking the AI setup…
        </p>
      )}

      {aiReady === false && (
        <div className="kea-card" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <p style={{ margin: 0 }}>Ask AI needs a language model for meetings.</p>
          {aiBlockedReason && (
            <p className="kea-muted" style={{ margin: 0, fontSize: 13 }}>
              {aiBlockedReason}
            </p>
          )}
          {onOpenModels && (
            <div>
              <button type="button" className="kea-btn" onClick={onOpenModels}>
                Open Models
              </button>
            </div>
          )}
        </div>
      )}

      {aiReady === true && (
        <>
          {recording && (
            <p className="kea-muted" style={{ margin: 0, fontSize: 13 }}>
              Editing is available when the meeting ends.
            </p>
          )}

          <div role="group" aria-label="Quick prompts" style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            {QUICK_PROMPTS.map((prompt) => (
              <button
                key={prompt.label}
                type="button"
                className="kea-btn"
                disabled={busy}
                onClick={() => void send(prompt.text, false)}
              >
                {prompt.label}
              </button>
            ))}
            <button
              type="button"
              className="kea-btn"
              disabled={busy}
              aria-expanded={translating}
              onClick={() => setTranslating((open) => !open)}
            >
              Translate
            </button>
          </div>

          {translating && (
            <div style={{ display: "flex", gap: 8 }}>
              <input
                className="kea-input"
                aria-label="Translate into which language"
                placeholder="Language, e.g. Hindi"
                value={language}
                disabled={busy}
                onChange={(e) => setLanguage(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") sendTranslate();
                }}
                style={{ flex: 1, minWidth: 0 }}
              />
              <button type="button" className="kea-btn" onClick={sendTranslate} disabled={busy || !language.trim()}>
                Send translation
              </button>
            </div>
          )}

          <div style={{ display: "flex", gap: 8, alignItems: "flex-end" }}>
            <textarea
              className="kea-input"
              aria-label="Ask about this meeting"
              placeholder="Ask about this meeting…"
              rows={2}
              value={draft}
              disabled={pendingText !== null || aiReady !== true}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={onInputKeyDown}
              style={{ flex: 1, minWidth: 0, resize: "vertical" }}
            />
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              disabled={pendingText !== null || aiReady !== true || !draft.trim()}
              onClick={() => void send(draft, true)}
            >
              Send
            </button>
          </div>
        </>
      )}

      {sendError && (
        <p role="alert" style={{ margin: 0, fontSize: 13, color: "var(--danger)" }}>
          {sendError}
        </p>
      )}
      {proposalError && (
        <p role="alert" style={{ margin: 0, fontSize: 13, color: "var(--danger)" }}>
          {proposalError}
        </p>
      )}

      {messages !== null && messages.length > 0 && (
        <div>
          {confirmingClear ? (
            <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
              <span style={{ fontSize: 13 }}>Delete this meeting&apos;s chat?</span>
              <button
                type="button"
                className="kea-btn"
                onClick={() => void clearChat()}
                style={{ color: "var(--danger)", borderColor: "var(--danger)" }}
              >
                Yes, clear chat
              </button>
              <button type="button" className="kea-btn" onClick={() => setConfirmingClear(false)}>
                Keep it
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="kea-btn"
              disabled={pendingText !== null}
              onClick={() => {
                setClearError(null);
                setConfirmingClear(true);
              }}
            >
              Clear chat
            </button>
          )}
          {clearError && (
            <p role="alert" style={{ margin: "6px 0 0", fontSize: 13, color: "var(--danger)" }}>
              {clearError}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
