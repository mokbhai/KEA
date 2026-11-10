import Section from "./Section";
import { Row, RowGroup } from "./SettingsRow";
import { useCallback, useEffect, useState } from "react";
import {
  getAllPermissionStatuses,
  openAccessibilitySettings,
  requestPermission,
  type PermStatus,
  type PermissionKind,
  type PermissionStatusItem,
} from "../api";
import { toMessage } from "../lib/format";

function permissionLabel(kind: string): string {
  switch (kind) {
    case "microphone":
      return "Microphone";
    case "screen_recording":
      return "Screen Recording";
    case "accessibility":
      return "Accessibility";
    case "calendar":
      return "Calendar";
    case "speech":
      return "Speech Recognition";
    default:
      return kind;
  }
}

/** The row's second line: the state in plain words, and what to do about it. */
function statusHint(status: PermStatus): string {
  switch (status) {
    case "Granted":
      return "Allowed";
    case "Denied":
      return "Not allowed. Turn it on in System Settings, under Privacy & Security.";
    default:
      return "Not asked yet";
  }
}

export default function PermissionPanel() {
  const [perms, setPerms] = useState<PermissionStatusItem[]>([]);
  const [permsBusy, setPermsBusy] = useState(false);
  const [permsStatus, setPermsStatus] = useState<string | null>(null);

  const refreshPerms = useCallback(async () => {
    setPermsBusy(true);
    setPermsStatus(null);
    try {
      const items = await getAllPermissionStatuses();
      setPerms(items);
    } catch (e) {
      setPermsStatus(toMessage(e));
    } finally {
      setPermsBusy(false);
    }
  }, []);

  useEffect(() => {
    void refreshPerms();
  }, [refreshPerms]);

  const onRequestPermission = async (kind: PermissionKind) => {
    setPermsBusy(true);
    setPermsStatus(null);
    try {
      const result = await requestPermission(kind);
      setPerms((prev) =>
        prev.map((p) => (p.kind === kind ? { ...p, status: result } : p)),
      );
      setPermsStatus(
        result === "Granted"
          ? `${permissionLabel(kind)} permission granted.`
          : `${permissionLabel(kind)} permission not granted — check System Settings.`,
      );
    } catch (e) {
      setPermsStatus(toMessage(e));
    } finally {
      setPermsBusy(false);
    }
  };

  return (
    <Section title="Permissions" actions={
      <button type="button" className="kea-btn" onClick={refreshPerms} disabled={permsBusy}>Refresh</button>
    }>
      <RowGroup aria-label="Permissions">
        {perms.map((p) => (
          <Row
            key={p.kind}
            label={permissionLabel(p.kind)}
            hint={
              p.kind === "accessibility" && p.status !== "Granted"
                ? `${statusHint(p.status)} After granting, click Refresh.`
                : statusHint(p.status)
            }
            tone={p.status === "Denied" ? "danger" : "muted"}
          >
            {/* Nothing to ask for once it is allowed: a granted row is just its state. */}
            {p.status !== "Granted" &&
              (p.kind === "microphone" ||
                p.kind === "screen_recording" ||
                p.kind === "accessibility") && (
                <button
                  type="button"
                  className="kea-btn"
                  aria-label={`Request ${permissionLabel(p.kind)}`}
                  onClick={() => onRequestPermission(p.kind as PermissionKind)}
                  disabled={permsBusy}
                >
                  Request
                </button>
              )}
            {p.kind === "accessibility" && p.status !== "Granted" && (
              <button
                type="button"
                className="kea-btn"
                onClick={() => openAccessibilitySettings()}
                disabled={permsBusy}
              >
                Open System Settings
              </button>
            )}
          </Row>
        ))}
      </RowGroup>
      {permsStatus && (
        <p className="kea-muted" style={{ margin: "12px 0 0" }}>
          {permsStatus}
        </p>
      )}
    </Section>
  );
}
