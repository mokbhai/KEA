import { useEffect, useState } from "react";
import {
  getEffectiveHotkey,
  getHotkeyRegistrationStatus,
  setHotkey,
  type EffectiveHotkey,
  type HotkeyRegStatus,
} from "../api";
import {
  acceleratorPartLabel,
  parseAcceleratorParts,
  useHotkeyRecorder,
} from "./useHotkeyRecorder";
import { Row } from "./SettingsRow";

export function formatAccelerator(accelerator: string): string {
  return parseAcceleratorParts(accelerator).map(acceleratorPartLabel).join("");
}

export function KeyChips({ accelerator }: { accelerator: string }) {
  const parts = parseAcceleratorParts(accelerator);
  if (parts.length === 0) return null;
  return (
    <span
      style={{ display: "inline-flex", flexWrap: "wrap", gap: 6, alignItems: "center" }}
      aria-label={`Shortcut ${accelerator}`}
    >
      {parts.map((part, index) => (
        <span
          key={`${part}-${index}`}
          style={{
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            minWidth: 28,
            padding: "4px 8px",
            fontSize: "0.8125rem",
            fontWeight: 600,
            color: "var(--text)",
            background: "var(--surface-2)",
            border: "1px solid var(--border)",
            borderRadius: 6,
            lineHeight: 1.2,
          }}
        >
          {acceleratorPartLabel(part)}
        </span>
      ))}
    </span>
  );
}

type Props = {
  feature: string;
  command: string;
  label: string;
  hint: string;
  onSaved?: (accelerator: string) => void;
  /**
   * Also surface a startup registration failure for this hotkey (another app
   * already owns the combo). The wizard leaves this off; feature pages turn it
   * on so the error stays visible where the hotkey is configured.
   */
  checkRegistration?: boolean;
};

/**
 * A hotkey as a settings row with inline re-record — the shared idiom for the
 * wizard's Hotkeys step and every feature page's Behavior card. Saves the
 * moment a combo is captured; no separate Save button.
 */
export default function HotkeyRow({
  feature,
  command,
  label,
  hint,
  onSaved,
  checkRegistration = false,
}: Props) {
  const [current, setCurrent] = useState<EffectiveHotkey | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [regError, setRegError] = useState<string | null>(null);
  const { capturing, pending, captureError, startCapture, clearPending } = useHotkeyRecorder();

  useEffect(() => {
    getEffectiveHotkey(feature, command)
      .then(setCurrent)
      .catch(() => setCurrent(null));
  }, [feature, command]);

  useEffect(() => {
    if (!checkRegistration) return;
    getHotkeyRegistrationStatus()
      .then((items: HotkeyRegStatus[]) => {
        const match = items.find((s) => s.feature === feature && s.command === command);
        setRegError(match && !match.ok ? match.error : null);
      })
      .catch(() => {});
  }, [feature, command, checkRegistration, saved]);

  // Save immediately once a combo is captured — same save-on-change rows as
  // the rest of the app.
  useEffect(() => {
    if (!pending) return;
    let cancelled = false;
    setError(null);
    setHotkey(feature, command, pending)
      .then(() => {
        if (cancelled) return;
        setCurrent({ accelerator: pending, source: "custom" });
        setSaved(true);
        clearPending();
        onSaved?.(pending);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        clearPending();
      });
    return () => {
      cancelled = true;
    };
  }, [pending, feature, command, clearPending, onSaved]);

  const registrationHint = regError
    ? `Shortcut registration failed at startup: ${regError}`
    : null;

  return (
    <Row label={label} hint={error ?? captureError ?? registrationHint ?? hint}>
      {capturing ? (
        <span className="kea-muted" aria-live="polite">
          Press keys… (Esc to cancel)
        </span>
      ) : current ? (
        <KeyChips accelerator={current.accelerator} />
      ) : (
        <span className="kea-muted">Not set</span>
      )}
      {saved && !capturing && <span className="kea-saved">Saved ✓</span>}
      <button type="button" className="kea-btn" onClick={startCapture} disabled={capturing}>
        Re-record
      </button>
    </Row>
  );
}
