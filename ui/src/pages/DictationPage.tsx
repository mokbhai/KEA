import { useEffect, useState } from "react";
import {
  getDictationSettings,
  getDictationStateApi,
  getEffectiveHotkey,
  onDictationLevel,
  onDictationState,
  setDictationSettings,
  startDictation,
  stopDictation,
  type DictationState,
} from "../api";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner, { type FixNavigate } from "../components/FeatureBanner";
import HotkeyRow, { formatAccelerator } from "../components/HotkeyRow";
import LevelMeter from "../components/LevelMeter";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import Toggle from "../components/Toggle";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useSavedFlash } from "../hooks/useSavedFlash";
import type { SlotSpec } from "../lib/featureSlot";

const DICTATION_FEATURE = "dictation";
const DICTATION_COMMAND = "push_to_talk";

const STT_SLOT: SlotSpec = {
  feature: "dictation",
  slot: "stt",
  capability: "stt",
  label: "Speech to text",
};

/**
 * With clean-up on, dictation.rs resolves ("rewrite", "llm") and fails the
 * *whole* run when it can't — the transcript is discarded, not merely left
 * un-cleaned. So the clean-up AI is a real dependency exactly while the toggle
 * is on, and the page must say so. Both arrays are module-level constants to
 * keep useFeatureAi's stable-identity contract.
 */
const SLOTS_PLAIN: SlotSpec[] = [STT_SLOT];

const SLOTS_WITH_CLEANUP: SlotSpec[] = [
  STT_SLOT,
  {
    feature: "rewrite",
    slot: "llm",
    capability: "llm",
    label: "Clean-up (uses the Rewrite AI)",
  },
];

type Props = {
  onNavigate?: FixNavigate;
};

export default function DictationPage({ onNavigate }: Props) {
  const [postProcess, setPostProcess] = useState(false);
  const ai = useFeatureAi(postProcess ? SLOTS_WITH_CLEANUP : SLOTS_PLAIN);
  const [settingsStatus, setSettingsStatus] = useState<string | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [dictationStatus, setDictationStatus] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dictationState, setDictationState] = useState<DictationState>("idle");
  const [level, setLevel] = useState(0);
  const [accelerator, setAccelerator] = useState<string | null>(null);
  const [savedKey, flash] = useSavedFlash();
  const listening = dictationState !== "idle";

  useEffect(() => {
    getDictationSettings()
      .then((settings) => setPostProcess(settings.post_process))
      .catch((e) => setSettingsStatus(e instanceof Error ? e.message : String(e)))
      .finally(() => setSettingsLoading(false));

    getDictationStateApi()
      .then(setDictationState)
      .catch(() => {}); /* best-effort — event subscription covers ongoing updates */

    getEffectiveHotkey(DICTATION_FEATURE, DICTATION_COMMAND)
      .then((hk) => setAccelerator(hk ? hk.accelerator : null))
      .catch(() => setAccelerator(null));
  }, []);

  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState((state) => setDictationState(state)),
      onDictationLevel(setLevel),
    ]);
    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    if (dictationState !== "listening") setLevel(0);
  }, [dictationState]);

  const savePostProcess = async (enabled: boolean) => {
    const previous = postProcess;
    setPostProcess(enabled);
    setBusy(true);
    setSettingsStatus(null);
    try {
      const current = await getDictationSettings();
      await setDictationSettings({ ...current, post_process: enabled });
      flash("post_process");
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
      setTranscript(text || null);
      setDictationStatus(text ? "Typed into the app you were last in." : "Nothing was heard.");
    } catch (e) {
      setDictationStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Dictation</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Hold the shortcut anywhere on your Mac, talk, and KEA types what you said.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Behavior</h2>
        {settingsLoading ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 44 }}>
            <Spinner size={16} />
            <span className="kea-muted">Loading settings…</span>
          </div>
        ) : (
          <>
            <RowGroup aria-label="Dictation behavior">
              <HotkeyRow
                feature={DICTATION_FEATURE}
                command={DICTATION_COMMAND}
                label="Shortcut"
                hint="Hold to talk, release to type what you said."
                onSaved={setAccelerator}
                checkRegistration
              />
              <Row
                label="Clean up text with AI"
                hint="Removes filler words and fixes punctuation before typing. Needs the Rewrite AI — dictation fails outright without it."
              >
                {savedKey === "post_process" && <span className="kea-saved">Saved ✓</span>}
                <Toggle
                  label="Clean up text with AI"
                  checked={postProcess}
                  disabled={busy}
                  onChange={(next) => void savePostProcess(next)}
                />
              </Row>
            </RowGroup>
            {settingsStatus && (
              <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {settingsStatus}
              </p>
            )}
          </>
        )}
      </section>

      <FeatureAiCard ai={ai} featureLabel="Dictation" />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Try it</h2>
        <div className="kea-card">
          <p style={{ marginTop: 0 }}>
            {accelerator ? (
              <>
                Hold <strong>{formatAccelerator(accelerator)}</strong> and say something —
                or use the buttons below.
              </>
            ) : (
              "Start listening, say something, then stop."
            )}
          </p>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 12,
              flexWrap: "wrap",
              marginBottom: 12,
            }}
          >
            <span className="kea-muted" style={{ fontSize: 13 }}>
              State:{" "}
              <strong style={{ color: "var(--text)" }}>
                {dictationState === "idle"
                  ? "Idle"
                  : dictationState === "listening"
                    ? "Listening"
                    : "Processing"}
              </strong>
            </span>
            <LevelMeter level={level} />
          </div>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button
              type="button"
              className="kea-btn kea-btn--primary"
              onClick={() => void onStart()}
              disabled={busy || listening}
            >
              Start listening
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onStop()}
              disabled={busy || !listening}
            >
              Stop
            </button>
          </div>
          {dictationStatus && (
            <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
              {dictationStatus}
            </p>
          )}
          {transcript && (
            <div style={{ marginTop: 12 }}>
              <span className="kea-label">Last transcript</span>
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
                {transcript}
              </p>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
