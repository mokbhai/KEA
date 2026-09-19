import { useEffect, useState } from "react";
import { onDictationLevel, onDictationState, type DictationState } from "../api";
import { usePrefersReducedMotion } from "../hooks/usePrefersReducedMotion";
import {
  barScale,
  emptyLevelHistory,
  pushLevel,
  WAVEFORM_BARS,
} from "../lib/waveform";
import LevelMeter from "./LevelMeter";

const stateLabels: Record<DictationState, string> = {
  idle: "Idle",
  listening: "Listening…",
  locked: "Locked",
  processing: "Transcribing…",
};

/**
 * A locked recording has no key holding it open, so the only thing standing
 * between it and an open microphone nobody remembers is this line. It says
 * both ways out, because the gesture that ends it is not the one that started
 * it.
 */
const LOCKED_HINT = "Tap ⌥⇧ to finish · Esc to cancel";

/** m:ss, which is all a five-minute cap ever needs. */
function formatElapsed(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}:${String(secs).padStart(2, "0")}`;
}

/* The state used to be carried by text colour alone; a dot carries it for
   anyone who can't tell the two colours apart, and the label carries it for
   anyone who can't see either. */
const stateDots: Record<DictationState, string> = {
  idle: "kea-dot--muted",
  listening: "kea-dot--ok",
  // Warn rather than ok: this one keeps recording after the keys come up, and
  // the dot is the fastest way to tell the two apart across the room.
  locked: "kea-dot--warn",
  processing: "kea-dot--accent",
};

function Waveform({ history }: { history: readonly number[] }) {
  const latest = history[history.length - 1] ?? 0;

  return (
    <div
      className="kea-waveform"
      role="meter"
      aria-label="Microphone level"
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={latest}
    >
      {history.map((level, index) => (
        <span
          // Bars are positions in a fixed-length window, not identities: the
          // values shift left through them, which is what makes the row read
          // as scrolling rather than as bars being inserted and removed.
          key={index}
          className="kea-waveform__bar"
          aria-hidden="true"
          style={{ transform: `scaleY(${barScale(level)})` }}
        />
      ))}
    </div>
  );
}

/**
 * Recording has already stopped by the time transcription starts, so there are
 * no levels to show — this is the phase that otherwise gives no feedback at
 * all. An indeterminate shimmer says "working" without implying progress.
 */
function TranscribingIndicator({ reducedMotion }: { reducedMotion: boolean }) {
  return (
    <div
      className={`kea-shimmer${reducedMotion ? " kea-shimmer--static" : ""}`}
      role="progressbar"
      aria-label="Transcribing"
    >
      <span className="kea-shimmer__sweep" aria-hidden="true" />
    </div>
  );
}

/**
 * The dictation heads-up display: state, live waveform and a transcribing
 * indicator. Rendered as the whole document of the floating overlay window,
 * which the Rust side shows and hides with the dictation state — so `idle`
 * renders nothing rather than an "Idle" chip nobody asked for.
 */
export default function DictationHud() {
  const [state, setState] = useState<DictationState>("idle");
  const [history, setHistory] = useState<number[]>(() => emptyLevelHistory());
  const [elapsed, setElapsed] = useState(0);
  const reducedMotion = usePrefersReducedMotion();
  const recording = state === "listening" || state === "locked";

  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState(setState),
      onDictationLevel((level) =>
        setHistory((previous) => pushLevel(previous, level, WAVEFORM_BARS)),
      ),
    ]);

    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    // A run that ended must not leave its last waveform frozen on screen for
    // the next one to start from.
    if (!recording) setHistory(emptyLevelHistory());
  }, [recording]);

  useEffect(() => {
    // Only the locked mode counts: a held recording lasts as long as the keys
    // are down, and the user can feel that. A locked one has to be told.
    if (state !== "locked") {
      setElapsed(0);
      return;
    }
    const started = Date.now();
    const timer = setInterval(
      () => setElapsed(Math.floor((Date.now() - started) / 1000)),
      1000,
    );
    return () => clearInterval(timer);
  }, [state]);

  if (state === "idle") return null;

  return (
    <div className={`kea-hud kea-hud--${state}`}>
      {/* Only the label sits in the live region: a meter that updates 20x a
          second inside one would be announced 20x a second. */}
      <div className="kea-hud__head" role="status">
        <span className={`kea-dot ${stateDots[state]}`} aria-hidden="true" />
        <span>
          {stateLabels[state]}
          {state === "locked" && ` · ${formatElapsed(elapsed)}`}
        </span>
      </div>
      {recording ? (
        reducedMotion ? (
          <LevelMeter level={history[history.length - 1] ?? 0} />
        ) : (
          <Waveform history={history} />
        )
      ) : (
        <TranscribingIndicator reducedMotion={reducedMotion} />
      )}
      {state === "locked" && (
        <span
          className="kea-muted"
          style={{ fontSize: "0.75rem", whiteSpace: "nowrap" }}
        >
          {LOCKED_HINT}
        </span>
      )}
    </div>
  );
}
