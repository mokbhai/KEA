import { useEffect, useState } from "react";
import {
  checkUpdate,
  getAutostart,
  getSetting,
  setAutostart,
  setSetting,
  showNotification,
} from "../api";
import PermissionPanel from "../components/PermissionPanel";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import { useSavedFlash } from "../hooks/useSavedFlash";
import { useTheme, type ThemePreference } from "../theme";

export default function GeneralPage({ onRunSetup }: { onRunSetup: () => void }) {
  const { preference, setPreference } = useTheme();
  const [savedKey, flash] = useSavedFlash();

  const [autostart, setAutostartEnabled] = useState(false);
  const [autostartBusy, setAutostartBusy] = useState(false);
  const [autostartError, setAutostartError] = useState<string | null>(null);

  const [autoCheck, setAutoCheck] = useState(true);
  const [autoCheckBusy, setAutoCheckBusy] = useState(false);

  const [updateBusy, setUpdateBusy] = useState(false);
  const [updateResult, setUpdateResult] = useState<string | null>(null);

  const [notifBusy, setNotifBusy] = useState(false);
  const [notifStatus, setNotifStatus] = useState<string | null>(null);

  const [setupBusy, setSetupBusy] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);

  useEffect(() => {
    getAutostart()
      .then(setAutostartEnabled)
      .catch((e) => setAutostartError(e instanceof Error ? e.message : String(e)));
    getSetting("updates.auto_check")
      .then((v) => {
        if (v === "false") setAutoCheck(false);
      })
      .catch(() => {});
  }, []);

  const onAppearanceChange = (next: ThemePreference) => {
    setPreference(next);
    flash("appearance");
  };

  const onAutostartChange = async (enabled: boolean) => {
    const previous = autostart;
    setAutostartEnabled(enabled);
    setAutostartBusy(true);
    setAutostartError(null);
    try {
      await setAutostart(enabled);
      flash("autostart");
    } catch (e) {
      setAutostartEnabled(previous);
      setAutostartError(e instanceof Error ? e.message : String(e));
    } finally {
      setAutostartBusy(false);
    }
  };

  const onAutoCheckChange = async (enabled: boolean) => {
    const previous = autoCheck;
    setAutoCheck(enabled);
    setAutoCheckBusy(true);
    try {
      await setSetting("updates.auto_check", enabled ? "true" : "false");
      flash("auto-check");
    } catch {
      setAutoCheck(previous);
    } finally {
      setAutoCheckBusy(false);
    }
  };

  const onCheckNow = async () => {
    setUpdateBusy(true);
    setUpdateResult(null);
    try {
      const status = await checkUpdate();
      if (status.status === "up-to-date") {
        setUpdateResult("KEA is up to date.");
      } else if (status.status === "available") {
        setUpdateResult(`Update available: v${status.version ?? "?"}`);
      } else {
        setUpdateResult(status.error ?? "Unknown status");
      }
    } catch (e) {
      setUpdateResult(e instanceof Error ? e.message : String(e));
    } finally {
      setUpdateBusy(false);
    }
  };

  const onSendTestNotification = async () => {
    setNotifBusy(true);
    setNotifStatus(null);
    try {
      await showNotification("KEA", "Hello from KEA! This is a test notification.");
      setNotifStatus("Test notification sent.");
    } catch (e) {
      setNotifStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setNotifBusy(false);
    }
  };

  const onRunSetupClick = async () => {
    setSetupBusy(true);
    setSetupError(null);
    try {
      await setSetting("onboarding.completed", "false");
      onRunSetup();
    } catch (e) {
      setSetupError(e instanceof Error ? e.message : String(e));
    } finally {
      setSetupBusy(false);
    }
  };

  return (
    <div>
      <h2 style={{ marginTop: 0 }}>General</h2>
      <p className="kea-muted" style={{ marginBottom: 24 }}>
        Appearance, startup, updates, notifications and permissions.
      </p>

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Appearance</h3>
        <RowGroup aria-label="Appearance">
          <Row label="Appearance" hint="Follow this Mac or pick a theme">
            {savedKey === "appearance" && <span className="kea-saved">Saved ✓</span>}
            <select
              className="kea-select"
              aria-label="Appearance"
              value={preference}
              onChange={(e) => onAppearanceChange(e.target.value as ThemePreference)}
            >
              <option value="system">System</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </Row>
        </RowGroup>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Startup & updates</h3>
        <RowGroup aria-label="Startup and updates">
          <Row
            label="Launch at login"
            hint={autostartError ?? "Start KEA when you log in"}
          >
            {savedKey === "autostart" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              checked={autostart}
              onChange={(next) => void onAutostartChange(next)}
              label="Launch KEA at login"
              disabled={autostartBusy}
            />
          </Row>
          <Row label="Check for updates automatically">
            {savedKey === "auto-check" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              checked={autoCheck}
              onChange={(next) => void onAutoCheckChange(next)}
              label="Check for updates automatically"
              disabled={autoCheckBusy}
            />
          </Row>
          <Row label="Updates" hint={updateResult ?? undefined}>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onCheckNow()}
              disabled={updateBusy}
            >
              Check now
            </button>
          </Row>
        </RowGroup>
      </section>

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Notifications</h3>
        <RowGroup aria-label="Notifications">
          <Row label="Test notifications" hint={notifStatus ?? "Send a sample notification"}>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onSendTestNotification()}
              disabled={notifBusy}
            >
              Send test
            </button>
          </Row>
        </RowGroup>
      </section>

      <PermissionPanel />

      <section style={{ marginBottom: 24 }}>
        <h3 style={{ margin: "0 0 12px" }}>Setup assistant</h3>
        <RowGroup aria-label="Setup assistant">
          <Row
            label="First-run setup"
            hint={setupError ?? "Re-run permissions, AI and hotkey setup"}
          >
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onRunSetupClick()}
              disabled={setupBusy}
            >
              Run again
            </button>
          </Row>
        </RowGroup>
      </section>
    </div>
  );
}
