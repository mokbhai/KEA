import { useEffect, useState } from "react";
import {
  onAssistantAnswer,
  onAssistantState,
  type AssistantAnswer,
  type AssistantState,
} from "../api";

const stateLabels: Record<AssistantState, string> = {
  listening: "Listening…",
  processing: "Thinking…",
  presenting: "",
  failed: "",
};

/**
 * The hint is the only place Escape is advertised.
 *
 * It is registered around a running session and released the moment one ends,
 * so a user who has never seen this line has no way to discover that the key
 * does anything at all right now.
 */
const CANCEL_HINT = "Esc to cancel";

/**
 * The assistant's half of the floating overlay.
 *
 * Renders `null` whenever no session is open, which is the initial state and
 * the resting one — the window itself is shown and hidden from the Rust side,
 * and this keeps the pill out of the layout when the HUD is showing dictation
 * instead.
 */
export default function AssistantHud() {
  const [status, setStatus] = useState<{
    state: AssistantState;
    message?: string;
    speaking?: boolean;
  } | null>(null);
  const [answer, setAnswer] = useState<AssistantAnswer | null>(null);

  useEffect(() => {
    const subs = [
      onAssistantState((next) => {
        setStatus(next);
        // A new request clears the previous answer, so the panel never shows
        // an old answer beside a new question.
        if (next.state === "listening") setAnswer(null);
      }),
      onAssistantAnswer(setAnswer),
    ];
    return () => {
      subs.forEach((s) => void s.then((un) => un()));
    };
  }, []);

  if (!status) return null;

  const label =
    status.state === "failed"
      ? (status.message ?? "Something went wrong.")
      : stateLabels[status.state];

  return (
    <div className="kea-hud kea-hud--assistant" role="status" aria-live="polite">
      <div className="kea-hud__head">
        <span
          className="kea-hud__dot"
          data-state={status.state}
          aria-hidden="true"
        />
        {label && <span className="kea-hud__label">{label}</span>}
      </div>

      {/* The request is shown before and alongside the answer: a misheard
          question is otherwise only inferable from a strange reply. */}
      {answer?.request && (
        <div className="kea-hud__request" title={answer.request}>
          “{answer.request}”
        </div>
      )}

      {answer?.text && <div className="kea-hud__answer">{answer.text}</div>}

      {/* Disclosure sits with the answer rather than in a log: the requirement
          is that the *user* is told what was read and where it went. */}
      {answer?.read && (
        <div className="kea-hud__disclosure">
          Read {answer.read}
          {answer.sent_externally ? " · sent to your AI provider" : ""}
        </div>
      )}

      {(status.state === "listening" ||
        status.state === "processing" ||
        status.speaking) && (
        <div className="kea-hud__hint">{CANCEL_HINT}</div>
      )}
    </div>
  );
}
