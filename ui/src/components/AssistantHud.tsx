import { useEffect, useState } from "react";
import {
  getAssistantSettings,
  onAssistantAnswer,
  onAssistantState,
  type AssistantAnswer,
  type AssistantSettings,
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
 * The stop control for a spoken answer, which is the same key wearing a
 * different label.
 *
 * A "Stop" button was the obvious alternative and is deliberately not here:
 * there is no stop-playback call at the Tauri boundary, so the only thing this
 * window can reach is the Escape binding the session holds while it is open.
 * A button wired to that would read as "stop the voice" and in fact end the
 * session, taking the answer off screen with it — a control that does more
 * than it says is worse than a line naming the key that already works. When a
 * stop-only command exists this becomes a button and this comment goes away.
 */
const STOP_SPEAKING_HINT = "Esc to stop speaking";

/**
 * What the surface assumes about the two output switches until the real
 * settings arrive, which is what the backend assumes when no row was ever
 * written (`kea_core::assistant::settings`).
 *
 * Rendering nothing until the fetch lands was the alternative, and it would
 * have cost the one thing this window exists to do: appear the instant the
 * hotkey is pressed, before the user has finished drawing breath. Guessing
 * wrong is cheap here because the first thing a session shows is "Listening…",
 * and an answer is seconds of speech and a model round trip away.
 */
const BOTH_ON: AssistantSettings = { speak_answers: true, show_answers: true };

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
  const [outputs, setOutputs] = useState<AssistantSettings>(BOTH_ON);

  useEffect(() => {
    let live = true;
    // A failed read keeps the defaults rather than reporting: the switches are
    // not what this window is for, and an error line in place of an answer
    // would be a worse outcome than showing one the user asked to hide.
    const loadOutputs = () => {
      getAssistantSettings()
        .then((next) => {
          if (live) setOutputs(next);
        })
        .catch(() => {});
    };

    loadOutputs();
    const subs = [
      onAssistantState((next) => {
        setStatus(next);
        if (next.state === "listening") {
          // A new request clears the previous answer, so the panel never shows
          // an old answer beside a new question.
          setAnswer(null);
          // The switches are edited in the settings window, which cannot reach
          // this one. Re-reading at the top of every turn is what stops a
          // change made a minute ago from taking effect a restart later.
          loadOutputs();
        }
      }),
      onAssistantAnswer(setAnswer),
    ];
    return () => {
      live = false;
      subs.forEach((s) => void s.then((un) => un()));
    };
  }, []);

  if (!status) return null;

  const label =
    status.state === "failed"
      ? (status.message ?? "Something went wrong.")
      : stateLabels[status.state];

  // Gated on the switch as well as the flag. The session that emitted this
  // state resolved the switches when it started, so a switch turned off while
  // one was open is the case where the two disagree — and the user's latest
  // answer to "should this thing speak" is the one to honour.
  const speaking = Boolean(status.speaking) && outputs.speak_answers;
  const working = status.state === "listening" || status.state === "processing";

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
          question is otherwise only inferable from a strange reply. It survives
          `show_answers` being off because it is not the answer — it is the
          record of what was heard, which the routing-visibility requirement
          asks for whether or not the reply is on screen. */}
      {answer?.request && (
        <div className="kea-hud__request" title={answer.request}>
          “{answer.request}”
        </div>
      )}

      {outputs.show_answers && answer?.text && (
        <div className="kea-hud__answer">{answer.text}</div>
      )}

      {/* Disclosure sits with the answer rather than in a log: the requirement
          is that the *user* is told what was read and where it went. It, too,
          outlives `show_answers`: turning the reply off is a preference about
          reading, not consent to stop being told what left the machine. */}
      {answer?.read && (
        <div className="kea-hud__disclosure">
          Read {answer.read}
          {answer.sent_externally ? " · sent to your AI provider" : ""}
        </div>
      )}

      {/* One hint at a time, and stopping wins: while an answer is playing,
          what the user most likely wants from Escape is silence. */}
      {speaking ? (
        <div className="kea-hud__hint">{STOP_SPEAKING_HINT}</div>
      ) : (
        working && <div className="kea-hud__hint">{CANCEL_HINT}</div>
      )}
    </div>
  );
}
