import { useEffect, useState } from "react";
import {
  onDictationLevel,
  onDictationState,
  type DictationState,
} from "../api";
import LevelMeter from "./LevelMeter";

const stateLabels: Record<DictationState, string> = {
  idle: "Idle",
  listening: "Listening…",
  processing: "Processing…",
};

const stateDots: Record<DictationState, string> = {
  idle: "kea-dot--muted",
  listening: "kea-dot--ok",
  processing: "kea-dot--ok",
};

export default function SpeechOverlay() {
  const [state, setState] = useState<DictationState>("idle");
  const [level, setLevel] = useState(0);

  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState(setState),
      onDictationLevel(setLevel),
    ]);

    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    if (state !== "listening") {
      setLevel(0);
    }
  }, [state]);

  return (
    <div className={`kea-float kea-speech-overlay kea-speech-overlay--${state}`}>
      {/* The state used to be carried by text colour alone; a dot carries it
          for anyone who can't tell the two colours apart. */}
      <span className={`kea-dot ${stateDots[state]}`} aria-hidden="true" />
      <span>{stateLabels[state]}</span>
      {state === "listening" && <LevelMeter level={level} />}
    </div>
  );
}
