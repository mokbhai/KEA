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
  processing: "Transcribing…",
};

/* The state used to be carried by text colour alone; a dot carries it for
   anyone who can't tell the two colours apart, and the label carries it for
   anyone who can't see either. */
const stateDots: Record<DictationState, string> = {
  idle: "kea-dot--muted",
  listening: "kea-dot--ok",
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
  const reducedMotion = usePrefersReducedMotion();

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
    if (state !== "listening") setHistory(emptyLevelHistory());
  }, [state]);

  if (state === "idle") return null;

  return (
    <div className={`kea-hud kea-hud--${state}`}>
      {/* Only the label sits in the live region: a meter that updates 20x a
          second inside one would be announced 20x a second. */}
      <div className="kea-hud__head" role="status">
        <span className={`kea-dot ${stateDots[state]}`} aria-hidden="true" />
        <span>{stateLabels[state]}</span>
      </div>
      {state === "listening" ? (
        reducedMotion ? (
          <LevelMeter level={history[history.length - 1] ?? 0} />
        ) : (
          <Waveform history={history} />
        )
      ) : (
        <TranscribingIndicator reducedMotion={reducedMotion} />
      )}
    </div>
  );
}
