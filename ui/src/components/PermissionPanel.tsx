import { useCallback, useEffect, useState } from "react";
import {
  getAllPermissionStatuses,
  openAccessibilitySettings,
  requestPermission,
  type PermStatus,
  type PermissionStatusItem,
} from "../api";

function statusColor(status: PermStatus): string {
  switch (status) {
    case "Granted":
      return "#16a34a";
    case "Denied":
      return "var(--danger)";
    default:
      return "#737373";
  }
}

function permissionLabel(kind: string): string {
  switch (kind) {
    case "microphone":
      return "Microphone";
    case "screen_recording":
      return "Screen Recording";
    case "accessibility":
      return "Accessibility";
    default:
      return kind;
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
      setPermsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setPermsBusy(false);
    }
  }, []);

  useEffect(() => {
    void refreshPerms();
  }, [refreshPerms]);

  const onRequestPermission = async (
    kind: "microphone" | "screen_recording" | "accessibility",
  ) => {
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
      setPermsStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setPermsBusy(false);
    }
  };

  return (
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          marginBottom: 12,
        }}
      >
        <h2 style={{ margin: 0 }}>Permissions</h2>
        <button
          type="button"
          className="kea-btn"
          onClick={refreshPerms}
          disabled={permsBusy}
        >
          Refresh
        </button>
      </div>
      <div style={{ display: "flex", gap: 12, flexWrap: "wrap", marginBottom: 16 }}>
        {perms.map((p) => (
          <span
            key={p.kind}
            style={{
              fontSize: 12,
              fontWeight: 600,
              padding: "4px 10px",
              borderRadius: 999,
              background: "var(--surface-2)",
              color: statusColor(p.status),
              border: "1px solid var(--border)",
            }}
          >
            {permissionLabel(p.kind)}: {p.status}
          </span>
        ))}
      </div>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          type="button"
          className="kea-btn"
          onClick={() => onRequestPermission("microphone")}
          disabled={permsBusy}
        >
          Request Microphone
        </button>
        <button
          type="button"
          className="kea-btn"
          onClick={() => onRequestPermission("screen_recording")}
          disabled={permsBusy}
        >
          Request Screen Recording
        </button>
        {perms.some((p) => p.kind === "accessibility" && p.status !== "Granted") && (
          <>
            <button
              type="button"
              className="kea-btn"
              onClick={() => onRequestPermission("accessibility")}
              disabled={permsBusy}
            >
              Request Accessibility
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => openAccessibilitySettings()}
              disabled={permsBusy}
            >
              Open System Settings
            </button>
            <span className="kea-muted" style={{ fontSize: 13, alignSelf: "center" }}>
              After granting, click Refresh.
            </span>
          </>
        )}
      </div>
      {permsStatus && (
        <p className="kea-muted" style={{ margin: "12px 0 0" }}>
          {permsStatus}
        </p>
      )}
    </section>
  );
}
