import { useEffect, useState } from "react";
import {
  getDictationSettings,
  getDictationStateApi,
  onDictationState,
  setDictationSettings,
  startDictation,
  stopDictation,
  type DictationState,
} from "../api";
import HotkeyBinder from "../components/HotkeyBinder";
import SlotBinder from "../components/SlotBinder";
import Spinner from "../components/Spinner";

const DICTATION_FEATURE = "dictation";
const DICTATION_SLOT = "stt";
const DICTATION_COMMAND = "push_to_talk";

const settingsSummaryStyle: React.CSSProperties = {
  cursor: "pointer",
  color: "var(--text)",
  fontWeight: 600,
  fontSize: "0.875rem",
};

export default function DictationPage() {
  const [postProcess, setPostProcess] = useState(false);
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [dictationStatus, setDictationStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dictationState, setDictationState] = useState<DictationState>("idle");
  const listening = dictationState !== "idle";

  useEffect(() => {
    getDictationSettings()
      .then((settings) => setPostProcess(settings.post_process))
      .catch((e) =>
        setSettingsStatus(e instanceof Error ? e.message : String(e)),
      )
      .finally(() => setSettingsLoading(false));

    getDictationStateApi()
      .then(setDictationState)
      .catch(() => {}); /* best-effort — event subscription covers ongoing updates */
  }, []);

  useEffect(() => {
    const unsub = onDictationState((state) => {
      setDictationState(state);
    });
    return () => {
      void unsub.then((fn) => fn());
    };
  }, []);

  const savePostProcess = async (enabled: boolean) => {
    const previous = postProcess;
    setPostProcess(enabled);
    setBusy(true);
    setSettingsStatus(null);
    try {
      const current = await getDictationSettings();
      await setDictationSettings({ ...current, post_process: enabled });
      setSettingsStatus("Dictation settings saved.");
    } catch (e) {
      setPostProcess(previous);
      setSettingsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onStart = async () => {
    setBusy(true);
    setDictationStatus(null);
    try {
      await startDictation();
      setDictationStatus("Listening — speak now, then stop.");
    } catch (e) {
      setDictationStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onStop = async () => {
    setBusy(true);
    setDictationStatus(null);
    try {
      const text = await stopDictation();
      setDictationStatus(
        text ? `Inserted: ${text.slice(0, 80)}${text.length > 80 ? "…" : ""}` : "Dictation stopped.",
      );
    } catch (e) {
      setDictationStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h2 style={{ marginTop: 0 }}>Dictation</h2>
        <p className="kea-muted" style={{ marginTop: 0 }}>
          Bind an STT engine, set push-to-talk, optionally refine transcripts with
          the rewrite LLM, and test capture from the UI.
        </p>
      </header>

      <section style={{ marginBottom: 24 }}>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={onStart}
            disabled={busy || listening}
          >
            Start dictation
          </button>
          <button
            type="button"
            className="kea-btn"
            onClick={onStop}
            disabled={busy || !listening}
          >
            Stop dictation
          </button>
        </div>
        {dictationStatus && (
          <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
            {dictationStatus}
          </p>
        )}
      </section>

      <details open className="kea-card" style={{ marginBottom: 16 }}>
        <summary className="kea-label" style={settingsSummaryStyle}>
          Settings
        </summary>
        <div style={{ marginTop: 16 }}>
          <SlotBinder
            feature={DICTATION_FEATURE}
            slot={DICTATION_SLOT}
            slotKind="stt"
          />
          <HotkeyBinder
            feature={DICTATION_FEATURE}
            command={DICTATION_COMMAND}
            label="Push-to-talk hotkey"
          />
          <section className="kea-card" style={{ marginBottom: 0 }}>
            <h3 style={{ margin: "0 0 12px" }}>Dictation options</h3>
            {settingsLoading ? (
              <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 60 }}>
                <Spinner size={16} />
                <span className="kea-muted">Loading settings…</span>
              </div>
            ) : (
              <>
            <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <input
                type="checkbox"
                checked={postProcess}
                disabled={busy}
                onChange={(e) => savePostProcess(e.target.checked)}
              />
              <span>Audio refinement (post-process transcript via LLM)</span>
            </label>
            {settingsStatus && (
              <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
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
