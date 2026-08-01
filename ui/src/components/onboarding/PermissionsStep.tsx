import { useCallback, useEffect, useState } from "react";
import {
  getAllPermissionStatuses,
  openAccessibilitySettings,
  requestPermission,
  type PermStatus,
} from "../../api";
import { Row, RowGroup } from "../SettingsRow";

type PermKind = "microphone" | "screen_recording" | "accessibility";

const PERMISSIONS: { kind: PermKind; label: string; hint: string }[] = [
  {
    kind: "microphone",
    label: "Microphone",
    hint: "So KEA can hear you when you dictate or record a meeting.",
  },
  {
    kind: "screen_recording",
    label: "Screen recording",
    hint: "Lets meetings capture what other people say in calls.",
  },
  {
    kind: "accessibility",
    label: "Accessibility",
    hint: "Lets KEA read the text you select and type the result back.",
  },
];

export default function PermissionsStep() {
  const [statuses, setStatuses] = useState<Partial<Record<PermKind, PermStatus>>>({});
  const [busyKind, setBusyKind] = useState<PermKind | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const items = await getAllPermissionStatuses();
      setStatuses(() => {
        const next: Partial<Record<PermKind, PermStatus>> = {};
        items.forEach((item) => {
          next[item.kind as PermKind] = item.status;
        });
        return next;
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  // Statuses refresh on mount and every time the window regains focus, so
  // granting a permission in System Settings shows up without a manual step.
  useEffect(() => {
    void refresh();
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  const grant = async (kind: PermKind) => {
    setBusyKind(kind);
    setError(null);
    try {
      const result = await requestPermission(kind);
      setStatuses((prev) => ({ ...prev, [kind]: result }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyKind(null);
    }
  };

  return (
    <div>
      <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
        KEA needs a few macOS permissions to work. You can grant them later,
        but features stay limited until you do.
      </p>
      <RowGroup aria-label="Permissions">
        {PERMISSIONS.map(({ kind, label, hint }) => {
          const status = statuses[kind];
          const granted = status === "Granted";
          return (
            <Row key={kind} label={label} hint={hint}>
              <span
                className={`kea-dot ${granted ? "kea-dot--ok" : "kea-dot--warn"}`}
                aria-hidden="true"
              />
              {granted ? (
                <span className="kea-saved">Granted ✓</span>
              ) : (
                <>
                  <button
                    type="button"
                    className="kea-btn"
                    onClick={() => void grant(kind)}
                    disabled={busyKind !== null}
                  >
                    Grant
                  </button>
                  {kind === "accessibility" && (
                    <button
                      type="button"
                      className="kea-btn"
                      onClick={() => void openAccessibilitySettings()}
                      disabled={busyKind !== null}
                    >
                      Open System Settings
                    </button>
                  )}
                </>
              )}
            </Row>
          );
        })}
      </RowGroup>
      {error && (
        <p className="kea-muted" style={{ margin: "12px 0 0" }}>
          {error}
        </p>
      )}
    </div>
  );
}
