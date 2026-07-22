import { useEffect, useState } from "react";
import {
  getTtsSettings,
  runReadAloud,
  setTtsSettings,
  triggerTts,
  type TtsSettings,
} from "../api";
import HotkeyBinder from "../components/HotkeyBinder";
import SlotBinder from "../components/SlotBinder";
import Spinner from "../components/Spinner";

const TTS_FEATURE = "tts";
const TTS_SLOT = "tts";
const TTS_COMMAND = "read_selection";

const OPENAI_VOICES = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"] as const;

const settingsSummaryStyle: React.CSSProperties = {
  cursor: "pointer",
  color: "var(--text)",
  fontWeight: 600,
  fontSize: "0.875rem",
};

export default function ReadAloudPage() {
  const [settings, setSettings] = useState<TtsSettings>({
    active_voice: null,
    active_model: null,
  });
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    getTtsSettings()
      .then(setSettings)
      .catch((e) =>
        setSettingsStatus(e instanceof Error ? e.message : String(e)),
      )
      .finally(() => setSettingsLoading(false));
  }, []);

  const saveSettings = async (next: TtsSettings) => {
    const prev = settings;
    setSettings(next);
    setBusy(true);
    setSettingsStatus(null);
    try {
      await setTtsSettings(next);
      setSettingsStatus("TTS settings saved.");
    } catch (e) {
      setSettings(prev);
      setSettingsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onReadSelection = async () => {
    setBusy(true);
    setRunStatus(null);
    try {
      await runReadAloud();
      setRunStatus("Read-aloud started.");
    } catch (e) {
      setRunStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onTriggerTts = async () => {
    setBusy(true);
    setRunStatus(null);
    try {
      await triggerTts();
      setRunStatus("TTS triggered.");
    } catch (e) {
      setRunStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h2 style={{ marginTop: 0 }}>Read aloud</h2>
        <p className="kea-muted" style={{ marginTop: 0 }}>
          Bind a TTS engine, configure voice and model, set the read-selection hotkey,
          and test playback from the UI.
        </p>
      </header>

      <section style={{ marginBottom: 24 }}>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={onReadSelection}
            disabled={busy}
          >
            Read selection
          </button>
          <button type="button" className="kea-btn" onClick={onTriggerTts} disabled={busy}>
            Trigger TTS (hotkey path)
          </button>
        </div>
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
          <SlotBinder feature={TTS_FEATURE} slot={TTS_SLOT} slotKind="tts" />
          <HotkeyBinder
            feature={TTS_FEATURE}
            command={TTS_COMMAND}
            label="Read selection hotkey"
          />
          <section className="kea-card" style={{ marginBottom: 0 }}>
            <h3 style={{ margin: "0 0 12px" }}>TTS options</h3>
            {settingsLoading ? (
              <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 60 }}>
                <Spinner size={16} />
                <span className="kea-muted">Loading settings…</span>
              </div>
            ) : (
              <>
            <label style={{ display: "block", marginBottom: 12 }}>
              <span className="kea-label">Voice (OpenAI)</span>
              <select
                className="kea-select"
                value={settings.active_voice ?? ""}
                disabled={busy}
                onChange={(e) =>
                  saveSettings({
                    ...settings,
                    active_voice: e.target.value || null,
                  })
                }
                style={{ minWidth: 220 }}
              >
                <option value="">Default</option>
                {OPENAI_VOICES.map((voice) => (
                  <option key={voice} value={voice}>
                    {voice}
                  </option>
                ))}
              </select>
            </label>
            <label style={{ display: "block", marginBottom: 12 }}>
              <span className="kea-label">Model</span>
              <input
                className="kea-input"
                value={settings.active_model ?? ""}
                disabled={busy}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    active_model: e.target.value || null,
                  })
                }
                onBlur={(e) =>
                  saveSettings({
                    ...settings,
                    active_model: e.target.value || null,
                  })
                }
                placeholder="e.g. tts-1, tts-1-hd, or local ONNX model id"
                style={{ maxWidth: 320 }}
              />
            </label>
            {settingsStatus && (
              <p className="kea-muted" style={{ marginTop: 0, marginBottom: 0 }}>
                {settingsStatus}
              </p>
            )}
              </>
            )}
          </section>
        </div>
      </details>
    </div>
  );
}
