import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { getFeatureEnabled, setFeatureEnabled, onFeatureSwitchChanged } from "../api";
import Toggle from "./Toggle";
import PageHeader from "./PageHeader";
import Section from "./Section";
import { Row, RowGroup } from "./SettingsRow";

export const FEATURES = {
  rewrite: "Rewrite",
  dictation: "Dictation",
  meetings: "Meetings",
  transcribe: "Transcribe",
  tts: "Read aloud",
  assistant: "Assistant",
} as const;
export type Feature = keyof typeof FEATURES;

/** What each feature does, said once: under its page title, beside its switch. */
export const FEATURE_SUMMARIES: Record<Feature, string> = {
  rewrite: "Select text anywhere on your Mac and press the shortcut to rewrite it in place.",
  dictation: "Hold the shortcut anywhere on your Mac, talk, and KEA types what you said.",
  meetings: "Record a meeting, watch the transcript appear live, and get notes when you stop.",
  transcribe:
    "Drop an audio or video recording here to get a timed transcript you can export as subtitles.",
  tts: "Select text anywhere on your Mac and press the shortcut to hear it read out loud.",
  assistant:
    "Press the shortcut, ask a question, and hear a short answer. Stop talking and it answers — there is no key to release.",
};

type SwitchState = {
  feature: Feature;
  enabled: boolean;
  busy: boolean;
  error: string | null;
  change: (next: boolean) => void;
};

function useFeatureSwitch(feature: Feature): SwitchState {
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    const refresh = () => getFeatureEnabled(feature)
      .then(value => { if (active) setEnabled(value !== false); })
      .catch(e => { if (active) setError(String(e)); });
    void refresh();
    const unlisten = onFeatureSwitchChanged(changed => {
      if (changed === feature) void refresh();
    });
    return () => {
      active = false;
      void unlisten.then(fn => fn()).catch(() => {});
    };
  }, [feature]);

  async function change(next: boolean) {
    setBusy(true);
    setError(null);
    try {
      await setFeatureEnabled(feature, next);
      setEnabled(next);
    } catch (e) {
      setError(String(e));
      // A shortcut registration failure still persists the master switch.
      const current = await getFeatureEnabled(feature).catch(() => enabled);
      setEnabled(current !== false);
    } finally {
      setBusy(false);
    }
  }

  return { feature, enabled, busy, error, change: next => void change(next) };
}

function SwitchToggle({ state }: { state: SwitchState }) {
  return (
    <Toggle
      label={`Enable ${FEATURES[state.feature]}`}
      checked={state.enabled}
      disabled={state.busy}
      onChange={state.change}
    />
  );
}

const FeatureSwitchContext = createContext<SwitchState | null>(null);

/**
 * The top of a feature page: its title, then the switch that turns the feature
 * on, with what the feature does as the switch's own description. The switch
 * sits under the title because it belongs to the page the title names; above
 * it, it read as a control for the whole window.
 *
 * `actions` sit on the title's line, for a page whose main control lives there.
 */
export function FeaturePageHeader({ actions }: { actions?: ReactNode }) {
  const state = useContext(FeatureSwitchContext);
  if (!state) throw new Error("FeaturePageHeader must be rendered inside a FeatureSwitch");
  const name = FEATURES[state.feature];

  return (
    <PageHeader title={name} actions={actions}>
      <RowGroup aria-label={`${name} availability`}>
        <Row label={`Enable ${name}`} hint={FEATURE_SUMMARIES[state.feature]}>
          <SwitchToggle state={state} />
        </Row>
      </RowGroup>
      {state.error && <p role="alert" className="kea-page-header__error">{state.error}</p>}
    </PageHeader>
  );
}

/** Wraps a feature page: its content shows only while the feature is on. */
export default function FeatureSwitch({
  feature,
  children,
}: {
  feature: Feature;
  children: ReactNode;
}) {
  const state = useFeatureSwitch(feature);

  return (
    <FeatureSwitchContext.Provider value={state}>
      {state.enabled ? children : (
        <div>
          <FeaturePageHeader />
          <p className="kea-muted" style={{ margin: 0 }}>
            {FEATURES[feature]} is turned off. Its shortcut and settings come back when
            you turn it on.
          </p>
        </div>
      )}
    </FeatureSwitchContext.Provider>
  );
}

function OverviewRow({ feature }: { feature: Feature }) {
  const state = useFeatureSwitch(feature);
  return (
    <Row
      label={FEATURES[feature]}
      hint={state.error ?? (state.enabled ? "Enabled" : "Turned off")}
      tone={state.error ? "danger" : "muted"}
    >
      <SwitchToggle state={state} />
    </Row>
  );
}

/** Every feature's switch in one list, for the General page. */
export function FeatureSwitchOverview() {
  return (
    <Section title="Features">
      <RowGroup aria-label="Features">
        {(Object.keys(FEATURES) as Feature[]).map(feature =>
          <OverviewRow key={feature} feature={feature} />,
        )}
      </RowGroup>
    </Section>
  );
}
