import { useEffect, useState } from "react";
import { getEffectiveHotkey, setHotkey, type EffectiveHotkey } from "../../api";
import {
  acceleratorPartLabel,
  parseAcceleratorParts,
  useHotkeyRecorder,
} from "../useHotkeyRecorder";
import Banner from "../Banner";
import { Row, RowGroup } from "../SettingsRow";

const FEATURE_HOTKEYS = [
  {
    feature: "rewrite",
    command: "rewrite_selection",
    label: "Rewrite",
    hint: "Rewrites the text you have selected.",
  },
  {
    feature: "dictation",
    command: "push_to_talk",
    label: "Dictation",
    hint: "Hold to talk, release to type what you said.",
  },
  {
    feature: "meetings",
    command: "toggle_meeting",
    label: "Meetings",
    hint: "Starts or stops meeting notes.",
  },
  {
    feature: "tts",
    command: "read_selection",
    label: "Read aloud",
    hint: "Reads the selected text out loud.",
  },
] as const;

function formatAccelerator(accelerator: string): string {
  return parseAcceleratorParts(accelerator).map(acceleratorPartLabel).join("");
}

function KeyChips({ accelerator }: { accelerator: string }) {
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

function HotkeyRow({
  feature,
  command,
  label,
  hint,
  onSaved,
}: {
  feature: string;
  command: string;
  label: string;
  hint: string;
  onSaved?: (accelerator: string) => void;
}) {
  const [current, setCurrent] = useState<EffectiveHotkey | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { capturing, pending, captureError, startCapture, clearPending } = useHotkeyRecorder();

  useEffect(() => {
    getEffectiveHotkey(feature, command)
      .then(setCurrent)
      .catch(() => setCurrent(null));
  }, [feature, command]);

  // Save immediately once a combo is captured — same save-on-change rows as
  // the feature pages.
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

  return (
    <Row label={label} hint={error ?? captureError ?? hint}>
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

export default function HotkeysStep() {
  // The raw Rewrite accelerator; the row below updates it after re-recording
  // so the "try it now" line never shows a stale combo.
  const [rewriteAccel, setRewriteAccel] = useState<string | null>(null);

  useEffect(() => {
    getEffectiveHotkey("rewrite", "rewrite_selection")
      .then((hk) => setRewriteAccel(hk ? hk.accelerator : null))
      .catch(() => setRewriteAccel(null));
  }, []);

  return (
    <div>
      <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
        These shortcuts work anywhere on your Mac. Change one by re-recording it.
      </p>
      <RowGroup aria-label="Hotkeys">
        {FEATURE_HOTKEYS.map((f) => (
          <HotkeyRow
            key={`${f.feature}/${f.command}`}
            feature={f.feature}
            command={f.command}
            label={f.label}
            hint={f.hint}
            onSaved={f.feature === "rewrite" ? setRewriteAccel : undefined}
          />
        ))}
      </RowGroup>
      <div style={{ marginTop: 16 }}>
        <Banner variant="ok">
          You're ready! Try it now: select some text in any app and press{" "}
          <strong>{rewriteAccel ? formatAccelerator(rewriteAccel) : "the Rewrite shortcut"}</strong>{" "}
          to rewrite it.
        </Banner>
      </div>
    </div>
  );
}
