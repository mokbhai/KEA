import { useEffect, useState } from "react";
import { getEffectiveHotkey, getHotkeyRegistrationStatus, setHotkey, type HotkeyRegStatus } from "../api";
import {
  acceleratorPartLabel,
  parseAcceleratorParts,
  useHotkeyRecorder,
} from "./useHotkeyRecorder";

type Props = {
  feature: string;
  command: string;
  label?: string;
};

function KeyChips({ accelerator }: { accelerator: string }) {
  const parts = parseAcceleratorParts(accelerator);
  if (parts.length === 0) return null;

  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        gap: 6,
        alignItems: "center",
      }}
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
    </div>
  );
}

export default function HotkeyBinder({ feature, command, label = "Hotkey" }: Props) {
  const [savedAccelerator, setSavedAccelerator] = useState("");
  const [source, setSource] = useState<"custom" | "default" | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [regError, setRegError] = useState<string | null>(null);
  const {
    capturing,
    pending,
    captureError,
    startCapture,
    clearPending,
  } = useHotkeyRecorder();

  useEffect(() => {
    getEffectiveHotkey(feature, command)
      .then((value) => {
        if (value) {
          setSavedAccelerator(value.accelerator);
          setSource(value.source);
        }
      })
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
  }, [feature, command]);

  useEffect(() => {
    getHotkeyRegistrationStatus()
      .then((items: HotkeyRegStatus[]) => {
        const match = items.find(
          (s) => s.feature === feature && s.command === command,
        );
        setRegError(match && !match.ok ? match.error : null);
      })
      .catch(() => {});
  }, [feature, command]);

  const save = async (accelerator: string) => {
    if (!accelerator.trim()) return;
    setBusy(true);
    setStatus(null);
    try {
      await setHotkey(feature, command, accelerator.trim());
      setSavedAccelerator(accelerator.trim());
      setSource("custom");
      // Re-query registration status instead of optimistically clearing the
      // error — the re-registration may have failed asynchronously after
      // persisting the binding.
      try {
        const items: HotkeyRegStatus[] = await getHotkeyRegistrationStatus();
        const match = items.find(
          (s) => s.feature === feature && s.command === command,
        );
        setRegError(match && !match.ok ? match.error : null);
      } catch {
        setRegError(null);
      }
      clearPending();
      setStatus("Hotkey saved.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const displayedAccelerator = pending ?? savedAccelerator;
  const hasPending = pending !== null;

  return (
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <h3 style={{ margin: "0 0 12px" }}>{label}</h3>

      <div style={{ marginBottom: 12 }}>
        <span className="kea-label">Current shortcut</span>
        {displayedAccelerator ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <KeyChips accelerator={displayedAccelerator} />
            {source === "default" && (
              <span className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                (default)
              </span>
            )}
          </div>
        ) : (
          <p className="kea-muted" style={{ margin: 0 }}>
            No shortcut set.
          </p>
        )}
      </div>

      {capturing ? (
        <button type="button" className="kea-btn" disabled aria-live="polite">
          Press keys… (Esc to cancel)
        </button>
      ) : hasPending ? (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" }}>
          <button
            type="button"
            className="kea-btn kea-btn--primary"
            onClick={() => save(pending)}
            disabled={busy}
          >
            Save hotkey
          </button>
          <button
            type="button"
            className="kea-btn"
            onClick={startCapture}
            disabled={busy}
          >
            Re-record
          </button>
          <button
            type="button"
            className="kea-btn"
            onClick={clearPending}
            disabled={busy}
          >
            Cancel
          </button>
        </div>
      ) : (
        <button
          type="button"
          className="kea-btn"
          onClick={startCapture}
          disabled={busy}
        >
          Record shortcut
        </button>
      )}

      {captureError && (
        <p style={{ margin: "12px 0 0", fontSize: "0.8125rem", color: "var(--danger)" }}>
          {captureError}
        </p>
      )}

      {regError && (
        <p style={{ margin: "12px 0 0", fontSize: "0.8125rem", color: "var(--danger)" }}>
          Hotkey registration failed at startup: {regError}
        </p>
      )}

      {status && (
        <p className="kea-muted" style={{ marginTop: 12, marginBottom: 0 }}>
          {status}
        </p>
      )}
    </section>
  );
}
