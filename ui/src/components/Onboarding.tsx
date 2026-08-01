import { useCallback, useRef, useState } from "react";
import Banner from "./Banner";
import ConnectStep, { type AiChoice } from "./onboarding/ConnectStep";
import HotkeysStep from "./onboarding/HotkeysStep";
import PermissionsStep from "./onboarding/PermissionsStep";
import VoiceStep from "./onboarding/VoiceStep";

type Props = {
  onFinish: () => void;
};

const STEPS = ["Permissions", "Connect your AI", "Voice", "Hotkeys"] as const;

function StepIndicator({ current, total }: { current: number; total: number }) {
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
  return (
    <div
      style={{ display: "flex", justifyContent: "center", marginBottom: 20 }}
      aria-hidden="true"
    >
      {dots}
    </div>
  );
}

export default function Onboarding({ onFinish }: Props) {
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [aiChoice, setAiChoice] = useState<AiChoice | null>(null);
  const total = STEPS.length;
  const isLast = step === total - 1;

  // The active step registers what should happen when the user hits Continue
  // (writing bindings, saving keys, …). Skip never runs it.
  const commitRef = useRef<(() => Promise<void>) | null>(null);
  const setCommit = useCallback((fn: (() => Promise<void>) | null) => {
    commitRef.current = fn;
  }, []);

  const advance = () => {
    setError(null);
    if (isLast) {
      onFinish();
    } else {
      setStep((s) => Math.min(total - 1, s + 1));
    }
  };

  const continueStep = async () => {
    const commit = commitRef.current;
    if (!commit) {
      advance();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await commit();
      advance();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const skip = () => advance();

  const back = () => {
    setError(null);
    setStep((s) => Math.max(0, s - 1));
  };

  return (
    <div style={{ maxWidth: 640, margin: "40px auto", padding: "0 16px" }}>
      <h1 style={{ marginTop: 0, textAlign: "center", marginBottom: 24 }}>Welcome to KEA</h1>
      <p className="kea-muted" style={{ textAlign: "center", marginBottom: 8 }}>
        Step {step + 1} of {total}: {STEPS[step]}
      </p>
      <StepIndicator current={step} total={total} />

      {step === 0 && <PermissionsStep />}
      {step === 1 && <ConnectStep setCommit={setCommit} onCommitted={setAiChoice} />}
      {step === 2 && <VoiceStep setCommit={setCommit} aiChoice={aiChoice} />}
      {step === 3 && <HotkeysStep />}

      {error && <Banner variant="error">{error}</Banner>}

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
            <button type="button" className="kea-btn" onClick={back} disabled={busy}>
              Back
            </button>
          )}
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="kea-btn" onClick={skip} disabled={busy}>
            {isLast ? "Skip and finish" : "Skip for now"}
          </button>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={() => void continueStep()}
            disabled={busy}
          >
            {busy ? "Working…" : isLast ? "Finish" : "Continue"}
          </button>
        </div>
      </div>
    </div>
  );
}
