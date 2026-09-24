import { useEffect, useRef } from "react";
import { getAssistantSettings, setAssistantSettings, type AssistantSettings } from "../api";
import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import { useFeatureAi } from "../hooks/useFeatureAi";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import type { SlotSpec } from "../lib/featureSlot";
import { toMessage } from "../lib/format";
import type { Navigate } from "../lib/nav";

const ASSISTANT_FEATURE = "assistant";
const ASSISTANT_COMMAND = "ask";

/**
 * Three slots, because one request uses all three engine kinds: speech in, a
 * model to answer or route, and speech out. Listed in the order a request
 * travels through them, which is also the order they fail in.
 */
const SLOTS: SlotSpec[] = [
  { feature: ASSISTANT_FEATURE, slot: "stt", capability: "stt", label: "Speech to text" },
  { feature: ASSISTANT_FEATURE, slot: "llm", capability: "llm", label: "Answers" },
  { feature: ASSISTANT_FEATURE, slot: "tts", capability: "tts", label: "Voice" },
];

/**
 * Both switches on, which is what the backend reads for a row that was never
 * written (`kea_core::assistant::settings`). Shown while the mount fetch is in
 * flight: the alternative, an empty pair of rows until it lands, would flicker
 * a state the user never chose onto a page whose whole content is two answers.
 */
const OUTPUTS_ON: AssistantSettings = { speak_answers: true, show_answers: true };

/** What the assistant can do besides answer, in the order the router sees them. */
const ACTIONS: { title: string; when: string }[] = [
  {
    title: "Read what's on screen",
    when: "“What does this say?” — reads your selection, the window you're in, or falls back to a screenshot.",
  },
  { title: "Open an application", when: "“Open Mail.”" },
  { title: "Start a meeting recording", when: "“Start recording this meeting.”" },
  { title: "Rewrite the selection", when: "“Make this more formal.”" },
];

type Props = {
  onNavigate?: Navigate;
};

export default function AssistantPage({ onNavigate }: Props) {
  const ai = useFeatureAi(SLOTS);

  const outputs = useOptimisticSetting<AssistantSettings>({
    initial: OUTPUTS_ON,
    persist: setAssistantSettings,
    // A save writes both switches at once, so re-reading first is what stops
    // one toggle from reverting whatever the other was just set to elsewhere.
    reread: getAssistantSettings,
  });
  const settings = outputs.value;
  const { setValue: setOutputs, setError: setOutputsError } = outputs;
  // Set once the user flips a switch, so a slow mount fetch cannot land on top
  // of the choice they made while it was in flight.
  const touched = useRef(false);

  useEffect(() => {
    getAssistantSettings()
      .then((loaded) => {
        if (!touched.current) setOutputs(loaded);
      })
      .catch((e) => setOutputsError(toMessage(e)));
  }, [setOutputs, setOutputsError]);

  const saveOutput = (patch: Partial<AssistantSettings>, key: string) => {
    touched.current = true;
    return outputs.save(patch, key);
  };

  return (
    <div>
      <header>
        <h1 style={{ marginTop: 0 }}>Assistant</h1>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 24 }}>
          Press the shortcut, ask a question, and hear a short answer. Stop
          talking and it answers — there is no key to release.
        </p>
      </header>

      <FeatureBanner ai={ai} onNavigate={onNavigate} />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Behavior</h2>
        <RowGroup aria-label="Assistant behavior">
          <HotkeyRow
            feature={ASSISTANT_FEATURE}
            command={ASSISTANT_COMMAND}
            label="Shortcut"
            hint="Opens the assistant and starts listening."
            checkRegistration
          />
          <Row
            label="Cancel"
            hint="Registered only while a session is open, so it stays available to every other app the rest of the time."
          >
            <kbd className="kea-kbd">Esc</kbd>
          </Row>
        </RowGroup>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 4px" }}>Answers</h2>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 12 }}>
          Two independent switches, because the useful settings are at both
          ends: spoken and not shown for an answer you asked for without
          looking, shown and not spoken for one you asked for in a room full of
          people.
        </p>
        <RowGroup aria-label="Assistant answers">
          <Row
            label="Speak answers"
            hint="Reads the answer aloud in the voice below. With this off, nothing is synthesized at all."
          >
            {outputs.savedKey === "speak_answers" && (
              <span className="kea-saved">Saved ✓</span>
            )}
            <Toggle
              label="Speak answers"
              checked={settings.speak_answers}
              disabled={outputs.busy}
              onChange={(next) => void saveOutput({ speak_answers: next }, "speak_answers")}
            />
          </Row>
          <Row
            label="Show answers"
            hint="Puts the answer text in the floating panel. With this off the panel still shows what it heard and what it read, just not the reply."
          >
            {outputs.savedKey === "show_answers" && (
              <span className="kea-saved">Saved ✓</span>
            )}
            <Toggle
              label="Show answers"
              checked={settings.show_answers}
              disabled={outputs.busy}
              onChange={(next) => void saveOutput({ show_answers: next }, "show_answers")}
            />
          </Row>
        </RowGroup>
        {/* Allowed, because a user who wants the actions and not the answers is
            entitled to that — but said out loud, since the alternative is
            silently asking a model a question whose answer goes nowhere. */}
        {!settings.speak_answers && !settings.show_answers && (
          <p className="kea-muted" style={{ marginTop: 8 }}>
            With both off, an answer is neither spoken nor shown — the assistant
            still runs what you ask it to do.
          </p>
        )}
        {outputs.error && (
          <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
            {outputs.error}
          </p>
        )}
      </section>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 4px" }}>What it can do</h2>
        <p className="kea-muted" style={{ marginTop: 0, marginBottom: 12 }}>
          Anything else is answered as a question. Nothing the assistant can do
          changes or deletes your work, so a misheard request costs you an
          irrelevant answer and nothing more.
        </p>
        <RowGroup aria-label="Assistant actions">
          {ACTIONS.map((action) => (
            <Row key={action.title} label={action.title} hint={action.when}>
              <span />
            </Row>
          ))}
        </RowGroup>
      </section>
    </div>
  );
}
