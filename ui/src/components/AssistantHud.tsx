import { useEffect, useState } from "react";
import {
  getAssistantSettings,
  onAssistantAnswer,
  onAssistantPartial,
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
 * different label — and it is a different label because Escape genuinely does
 * a different thing here.
 *
 * `assistant_escape` in `src-tauri/src/hotkeys.rs` resolves the press against
 * whether an answer is playing: while one is, it silences the voice and leaves
 * the answer on screen and the session open; with nothing playing, it cancels.
 * So this line is not a euphemism for cancelling.
 *
 * A "Stop" button was the obvious alternative and is deliberately not here:
 * the overlay is `focusable(false)` and `set_ignore_cursor_events(true)`, so a
 * button drawn in it would render and never be clickable. Every control this
 * window can offer is a key, which is why all three of these are lines of text
 * rather than affordances.
 */
const STOP_SPEAKING_HINT = "Esc to stop speaking";

/**
 * The copy control, which is a key for the same reason the two above are.
 *
 * The overlay is `focusable(false)` and `set_ignore_cursor_events(true)` so
 * that opening a session cannot take the caret out of the application the user
 * is asking about — which means a button drawn here would render and never be
 * clickable. `Cmd+Shift+C` is registered around a session and released when it
 * ends (`set_assistant_copyable`), so this line is the only place it is
 * advertised; a user who never sees it has no way to discover it exists.
 *
 * Shown only alongside an answer, because that is exactly when the binding has
 * something to put on the clipboard — pressed at any other point in a session
 * it deliberately does nothing rather than clearing what the user had copied
 * for their own purposes.
 */
const COPY_HINT = "⌘⇧C to copy";

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
  /**
   * The live hypothesis for the request being spoken, or `null` when there is
   * none — which is both the resting state and the whole of the
   * no-streaming-model case. Never an empty string standing in for "none": the
   * surface reserves no room for a line it is not showing, and `""` would
   * reserve it.
   */
  const [partial, setPartial] = useState<string | null>(null);
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
          // And the previous hypothesis with it. Cleared on entering
          // `listening` rather than on leaving it, so the recognised text stays
          // up through `processing` — that is the window in which the user
          // checks whether they were heard, and blanking it there would take
          // the evidence away at the moment it is wanted.
          setPartial(null);
          // The switches are edited in the settings window, which cannot reach
          // this one. Re-reading at the top of every turn is what stops a
          // change made a minute ago from taking effect a restart later.
          loadOutputs();
        }
      }),
      onAssistantAnswer(setAnswer),
      onAssistantPartial(({ text }) => setPartial(text)),
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

      {/* What the recogniser has made of the request so far, while the user
          is still speaking it. Absent for the whole of a session running
          without a streaming model, which is the ordinary case on a machine
          that never downloaded one — the label above still says the assistant
          is listening, which is what the spec asks for when live recognition
          cannot produce text.

          Dropped once the answer arrives rather than shown beside it: the
          answer payload carries the request as the *offline* transcript heard
          it, and two versions of the same question on screen at once invites
          the user to wonder which one was answered. */}
      {partial && !answer?.request && (
        <div className="kea-hud__heard" title={partial}>
          {partial}
        </div>
      )}

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

      {/* What separates an answered request from an actioned one. Without it
          the two are the same pill with different prose, and the user has no
          way to tell that a question was quietly carried out rather than
          replied to — the routing-visibility requirement asks for the selected
          action, not just the text it produced.

          It survives `show_answers` being off for the same reason the request
          echo does: the switch is a preference about reading replies, not
          consent to have things done unseen. Deliberately a small muted line
          under the request rather than a heading: the answer is what the user
          came for, and an action label loud enough to compete with it would
          make every actioned turn look like an error. */}
      {answer?.action && (
        <div className="kea-hud__action">{answer.action}</div>
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

      {/* One Escape hint at a time, and stopping wins: while an answer is
          playing, what the user most likely wants from Escape is silence.

          The copy hint sits beside it rather than competing for the slot,
          because it names a different key and is true whenever an answer is on
          screen — including while that answer is still being read aloud. */}
      <div className="kea-hud__hints">
        {speaking ? (
          <span className="kea-hud__hint">{STOP_SPEAKING_HINT}</span>
        ) : (
          working && <span className="kea-hud__hint">{CANCEL_HINT}</span>
        )}
        {outputs.show_answers && answer?.text && (
          <span className="kea-hud__hint">{COPY_HINT}</span>
        )}
      </div>
    </div>
  );
}
