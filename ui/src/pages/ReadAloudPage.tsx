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
import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import LoadingBlock from "../components/LoadingBlock";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import { OPENAI_TTS_VOICES } from "../lib/capabilityDefaults";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

const TTS_FEATURE = "tts";
const TTS_COMMAND = "read_selection";

const SLOTS: SlotSpec[] = [
  { feature: "tts", slot: "tts", capability: "tts", label: "Text to speech" },
];

type Props = {
  onNavigate?: Navigate;
};

export default function ReadAloudPage({ onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);
  const tts = useOptimisticSetting<TtsSettings>({
    initial: { active_voice: null, active_model: null },
    persist: setTtsSettings,
  });
  const settings = tts.value;
  const { setValue: setSettings, setError: setSettingsError } = tts;
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [runStatus, setRunStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A save and a run both lock the whole panel, as they did when this was a
  // single `busy`.
  const anyBusy = busy || tts.busy;

  useEffect(() => {
    getTtsSettings()
      .then(setSettings)
      .catch((e) => setSettingsError(toMessage(e)))
      .finally(() => setSettingsLoading(false));
  }, [setSettings, setSettingsError]);

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
        effective.provider_ref,
      );
      setRunStatus("Playing a sample sentence…");
    } catch (e) {
      setRunStatus(toMessage(e));
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
      setRunStatus(toMessage(e));
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
      setRunStatus(toMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Read aloud</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Select text anywhere on your Mac and press the shortcut to hear it read
          out loud.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Behavior</h2>
        {settingsLoading ? (
          <LoadingBlock label="Loading settings…" minHeight={44} />
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
                {tts.savedKey === "voice" && <span className="kea-saved">Saved ✓</span>}
                <select
                  className="kea-select"
                  aria-label="Voice"
                  value={settings.active_voice ?? ""}
                  disabled={anyBusy}
                  onChange={(e) =>
                    void tts.save({ active_voice: e.target.value || null }, "voice")
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
                    disabled={anyBusy}
                    onChange={(e) =>
                      setSettings({ ...settings, active_model: e.target.value || null })
                    }
                    onBlur={(e) =>
                      void tts.save({ active_model: e.target.value || null }, "model")
                    }
                    placeholder="e.g. tts-1, tts-1-hd, or local ONNX model id"
                  />
                  <span className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                    Only used when the chosen voice above carries no model of its own.
                  </span>
                </label>
                {tts.savedKey === "model" && <span className="kea-saved">Saved ✓</span>}
              </div>
            </details>

            {tts.error && (
              <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {tts.error}
              </p>
            )}
          </>
        )}
      </section>

      <FeatureAiCard ai={ai} featureLabel="Read aloud" />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Try it</h2>
        <div className="kea-card">
          <p style={{ marginTop: 0 }}>Hear a sample sentence in the chosen voice.</p>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              onClick={() => void playSample()}
              disabled={anyBusy || !effective}
            >
              {anyBusy ? <Spinner size={14} /> : "▶︎"} Play sample
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onReadSelection()}
              disabled={anyBusy}
            >
              Read my selection
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onTriggerTts()}
              disabled={anyBusy}
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
