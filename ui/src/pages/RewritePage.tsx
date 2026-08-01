import { useCallback, useState } from "react";
import { previewRewrite } from "../api";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner, { type FixNavigate } from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import SettingsForm, { type RewriteSettings } from "../components/SettingsForm";
import Spinner from "../components/Spinner";
import { useFeatureAi } from "../hooks/useFeatureAi";
import type { SlotSpec } from "../lib/featureSlot";

const SAMPLE = "i think we should probaly ship this on friday, lmk what u think";

const SLOTS: SlotSpec[] = [
  { feature: "rewrite", slot: "llm", capability: "llm", label: "Writing & rewriting" },
];

type Props = {
  onRunSetup?: () => void;
  onNavigate?: FixNavigate;
};

export default function RewritePage({ onRunSetup, onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const [settings, setSettings] = useState<RewriteSettings>({
    mode: "improve",
    preset_id: null,
    custom_instruction: "",
  });
  const [sample, setSample] = useState(SAMPLE);
  const [result, setResult] = useState<string | null>(null);
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const onSettingsChange = useCallback((next: RewriteSettings) => {
    setSettings(next);
  }, []);

  const runSample = async () => {
    setBusy(true);
    setRunStatus(null);
    setResult(null);
    try {
      const text = await previewRewrite(
        sample,
        settings.mode,
        settings.preset_id,
        settings.mode === "ask_kea" ? settings.custom_instruction || null : null,
      );
      setResult(text);
    } catch (e) {
      setRunStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h2 style={{ marginTop: 0 }}>Rewrite</h2>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Select text anywhere on your Mac and press the shortcut to rewrite it in
          place.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      {onRunSetup && ai.statuses?.some((s) => s.blocked) && (
        <div style={{ marginBottom: 16 }}>
          <button type="button" className="kea-btn" onClick={onRunSetup}>
            Run setup again
          </button>
        </div>
      )}

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Behavior</h3>
        <SettingsForm
          onChange={onSettingsChange}
          leadingRows={
            <HotkeyRow
              feature="rewrite"
              command="rewrite_selection"
              label="Shortcut"
              hint="Rewrites the text you have selected."
              checkRegistration
            />
          }
        />
      </section>

      <FeatureAiCard ai={ai} featureLabel="Rewrite" />

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Try it</h3>
        <div className="kea-card">
          <label style={{ display: "block", marginBottom: 12 }}>
            <span className="kea-label">Sample text</span>
            <textarea
              className="kea-input"
              aria-label="Sample text"
              value={sample}
              onChange={(e) => setSample(e.target.value)}
              rows={3}
              style={{ width: "100%", maxWidth: 520, resize: "vertical" }}
            />
          </label>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={() => void runSample()}
            disabled={busy || !sample.trim()}
          >
            {busy ? (
              <>
                <Spinner size={14} /> Rewriting…
              </>
            ) : (
              "Rewrite this"
            )}
          </button>
          <p className="kea-muted" style={{ margin: "8px 0 0", fontSize: "0.8125rem" }}>
            Runs the same rewrite your shortcut does, but only shows the result
            here — nothing is pasted anywhere.
          </p>
          {result !== null && (
            <div style={{ marginTop: 12 }}>
              <span className="kea-label">Result</span>
              <p
                style={{
                  margin: "4px 0 0",
                  padding: 12,
                  background: "var(--surface-2)",
                  border: "1px solid var(--border)",
                  borderRadius: 6,
                  whiteSpace: "pre-wrap",
                }}
              >
                {result}
              </p>
            </div>
          )}
          {runStatus && (
            <p style={{ marginTop: 12, marginBottom: 0, fontSize: "0.8125rem", color: "var(--danger)" }}>
              {runStatus}
            </p>
          )}
        </div>
      </section>
    </div>
  );
}
