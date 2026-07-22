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

const stateColors: Record<DictationState, { bg: string; fg: string }> = {
  idle: { bg: "var(--surface-2)", fg: "var(--text-muted)" },
  listening: { bg: "var(--surface-2)", fg: "var(--accent)" },
  processing: { bg: "var(--surface-2)", fg: "var(--accent)" },
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

  const { bg, fg } = stateColors[state];

  return (
    <div
      style={{
        position: "fixed",
        top: 16,
        right: 16,
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "8px 14px",
        borderRadius: 999,
        background: bg,
        color: fg,
        fontSize: 13,
        fontWeight: 500,
        border: "1px solid var(--border)",
        boxShadow: "0 2px 8px color-mix(in srgb, var(--text) 10%, transparent)",
        zIndex: 1000,
      }}
    >
      <span>{stateLabels[state]}</span>
      {state === "listening" && <LevelMeter level={level} />}
    </div>
  );
}
