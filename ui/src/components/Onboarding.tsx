import { useEffect, useState } from "react";
import { getEffectiveHotkey, type EffectiveHotkey } from "../api";
import EngineConfig from "../components/EngineConfig";
import ModelManager from "../components/ModelManager";
import PermissionPanel from "../components/PermissionPanel";
import SlotBinder from "../components/SlotBinder";

type Props = {
  onFinish: () => void;
};

const STEPS = ["Permissions", "Provider & engines", "Speech engines", "Hotkeys"] as const;

const FEATURE_HOTKEYS = [
  { feature: "rewrite", command: "rewrite_selection", label: "Rewrite" },
  { feature: "dictation", command: "push_to_talk", label: "Dictation" },
  { feature: "meetings", command: "toggle_meeting", label: "Meetings" },
  { feature: "tts", command: "read_selection", label: "Read-aloud" },
] as const;

function StepIndicator({
  current,
  total,
}: {
  current: number;
  total: number;
}) {
  const dots = [];
  for (let i = 0; i < total; i++) {
    dots.push(
      <span
        key={i}
        style={{
          display: "inline-block",
          width: 10,
          height: 10,
          borderRadius: "50%",
          margin: "0 4px",
          background: i <= current ? "var(--accent)" : "var(--border)",
          transition: "background 0.2s ease",
        }}
      />,
    );
  }
  return <div style={{ display: "flex", justifyContent: "center", marginBottom: 20 }}>{dots}</div>;
}

function HotkeyRow({ feature, command, label }: { feature: string; command: string; label: string }) {
  const [hk, setHk] = useState<EffectiveHotkey | null | undefined>(undefined);

  useEffect(() => {
    getEffectiveHotkey(feature, command)
      .then(setHk)
      .catch(() => setHk(null));
  }, [feature, command]);

  return (
    <div
      style={{
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        padding: "8px 0",
        borderBottom: "1px solid var(--border)",
      }}
    >
      <span style={{ fontWeight: 500 }}>{label}</span>
      <span className="kea-muted" style={{ fontFamily: "monospace", fontSize: "0.8125rem" }}>
        {hk === undefined
          ? "…"
          : hk
            ? `${hk.accelerator}${hk.source === "default" ? " (default)" : ""}`
            : "Not set"}
      </span>
    </div>
  );
}

function HotkeysStep() {
  return (
    <div>
      <h3 style={{ margin: "0 0 16px" }}>Hotkeys</h3>
      <p className="kea-muted" style={{ marginBottom: 16 }}>
        Each feature has a compiled-in default hotkey. You can customize these per
        feature on its page after setup.
      </p>
      <section className="kea-card" style={{ marginBottom: 16 }}>
        {FEATURE_HOTKEYS.map((f) => (
          <HotkeyRow key={`${f.feature}/${f.command}`} feature={f.feature} command={f.command} label={f.label} />
        ))}
      </section>
    </div>
  );
}

function ProviderStep() {
  return (
    <div>
      <h3 style={{ margin: "0 0 16px" }}>Provider & engines</h3>
      <p className="kea-muted" style={{ marginBottom: 16 }}>
        Configure your OpenAI provider and bind an LLM engine to the Rewrite
        feature — the minimum to make one feature work. Other features bind
        per-page.
      </p>
      <EngineConfig provider_ref="openai" title="OpenAI" />
      <SlotBinder feature="rewrite" slot="llm" title="Rewrite LLM binding" />
    </div>
  );
}

function SpeechStep() {
  return (
    <div>
      <h3 style={{ margin: "0 0 16px" }}>Speech engines</h3>
      <p className="kea-muted" style={{ marginBottom: 16 }}>
        Choose engines for speech-to-text (dictation) and text-to-speech
        (read-aloud). Local engines need a model downloaded first.
      </p>
      <section className="kea-card" style={{ marginBottom: 16 }}>
        <h4 style={{ margin: "0 0 12px" }}>Speech-to-text (Dictation)</h4>
        <SlotBinder feature="dictation" slot="stt" slotKind="stt" title="Dictation STT engine" />
        <ModelManager kind="whisper" />
        <ModelManager kind="parakeet" />
      </section>
      <section className="kea-card" style={{ marginBottom: 16 }}>
        <h4 style={{ margin: "0 0 12px" }}>Text-to-speech (Read-aloud)</h4>
        <SlotBinder feature="tts" slot="tts" slotKind="tts" title="Read-aloud TTS engine" />
        <ModelManager kind="tts" />
      </section>
    </div>
  );
}

export default function Onboarding({ onFinish }: Props) {
  const [step, setStep] = useState(0);
  const total = STEPS.length;

  const next = () => {
    if (step < total - 1) {
      setStep((s) => s + 1);
    } else {
      onFinish();
    }
  };

  const prev = () => setStep((s) => Math.max(0, s - 1));

  return (
    <div
      style={{
        maxWidth: 640,
        margin: "40px auto",
        padding: "0 16px",
      }}
    >
      <h2 style={{ marginTop: 0, textAlign: "center", marginBottom: 24 }}>Welcome to KEA</h2>
      <p className="kea-muted" style={{ textAlign: "center", marginBottom: 8 }}>
        Step {step + 1} of {total}: {STEPS[step]}
      </p>
      <StepIndicator current={step} total={total} />

      {step === 0 && <PermissionPanel />}
      {step === 1 && <ProviderStep />}
      {step === 2 && <SpeechStep />}
      {step === 3 && <HotkeysStep />}

      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          marginTop: 24,
          paddingTop: 16,
          borderTop: "1px solid var(--border)",
        }}
      >
        <div>
          {step > 0 && (
            <button type="button" className="kea-btn" onClick={prev}>
              Back
            </button>
          )}
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="kea-btn" onClick={onFinish}>
            Skip
          </button>
          <button type="button" className="kea-btn kea-btn--primary" onClick={next}>
            {step < total - 1 ? "Next" : "Finish"}
          </button>
        </div>
      </div>
    </div>
  );
}
