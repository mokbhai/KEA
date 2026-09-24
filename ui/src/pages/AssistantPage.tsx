import FeatureBanner from "../components/FeatureBanner";
import HotkeyRow from "../components/HotkeyRow";
import { Row, RowGroup } from "../components/SettingsRow";
import { useFeatureAi } from "../hooks/useFeatureAi";
import type { SlotSpec } from "../lib/featureSlot";
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
