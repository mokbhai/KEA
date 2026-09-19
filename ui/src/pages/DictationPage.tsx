import { useEffect, useState } from "react";
import {
  DICTATION_LANGUAGES,
  getDictationSettings,
  getDictationStateApi,
  getEffectiveHotkey,
  getSetting,
  listInputDevices,
  listInstalledOnnxModels,
  listOnnxModels,
  onDeviceFallback,
  onDictationLevel,
  onDictationPreview,
  onDictationState,
  setDictationSettings,
  setSetting,
  startDictation,
  startInputPreview,
  stopDictation,
  stopInputPreview,
  type DictationSettings,
  type DictationState,
  type InputDevice,
  type OnnxModel,
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
import { acceptsLanguage, engineSpec } from "../lib/engines";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

const DICTATION_FEATURE = "dictation";
const DICTATION_COMMAND = "push_to_talk";

/**
 * The `<select>` value standing for "no preference". An empty string rather
 * than a sentinel name: device ids are names, and any name we invented could
 * collide with a real one.
 */
const SYSTEM_DEFAULT = "";

/**
 * The `<select>` value standing for "let the model work it out", which is the
 * `null` language. Same reasoning as SYSTEM_DEFAULT: a real tag can never be
 * the empty string.
 */
const AUTO_DETECT = "";

/**
 * The catalog language of a model that is not tied to one language — the only
 * kind a language can be chosen for (crates/infer/src/registry.rs).
 */
const MULTILINGUAL = "multilingual";

/**
 * The live-transcript settings, which are plain key/value rows rather than
 * part of `DictationSettings` (src-tauri/src/commands.rs).
 */
const STREAMING_MODEL_KEY = "dictation.streaming_model";
const SHOW_PARTIALS_KEY = "dictation.show_partials";
const STREAMING_FALLBACK_KEY = "dictation.streaming_fallback";

/**
 * The `<select>` value standing for "no live transcript". Same reasoning as
 * SYSTEM_DEFAULT — and it is also what the backend reads as off, since a
 * blank model id means the feature is off there (`streaming_model_setting`).
 * There is deliberately no separate enable flag: the model *is* the switch.
 */
const STREAMING_OFF = "";

/** The catalog kind the live-preview models are listed and installed under. */
const STREAMING_KIND = "streaming";

type LiveTranscript = {
  /** Empty means off. */
  model: string;
  showPartials: boolean;
  fallback: boolean;
};

/**
 * `set_setting` takes a string and stores the JSON *of that string*, so a
 * boolean written from here is the JSON string "true"/"false" — which is what
 * the Rust side's `bool_setting` reads back (it accepts a real JSON bool too,
 * for the callers that are not this command). `String(next)` is the same
 * encoding HistoryPage's toggle writes; anything else would be a value only
 * one half of the wire understood.
 */
const readBool = (value: string | null, whenUnset: boolean) =>
  value === "true" ? true : value === "false" ? false : whenUnset;

/**
 * All three keys go together so the stored state is never half a decision —
 * and so choosing a model materialises the two defaults next to it instead of
 * leaving them implicit.
 */
const persistLiveTranscript = (next: LiveTranscript) =>
  Promise.all([
    setSetting(STREAMING_MODEL_KEY, next.model),
    setSetting(SHOW_PARTIALS_KEY, String(next.showPartials)),
    setSetting(STREAMING_FALLBACK_KEY, String(next.fallback)),
  ]);

const STATE_LABELS: Record<DictationState, string> = {
  idle: "Idle",
  listening: "Listening",
  locked: "Locked",
  processing: "Processing",
};

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
    initial: {
      post_process: false,
      active_model: null,
      hold_to_talk: false,
      input_device: null,
      preroll: true,
      language: null,
    },
    persist: setDictationSettings,
    reread: getDictationSettings,
  });
  const {
    post_process: postProcess,
    hold_to_talk: holdToTalk,
    input_device: inputDevice,
    preroll,
    language,
  } = settings.value;
  const { setValue: setSettingsValue, setError: setSettingsError } = settings;
  // Three independent settings keys, saved together. No `reread`: nothing else
  // in this window writes them, and the one backend writer — the sweep that
  // clears the model when its files are deleted from the Models page — is
  // picked up by the mount read the next time this page is opened.
  const live = useOptimisticSetting<LiveTranscript>({
    initial: { model: STREAMING_OFF, showPartials: true, fallback: false },
    persist: persistLiveTranscript,
  });
  const { model: streamingModel, showPartials, fallback: draftFallback } = live.value;
  const { setValue: setLiveValue } = live;
  const ai = useFeatureAi(postProcess ? SLOTS_WITH_CLEANUP : SLOTS_PLAIN);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [liveLoading, setLiveLoading] = useState(true);
  const [streamingModels, setStreamingModels] = useState<OnnxModel[]>([]);
  const [streamingCatalogError, setStreamingCatalogError] = useState<string | null>(
    null,
  );
  const [dictationStatus, setDictationStatus] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [dictationState, setDictationState] = useState<DictationState>("idle");
  const [level, setLevel] = useState(0);
  const [accelerator, setAccelerator] = useState<string | null>(null);
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [previewing, setPreviewing] = useState(false);
  const [fallbackNotice, setFallbackNotice] = useState<string | null>(null);
  const [catalogLanguages, setCatalogLanguages] = useState<Map<string, string>>(
    () => new Map(),
  );
  const listening = dictationState !== "idle";

  // What the backend will actually transcribe with: the speech-to-text slot's
  // effective binding, and the model it carries — or the saved fallback it
  // reads when the binding carries none (dictation.rs).
  const stt = ai.statuses?.find((s) => s.spec.capability === "stt")?.effective ?? null;
  const sttCatalog =
    stt && acceptsLanguage(stt.engine_id) ? engineSpec(stt.engine_id)?.catalog : undefined;
  const sttModel = stt?.model ?? settings.value.active_model;
  // One flag for the page: a save and a run both lock the whole panel, as
  // they did when this was a single `busy`.
  const anyBusy = busy || settings.busy || live.busy;

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

    listInputDevices()
      .then(setDevices)
      .catch(() => setDevices([])); /* the dropdown falls back to the saved name */

    // Each key degrades on its own, and a failed read means off rather than
    // broken: a model cleared by the Models page is stored as the JSON literal
    // `null`, which `get_setting` — a plain `get::<String>` — cannot
    // deserialize, so the read for that key rejects forever after. Off is
    // exactly what that state means, and it is what the backend does with it.
    Promise.all([
      getSetting(STREAMING_MODEL_KEY).catch(() => null),
      getSetting(SHOW_PARTIALS_KEY).catch(() => null),
      getSetting(STREAMING_FALLBACK_KEY).catch(() => null),
    ])
      .then(([model, partials, draft]) =>
        setLiveValue({
          model: model?.trim() || STREAMING_OFF,
          showPartials: readBool(partials, true),
          fallback: readBool(draft, false),
        }),
      )
      .finally(() => setLiveLoading(false));

    // Only the installed ones are offered: a model that is not on disk would
    // be a choice that silently does nothing, so the empty state sends the
    // user to the Models page instead.
    Promise.all([
      listOnnxModels(STREAMING_KIND),
      listInstalledOnnxModels(STREAMING_KIND),
    ])
      .then(([models, installed]) => {
        const onDisk = new Set(installed);
        setStreamingModels(models.filter((m) => onDisk.has(m.id)));
      })
      .catch((e) => setStreamingCatalogError(toMessage(e)));
  }, []);

  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState((state) => setDictationState(state)),
      onDictationLevel(setLevel),
      // The preview stops itself — on a timer, on window blur, and when a
      // hotkey takes the microphone — so the toggle follows the backend
      // rather than the click.
      onDictationPreview(setPreviewing),
      onDeviceFallback((fallback) =>
        setFallbackNotice(
          `${fallback.requested} isn't connected — recording from ${
            fallback.using ?? "the default microphone"
          } instead.`,
        ),
      ),
    ]);
    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    // Levels arrive from a recording and from the preview alike; a stale bar
    // left standing after either ends reads as a mic that never closed.
    const metered = dictationState === "listening" || dictationState === "locked";
    if (!metered && !previewing) setLevel(0);
  }, [dictationState, previewing]);

  // Only a language-capable engine's catalog is worth fetching, and only its
  // `language` column is used — a model pinned to one language has nothing to
  // choose.
  useEffect(() => {
    if (!sttCatalog) return;
    let cancelled = false;
    sttCatalog
      .list()
      .then((models) => {
        if (cancelled) return;
        setCatalogLanguages(new Map(models.map((m) => [m.id, m.language])));
      })
      // Deliberately silent: a catalog we could not read is no reason to
      // offer a setting the engine might drop, so the row stays hidden.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [sttCatalog]);

  /**
   * Whether to offer a language at all.
   *
   * Gated on the *engine* first, not just the model. The ONNX transducer
   * accepted a language and silently dropped it until the design review took
   * the parameter away from it (crates/engines/src/stt/parakeet.rs); a
   * control that writes a setting nothing reads is that same defect wearing a
   * dropdown. The model gate is second: the English-only Whisper builds have
   * nothing to choose between.
   */
  const showLanguage =
    !!stt &&
    acceptsLanguage(stt.engine_id) &&
    !!sttModel &&
    catalogLanguages.get(sttModel)?.toLowerCase() === MULTILINGUAL;

  // A selected model is the whole on/off state of the feature, so the two
  // dependent rows follow it: showing them while nothing can produce a partial
  // would offer settings that do nothing.
  const streamingOn = streamingModel !== STREAMING_OFF;
  // A saved model whose files are gone still counts as a choice — it has to
  // stay selectable so "Off" is a thing the user can actually pick.
  const noStreamingChoice = streamingModels.length === 0 && !streamingOn;

  // Stopping is best-effort on unmount: the backend also stops the preview on
  // window blur and after 30s, so a navigation cannot strand an open mic.
  useEffect(() => () => void stopInputPreview().catch(() => {}), []);

  const onToggleDevice = (id: string) => {
    setFallbackNotice(null);
    void settings.save(
      { input_device: id === SYSTEM_DEFAULT ? null : id },
      "input_device",
    );
  };

  const onTogglePreview = async (next: boolean) => {
    // Optimistic so the toggle does not lag the meter; the backend's own
    // preview event corrects it either way.
    setPreviewing(next);
    try {
      await (next ? startInputPreview() : stopInputPreview());
    } catch (e) {
      setPreviewing(false);
      setDictationStatus(toMessage(e));
    }
  };

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
        <h2 style={{ margin: "0 0 12px" }}>Microphone</h2>
        <RowGroup aria-label="Microphone">
          <Row
            label="Input device"
            hint="Which microphone to record from. If it isn't connected when you dictate, KEA uses the default one and says so."
          >
            {settings.savedKey === "input_device" && (
              <span className="kea-saved">Saved ✓</span>
            )}
            <select
              className="kea-select"
              aria-label="Input device"
              value={inputDevice ?? SYSTEM_DEFAULT}
              disabled={anyBusy}
              onChange={(e) => onToggleDevice(e.target.value)}
            >
              <option value={SYSTEM_DEFAULT}>System default</option>
              {devices.map((device) => (
                <option key={device.id} value={device.id}>
                  {device.name}
                  {device.is_default ? " (default)" : ""}
                </option>
              ))}
              {/* A saved device that is not plugged in right now still has to
                  show as the selection, or the dropdown would silently look
                  like the user had picked the default. */}
              {inputDevice && !devices.some((d) => d.id === inputDevice) && (
                <option value={inputDevice}>{inputDevice} (not connected)</option>
              )}
            </select>
          </Row>
          <Row
            label="Test microphone"
            hint="Opens the mic just to show the level — nothing is recorded. Stops on its own after 30 seconds, and whenever a real recording starts."
          >
            <LevelMeter level={previewing ? level : 0} />
            <Toggle
              label="Test microphone"
              checked={previewing}
              disabled={anyBusy || listening}
              onChange={(next) => void onTogglePreview(next)}
            />
          </Row>
        </RowGroup>
        {fallbackNotice && (
          <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--warn)" }}>
            {fallbackNotice}
          </p>
        )}
      </section>

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
              {/* Hidden rather than explained when it does not apply: a
                  paragraph about a control that is not on screen is noise. */}
              {showLanguage && (
                <Row
                  label="Language"
                  hint="Naming the language you speak beats letting the model guess it — a guess can turn on one stray word and take the rest of the sentence with it."
                >
                  {settings.savedKey === "language" && (
                    <span className="kea-saved">Saved ✓</span>
                  )}
                  <select
                    className="kea-select"
                    aria-label="Dictation language"
                    value={language ?? AUTO_DETECT}
                    disabled={anyBusy}
                    onChange={(e) =>
                      void settings.save(
                        {
                          language:
                            e.target.value === AUTO_DETECT ? null : e.target.value,
                        },
                        "language",
                      )
                    }
                  >
                    <option value={AUTO_DETECT}>Auto-detect</option>
                    {DICTATION_LANGUAGES.map((l) => (
                      <option key={l.tag} value={l.tag}>
                        {l.label}
                      </option>
                    ))}
                    {/* A tag this build does not list — saved by a newer one,
                        or a full locale — still has to show as the selection,
                        or the dropdown would read as auto-detect. */}
                    {language && !DICTATION_LANGUAGES.some((l) => l.tag === language) && (
                      <option value={language}>{language}</option>
                    )}
                  </select>
                </Row>
              )}
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
                label="Catch the first word"
                hint="Opens the mic the moment you press ⌥ or ⇧, so the start of what you say isn't clipped while KEA waits to be sure you meant it. Audio from a chord that never becomes a recording is thrown away."
              >
                {settings.savedKey === "preroll" && (
                  <span className="kea-saved">Saved ✓</span>
                )}
                <Toggle
                  label="Catch the first word"
                  checked={preroll}
                  disabled={anyBusy}
                  onChange={(next) => void settings.save({ preroll: next }, "preroll")}
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

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Live transcript</h2>
        {liveLoading ? (
          <LoadingBlock label="Loading settings…" minHeight={44} />
        ) : (
          <>
            <RowGroup aria-label="Live transcript">
              <Row
                label="Live preview model"
                hint="A small, fast model that shows words in the dictation window while you talk. It is a preview only — when you stop, your speech model transcribes the whole recording again and that second pass is what gets typed, so the preview can be visibly wrong and then correct itself."
              >
                {streamingCatalogError ? (
                  <span className="kea-muted">
                    Couldn&apos;t check which previews are downloaded.
                  </span>
                ) : noStreamingChoice ? (
                  <span className="kea-muted">No preview model is downloaded yet.</span>
                ) : (
                  <>
                    {live.savedKey === "model" && (
                      <span className="kea-saved">Saved ✓</span>
                    )}
                    <select
                      className="kea-select"
                      aria-label="Live preview model"
                      value={streamingModel}
                      disabled={anyBusy}
                      onChange={(e) => void live.save({ model: e.target.value }, "model")}
                    >
                      <option value={STREAMING_OFF}>Off</option>
                      {streamingModels.map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.display_name}
                        </option>
                      ))}
                      {/* A saved model whose files were removed still has to
                          show as the selection, or the dropdown would read as
                          Off while the setting says otherwise. */}
                      {streamingOn &&
                        !streamingModels.some((m) => m.id === streamingModel) && (
                          <option value={streamingModel}>
                            {streamingModel} (not downloaded)
                          </option>
                        )}
                    </select>
                  </>
                )}
                {/* Only offered when there is somewhere to send the user, the
                    same rule the blocked-slot banner uses. The label is
                    spelled out because that banner's own "Open Models" can be
                    on screen at the same time, for a different reason. */}
                {(streamingCatalogError || noStreamingChoice) && onNavigate && (
                  <button
                    type="button"
                    className="kea-btn"
                    aria-label="Open Models to download a live preview"
                    onClick={() => onNavigate("models")}
                  >
                    Open Models
                  </button>
                )}
              </Row>
              {streamingOn && (
                <>
                  <Row
                    label="Show the preview"
                    hint="Draws the running guess in the dictation window. Off, nothing extra is shown and the second model does all the work on its own."
                  >
                    {live.savedKey === "showPartials" && (
                      <span className="kea-saved">Saved ✓</span>
                    )}
                    <Toggle
                      label="Show the preview"
                      checked={showPartials}
                      disabled={anyBusy}
                      onChange={(next) =>
                        void live.save({ showPartials: next }, "showPartials")
                      }
                    />
                  </Row>
                  <Row
                    label="Type the preview if the second pass fails"
                    hint="Trades accuracy for not losing the recording: you get the rough live text instead of nothing, and nothing tells you it is the rough one — which is why it starts off. Needs the preview above switched on, since that is what produces the draft."
                  >
                    {live.savedKey === "fallback" && (
                      <span className="kea-saved">Saved ✓</span>
                    )}
                    <Toggle
                      label="Type the preview if the second pass fails"
                      checked={draftFallback}
                      disabled={anyBusy}
                      onChange={(next) => void live.save({ fallback: next }, "fallback")}
                    />
                  </Row>
                </>
              )}
            </RowGroup>
            {(streamingCatalogError || live.error) && (
              <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
                {streamingCatalogError ?? live.error}
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
          {holdToTalk && (
            <p className="kea-muted" style={{ marginTop: 0, fontSize: "0.8125rem" }}>
              Tap <KeyChips accelerator="Option+Shift" /> twice to keep recording
              hands-free. Tap again to finish, or press Esc to throw it away.
            </p>
          )}
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
                {STATE_LABELS[dictationState]}
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
