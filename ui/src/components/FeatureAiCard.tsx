import { useState } from "react";
import { deleteBinding } from "../api";
import DefaultsPicker from "./DefaultsPicker";
import { Row, RowGroup } from "./SettingsRow";
import Spinner from "./Spinner";
import { useSavedFlash } from "../hooks/useSavedFlash";
import type { FeatureAi } from "../hooks/useFeatureAi";
import type { SlotStatus } from "../lib/featureSlot";

type Props = {
  ai: FeatureAi;
  /** Card heading; defaults to "AI". */
  title?: string;
  /** What this feature is called inside the picker heading. */
  featureLabel: string;
};

const keyOf = (status: SlotStatus) => `${status.spec.feature}/${status.spec.slot}`;

function hintFor(status: SlotStatus): string {
  if (!status.summary) return "Not set";
  if (status.source === "override") return `This feature only — ${status.summary}`;
  return `Using default — ${status.summary}`;
}

/**
 * The shared "AI" card: what each slot currently uses in plain language, a
 * change action that writes a feature-scoped override, a way back to the
 * shared default, and raw ids only under Advanced.
 */
export default function FeatureAiCard({ ai, title = "AI", featureLabel }: Props) {
  const [savedKey, flash] = useSavedFlash();
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const useDefaultAgain = async (status: SlotStatus) => {
    const key = keyOf(status);
    setBusyKey(key);
    setError(null);
    try {
      await deleteBinding(status.spec.feature, status.spec.slot);
      flash(key);
      ai.reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyKey(null);
    }
  };

  return (
    <section style={{ marginBottom: 24 }}>
      <h3 style={{ margin: "0 0 12px" }}>{title}</h3>
      {ai.statuses === null ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 44 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading…</span>
        </div>
      ) : (
        <>
          <RowGroup aria-label={`${featureLabel} AI`}>
            {ai.statuses.map((status) => {
              const key = keyOf(status);
              return (
                <Row key={key} label={status.spec.label} hint={hintFor(status)}>
                  {!status.summary && (
                    <>
                      <span className="kea-dot kea-dot--warn" aria-hidden="true" />
                      <span className="kea-muted">Not set</span>
                    </>
                  )}
                  {savedKey === key && <span className="kea-saved">Saved ✓</span>}
                  <button
                    type="button"
                    className="kea-btn"
                    onClick={() => ai.openPicker(status.spec)}
                  >
                    {status.summary ? "Change…" : "Choose…"}
                  </button>
                  {status.override && (
                    <button
                      type="button"
                      className="kea-btn"
                      disabled={busyKey === key}
                      onClick={() => void useDefaultAgain(status)}
                    >
                      Use default again
                    </button>
                  )}
                </Row>
              );
            })}
          </RowGroup>

          <details className="kea-advanced">
            <summary>Advanced</summary>
            <div className="kea-advanced__body">
              {ai.statuses.map((status) => (
                <div key={keyOf(status)} className="kea-muted" style={{ fontSize: "0.8125rem" }}>
                  <strong style={{ color: "var(--text)" }}>{status.spec.label}</strong> —{" "}
                  engine <code>{status.effective?.engine_id ?? "—"}</code> · model{" "}
                  <code>{status.effective?.model ?? "—"}</code> · provider{" "}
                  <code>{status.effective?.provider_ref ?? "—"}</code> · source{" "}
                  <code>{status.source}</code> · binding{" "}
                  <code>
                    {status.spec.feature}/{status.spec.slot}
                  </code>
                </div>
              ))}
            </div>
          </details>

          {error && (
            <p style={{ marginTop: 8, fontSize: "0.8125rem", color: "var(--danger)" }}>
              {error}
            </p>
          )}
        </>
      )}

      {ai.pickerSpec && (
        <DefaultsPicker
          capability={ai.pickerSpec.capability}
          target={{ feature: ai.pickerSpec.feature, slot: ai.pickerSpec.slot }}
          title={`${ai.pickerSpec.label} for ${featureLabel}`}
          open
          onClose={ai.closePicker}
          onApplied={() => ai.reload()}
        />
      )}
    </section>
  );
}
