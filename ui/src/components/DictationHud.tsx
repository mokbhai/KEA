import { useEffect, useState } from "react";
import {
  onDictationLevel,
  onDictationPartial,
  onDictationState,
  type DictationState,
} from "../api";
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

/**
 * How wide the transcript line is allowed to be.
 *
 * **Must agree with `PARTIAL_WIDTH` in `src-tauri/src/overlay.rs`**, which is
 * what makes the overlay window wide enough to hold it. There is no seam where
 * Rust and CSS meet, so the cross-reference in both files is the only defence.
 */
const PARTIAL_WIDTH_PX = 220;

type PartialState = { seq: number; text: string; stableChars: number | null };

/**
 * The live transcript, settled words in `--text` and the still-moving tail in
 * `--text-muted`.
 *
 * Two things here are load bearing and easy to undo by accident:
 *
 * - `aria-hidden`. A live region fed a self-revising string ten times a second
 *   is unusable and no `aria-live` politeness setting fixes it, so the head's
 *   `role="status"` stays the only announced element. A screen-reader user
 *   gets the transcript from the application it was typed into, which is the
 *   authoritative copy; this line is a sighted-user affordance.
 * - The tail is dimmed with the `--text-muted` *token*, never with `opacity`.
 *   The contrast guard in `ui/src/palette.contrast.test.ts` composites only
 *   declared alpha, so an `opacity` here would drop the real ratio below 4.5:1
 *   with the test still green.
 *
 * Splitting is by `Array.from`, never `slice`: `stableChars` counts Unicode
 * scalar values, and a byte or UTF-16 offset would cut an emoji or a CJK
 * character in half and render a replacement character. Invisible in
 * English-only testing, which is why it is spelled out.
 */
function PartialLine({ partial }: { partial: PartialState | null }) {
  const scalars = partial ? Array.from(partial.text) : [];
  // `null` means the engine does not report stability, so nothing is claimed
  // to be still moving.
  const stable = partial?.stableChars ?? scalars.length;

  return (
    <div
      className="kea-hud__partial"
      aria-hidden="true"
      style={{
        display: "flex",
        // The newest words are pinned to the right, so overflow spills off the
        // start edge and is clipped there while the text itself stays LTR.
        // `direction: rtl` would scroll the same way and mangle trailing
        // punctuation.
        justifyContent: "flex-end",
        // No partial, no space reserved: this is the item-9-is-missing state
        // and the initial one.
        width: partial ? PARTIAL_WIDTH_PX : 0,
        overflow: "hidden",
        whiteSpace: "nowrap",
        // Exactly one width change per run, 0 → 220px when the first partial
        // arrives, not one per partial. The global reduced-motion rule in
        // index.css zeroes this transition.
        transition: "width 140ms ease-out",
        maskImage: "linear-gradient(to right, transparent 0, #000 24px)",
        WebkitMaskImage: "linear-gradient(to right, transparent 0, #000 24px)",
      }}
    >
      <span className="kea-hud__partial-stable" style={{ color: "var(--text)" }}>
        {scalars.slice(0, stable).join("")}
      </span>
      <span className="kea-hud__partial-tail" style={{ color: "var(--text-muted)" }}>
        {scalars.slice(stable).join("")}
      </span>
    </div>
  );
}

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
  // `null` is the initial state *and* the whole absence path: a build with no
  // streaming model selected, or one whose model is not installed, never
  // receives a partial and renders exactly the HUD that shipped before them.
  const [partial, setPartial] = useState<PartialState | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const reducedMotion = usePrefersReducedMotion();
  const recording = state === "listening" || state === "locked";

  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState(setState),
      onDictationLevel((level) =>
        setHistory((previous) => pushLevel(previous, level, WAVEFORM_BARS)),
      ),
      onDictationPartial((incoming) =>
        setPartial((previous) =>
          // A streaming hypothesis grows *and revises its tail*, so each
          // payload carries the whole display string and this assigns rather
          // than appends. Reconciling deltas would be the engine's job, not
          // the HUD's. Ignoring a lower seq is what makes the Rust throttle's
          // drop-and-coalesce safe and turns a straggler arriving after
          // `processing` into a no-op rather than a flicker.
          previous && incoming.seq <= previous.seq
            ? previous
            : {
                seq: incoming.seq,
                text: incoming.text,
                stableChars: incoming.stable_chars,
              },
        ),
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
    // The partial has a different lifetime from the waveform. It must
    // *survive* listening → processing, because that is exactly the window in
    // which the user has stopped talking and wants to see what was heard — and
    // in which the final partial, the text actually typed, arrives. So it is
    // cleared on idle (the run is over) and on entry to listening (a new run
    // must not inherit the previous one's words).
    if (state === "idle" || state === "listening") setPartial(null);
  }, [state]);

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
    <div
      className={`kea-hud kea-hud--${state}${partial ? " kea-hud--has-partial" : ""}`}
    >
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
      <PartialLine partial={partial} />
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
