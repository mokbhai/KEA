import { useCallback, useEffect, useState } from "react";
import { getBinding, triggerRewrite } from "../api";
import HotkeyBinder from "../components/HotkeyBinder";
import SettingsForm, { type RewriteSettings } from "../components/SettingsForm";
import SlotBinder from "../components/SlotBinder";
import Spinner from "../components/Spinner";

const REWRITE_FEATURE = "rewrite";
const REWRITE_SLOT = "llm";
const REWRITE_COMMAND = "rewrite_selection";

const settingsSummaryStyle: React.CSSProperties = {
  cursor: "pointer",
  color: "var(--text)",
  fontWeight: 600,
  fontSize: "0.875rem",
};

type Props = {
  onRunSetup?: () => void;
};

export default function RewritePage({ onRunSetup }: Props) {
  const [settings, setSettings] = useState<RewriteSettings>({
    mode: "improve",
    preset_id: null,
    custom_instruction: "",
  });
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [hasBinding, setHasBinding] = useState<boolean | null>(null);

  const onSettingsChange = useCallback((next: RewriteSettings) => {
    setSettings(next);
  }, []);

  useEffect(() => {
    getBinding("rewrite", "llm")
      .then((b) => setHasBinding(b !== null))
      .catch(() => setHasBinding(false));
  }, []);

  const runRewrite = async () => {
    setBusy(true);
    setRunStatus(null);
    try {
      const result = await triggerRewrite(
        settings.mode,
        settings.preset_id,
        settings.mode === "ask_kea" ? settings.custom_instruction || null : null,
      );
      setRunStatus(result ? `Done: ${result.slice(0, 80)}…` : "Rewrite completed.");
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
        <p className="kea-muted" style={{ marginTop: 0 }}>
          Bind an LLM engine, configure rewrite mode and presets, and set the
          global hotkey for in-place selection rewrite.
        </p>
      </header>

      {hasBinding === false && onRunSetup && (
        <section
          className="kea-card"
          style={{
            marginBottom: 24,
            background: "var(--surface-2)",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            flexWrap: "wrap",
            gap: 12,
          }}
        >
          <span className="kea-muted" style={{ fontSize: "0.875rem" }}>
            Rewrite isn't configured yet — run setup to bind an engine.
          </span>
          <button type="button" className="kea-btn kea-btn--primary" onClick={onRunSetup}>
            Run setup
          </button>
        </section>
      )}

      <section style={{ marginBottom: 24 }}>
        <button
          type="button"
          className="kea-btn kea-btn--primary"
          onClick={runRewrite}
          disabled={busy}
        >
          {busy ? (
            <>
              <Spinner size={14} /> Processing…
            </>
          ) : (
            "Run rewrite"
          )}
        </button>
        {runStatus && (
          <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
            {runStatus}
          </p>
        )}
      </section>

      <details open className="kea-card" style={{ marginBottom: 16 }}>
        <summary className="kea-label" style={settingsSummaryStyle}>
          Settings
        </summary>
        <div style={{ marginTop: 16 }}>
          <SlotBinder feature={REWRITE_FEATURE} slot={REWRITE_SLOT} />
          <HotkeyBinder
            feature={REWRITE_FEATURE}
            command={REWRITE_COMMAND}
            label="Rewrite hotkey"
          />
          <SettingsForm onChange={onSettingsChange} />
        </div>
      </details>
    </div>
  );
}
