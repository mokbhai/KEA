import { useEffect, useState } from "react";
import {
  getTtsSettings,
  previewVoice,
  runReadAloud,
  setTtsSettings,
  triggerTts,
  type TtsSettings,
} from "../api";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner, { type FixNavigate } from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useSavedFlash } from "../hooks/useSavedFlash";
import { OPENAI_TTS_VOICES } from "../lib/capabilityDefaults";
import type { SlotSpec } from "../lib/featureSlot";

const TTS_FEATURE = "tts";
const TTS_COMMAND = "read_selection";

const SLOTS: SlotSpec[] = [
  { feature: "tts", slot: "tts", capability: "tts", label: "Text to speech" },
];

type Props = {
  onNavigate?: FixNavigate;
};

export default function ReadAloudPage({ onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const [settings, setSettings] = useState<TtsSettings>({
    active_voice: null,
    active_model: null,
  });
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [savedKey, flash] = useSavedFlash();

  useEffect(() => {
    getTtsSettings()
      .then(setSettings)
      .catch((e) => setSettingsStatus(e instanceof Error ? e.message : String(e)))
      .finally(() => setSettingsLoading(false));
  }, []);

  const saveSettings = async (next: TtsSettings, key: string) => {
    const prev = settings;
    setSettings(next);
    setBusy(true);
    setSettingsStatus(null);
    try {
      await setTtsSettings(next);
      flash(key);
    } catch (e) {
      setSettings(prev);
      setSettingsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const effective = ai.statuses?.[0]?.effective ?? null;

  const playSample = async () => {
    setBusy(true);
    setRunStatus(null);
    try {
      if (!effective) throw new Error("Choose a voice first.");
      await previewVoice(
        effective.engine_id,
        effective.model ?? settings.active_model,
        settings.active_voice,
      );
      setRunStatus("Playing a sample sentence…");
    } catch (e) {
      setRunStatus(e instanceof Error ? e.message : String(e));
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
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Select text anywhere on your Mac and press the shortcut to hear it read
          out loud.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Behavior</h3>
        {settingsLoading ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 44 }}>
            <Spinner size={16} />
            <span className="kea-muted">Loading settings…</span>
          </div>
        ) : (
          <>
            <RowGroup aria-label="Read aloud behavior">
              <HotkeyRow
                feature={TTS_FEATURE}
                command={TTS_COMMAND}
                label="Shortcut"
                hint="Reads the selected text out loud."
                checkRegistration
              />
              <Row label="Voice" hint="Used by the cloud voices; local voices bring their own.">
                {savedKey === "voice" && <span className="kea-saved">Saved ✓</span>}
                <select
                  className="kea-select"
                  aria-label="Voice"
                  value={settings.active_voice ?? ""}
                  disabled={busy}
                  onChange={(e) =>
                    void saveSettings(
                      { ...settings, active_voice: e.target.value || null },
                      "voice",
                    )
                  }
                >
                  <option value="">Default</option>
                  {OPENAI_TTS_VOICES.map((voice) => (
                    <option key={voice} value={voice}>
                      {voice}
                    </option>
                  ))}
                </select>
              </Row>
            </RowGroup>

            <details className="kea-advanced">
              <summary>Advanced</summary>
              <div className="kea-advanced__body">
                <label>
                  <span className="kea-label">Fallback model id</span>
                  <input
                    className="kea-input"
                    value={settings.active_model ?? ""}
                    disabled={busy}
                    onChange={(e) =>
                      setSettings({ ...settings, active_model: e.target.value || null })
                    }
                    onBlur={(e) =>
                      void saveSettings(
                        { ...settings, active_model: e.target.value || null },
                        "model",
                      )
                    }
                    placeholder="e.g. tts-1, tts-1-hd, or local ONNX model id"
                  />
                  <span className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                    Only used when the chosen voice above carries no model of its own.
                  </span>
                </label>
                {savedKey === "model" && <span className="kea-saved">Saved ✓</span>}
              </div>
            </details>

            {settingsStatus && (
              <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {settingsStatus}
              </p>
            )}
          </>
        )}
      </section>

      <FeatureAiCard ai={ai} featureLabel="Read aloud" />

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Try it</h3>
        <div className="kea-card">
          <p style={{ marginTop: 0 }}>Hear a sample sentence in the chosen voice.</p>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              onClick={() => void playSample()}
              disabled={busy || !effective}
            >
              {busy ? <Spinner size={14} /> : "▶︎"} Play sample
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onReadSelection()}
              disabled={busy}
            >
              Read my selection
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onTriggerTts()}
              disabled={busy}
            >
              Trigger the shortcut path
            </button>
          </div>
          {runStatus && (
            <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
              {runStatus}
            </p>
          )}
        </div>
      </section>
    </div>
  );
}
