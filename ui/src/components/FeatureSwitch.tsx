import { useEffect, useState, type ReactNode } from "react";
import { getFeatureEnabled, setFeatureEnabled, onFeatureSwitchChanged } from "../api";
import Toggle from "./Toggle";
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

export default function FeatureSwitch({
  feature,
  children,
}: {
  feature: Feature;
  children?: ReactNode;
}) {
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

  return <>
    <RowGroup aria-label={`${FEATURES[feature]} availability`}>
      <Row label={FEATURES[feature]} hint={enabled ? "Enabled" : "Turned off"}>
        <Toggle
          label={`Enable ${FEATURES[feature]}`}
          checked={enabled}
          disabled={busy}
          onChange={next => void change(next)}
        />
      </Row>
    </RowGroup>
    {error && <p role="alert">{error}</p>}
    {children && (enabled ? children : <div>
      <h1>{FEATURES[feature]}</h1>
      <p>Turned off. Enable {FEATURES[feature]} to use it.</p>
    </div>)}
  </>;
}

export function FeatureSwitchOverview() {
  return <section>
    <h2>Features</h2>
    {(Object.keys(FEATURES) as Feature[]).map(feature =>
      <FeatureSwitch key={feature} feature={feature} />,
    )}
  </section>;
}
