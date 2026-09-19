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
  type DictationSettings,
  type DictationState,
} from "../api";
import FeatureAiCard from "../components/FeatureAiCard";
import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow, { formatAccelerator, KeyChips } from "../components/HotkeyRow";
import LevelMeter from "../components/LevelMeter";
import LoadingBlock from "../components/LoadingBlock";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

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
  onNavigate?: Navigate;
};

export default function DictationPage({ onNavigate }: Props) {
  // Saved whole, so every write re-reads first: a stale copy of the other
  // field would quietly revert it.
  const settings = useOptimisticSetting<DictationSettings>({
    initial: { post_process: false, active_model: null, hold_to_talk: false },
    persist: setDictationSettings,
    reread: getDictationSettings,
  });
  const { post_process: postProcess, hold_to_talk: holdToTalk } = settings.value;
  const { setValue: setSettingsValue, setError: setSettingsError } = settings;
  const ai = useFeatureAi(postProcess ? SLOTS_WITH_CLEANUP : SLOTS_PLAIN);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [dictationStatus, setDictationStatus] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dictationState, setDictationState] = useState<DictationState>("idle");
  const [level, setLevel] = useState(0);
  const [accelerator, setAccelerator] = useState<string | null>(null);
  const listening = dictationState !== "idle";
  // One flag for the page: a save and a run both lock the whole panel, as
  // they did when this was a single `busy`.
  const anyBusy = busy || settings.busy;

  useEffect(() => {
    getDictationSettings()
      .then(setSettingsValue)
      .catch((e) => setSettingsError(toMessage(e)))
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

  const onStart = async () => {
    setBusy(true);
    setDictationStatus(null);
    try {
      await startDictation();
      setDictationStatus("Listening — speak now, then stop.");
    } catch (e) {
      setDictationStatus(toMessage(e));
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
      setDictationStatus(toMessage(e));
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
          <LoadingBlock label="Loading settings…" minHeight={44} />
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
                label="Hold to talk"
                hint="Hold ⌥⇧ anywhere to record, then let go and KEA types what you said. A quick tap does nothing, and neither does holding ⌥⇧ for one of your own shortcuts."
              >
                <KeyChips accelerator="Option+Shift" />
                {settings.savedKey === "hold_to_talk" && (
                  <span className="kea-saved">Saved ✓</span>
                )}
                <Toggle
                  label="Hold to talk"
                  checked={holdToTalk}
                  disabled={anyBusy}
                  onChange={(next) => void settings.save({ hold_to_talk: next }, "hold_to_talk")}
                />
              </Row>
              <Row
                label="Clean up text with AI"
                hint="Removes filler words and fixes punctuation before typing. Needs the Rewrite AI — dictation fails outright without it."
              >
                {settings.savedKey === "post_process" && (
                  <span className="kea-saved">Saved ✓</span>
                )}
                <Toggle
                  label="Clean up text with AI"
                  checked={postProcess}
                  disabled={anyBusy}
                  onChange={(next) => void settings.save({ post_process: next }, "post_process")}
                />
              </Row>
            </RowGroup>
            {settings.error && (
              <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {settings.error}
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
              disabled={anyBusy || listening}
            >
              Start listening
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onStop()}
              disabled={anyBusy || !listening}
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
