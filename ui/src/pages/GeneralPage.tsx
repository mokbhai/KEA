import { useEffect, useState } from "react";
import {
  checkUpdate,
  getApiSettings,
  getAutostart,
  getSetting,
  installCliShim,
  regenerateApiToken,
  revealApiToken,
  setApiEnabled,
  setApiRateLimit,
  setAutostart,
  setSetting,
  showNotification,
  type ApiSettings,
} from "../api";
import PermissionPanel from "../components/PermissionPanel";
import { Row, RowGroup } from "../components/SettingsRow";
import Toggle from "../components/Toggle";
import { useOptimisticSetting } from "../hooks/useOptimisticSetting";
import { useSavedFlash } from "../hooks/useSavedFlash";
import { toMessage } from "../lib/format";
import { useTheme, type ThemePreference } from "../theme";

/**
 * What the API row shows until the mount fetch lands: off, and not running.
 * Anything else would flash "listening" at a user whose API is disabled.
 */
const API_OFF: ApiSettings = {
  enabled: false,
  running: false,
  socket_path: "",
  token_file: "",
  max_rewrites_per_minute: 20,
  supported: true,
};

/**
 * The one paragraph that has to be on screen before anyone turns this on.
 *
 * It is deliberately about capability, not reassurance: the token is worth
 * exactly what the app can do to the focused window, and the honest boundary
 * is "another user on this machine, or a web page", not "malware running as
 * you".
 */
const API_THREAT_MODEL =
  "Anything holding this token can type text into whatever app you have focused, read your " +
  "current selection, open your microphone and spend your LLM API credits. KEA listens on a " +
  "file-permission-protected socket (not a network port), so no web page can reach it and no " +
  "other account on this Mac can either — but any program running as you can.";

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

  const [soundCues, setSoundCues] = useState(true);
  const [soundCuesBusy, setSoundCuesBusy] = useState(false);

  const [setupBusy, setSetupBusy] = useState(false);
  const [setupError, setSetupError] = useState<string | null>(null);

  const api = useOptimisticSetting<ApiSettings>({
    initial: API_OFF,
    // Only `enabled` is persisted here; the rest of the object is what the
    // backend reports back, and is re-read below.
    persist: (next) => setApiEnabled(next.enabled),
  });
  const { setValue: setApiValue, setError: setApiError } = api;
  const [apiToken, setApiToken] = useState<string | null>(null);
  const [apiTokenBusy, setApiTokenBusy] = useState(false);
  const [rateLimit, setRateLimit] = useState("20");
  const [cliStatus, setCliStatus] = useState<string | null>(null);
  const [cliBusy, setCliBusy] = useState(false);

  useEffect(() => {
    getAutostart()
      .then(setAutostartEnabled)
      .catch((e) => setAutostartError(toMessage(e)));
    getSetting("updates.auto_check")
      .then((v) => {
        if (v === "false") setAutoCheck(false);
      })
      .catch(() => {});
    getSetting("sound.cues_enabled")
      .then((v) => {
        if (v === "false") setSoundCues(false);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    getApiSettings()
      .then((settings) => {
        setApiValue(settings);
        setRateLimit(String(settings.max_rewrites_per_minute));
      })
      .catch((e) => setApiError(toMessage(e)));
  }, [setApiValue, setApiError]);

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
      setAutostartError(toMessage(e));
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

  const onSoundCuesChange = async (enabled: boolean) => {
    const previous = soundCues;
    setSoundCues(enabled);
    setSoundCuesBusy(true);
    try {
      await setSetting("sound.cues_enabled", enabled ? "true" : "false");
      flash("sound-cues");
    } catch {
      setSoundCues(previous);
    } finally {
      setSoundCuesBusy(false);
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
      setUpdateResult(toMessage(e));
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
      setNotifStatus(toMessage(e));
    } finally {
      setNotifBusy(false);
    }
  };

  const onApiToggle = async (enabled: boolean) => {
    await api.save({ enabled }, "local-api");
    // A token on screen for an API that is now off is a secret with no
    // purpose; hide it again rather than leaving it up.
    if (!enabled) setApiToken(null);
    // `running` and the socket path are the backend's to report — a bind that
    // failed must not leave the row claiming the socket is up.
    try {
      setApiValue(await getApiSettings());
    } catch {
      // The save already reported anything that went wrong.
    }
  };

  const onRevealToken = async () => {
    setApiTokenBusy(true);
    setApiError(null);
    try {
      setApiToken(await revealApiToken());
    } catch (e) {
      setApiError(toMessage(e));
    } finally {
      setApiTokenBusy(false);
    }
  };

  const onRegenerateToken = async () => {
    setApiTokenBusy(true);
    setApiError(null);
    try {
      setApiToken(await regenerateApiToken());
    } catch (e) {
      setApiError(toMessage(e));
    } finally {
      setApiTokenBusy(false);
    }
  };

  const onRateLimitCommit = async () => {
    const parsed = Number.parseInt(rateLimit, 10);
    if (!Number.isFinite(parsed) || parsed < 0) {
      setRateLimit(String(api.value.max_rewrites_per_minute));
      return;
    }
    try {
      await setApiRateLimit(parsed);
      setApiValue({ ...api.value, max_rewrites_per_minute: parsed });
      flash("api-rate-limit");
    } catch (e) {
      setApiError(toMessage(e));
    }
  };

  const onInstallCli = async () => {
    setCliBusy(true);
    try {
      setCliStatus(`Installed at ${await installCliShim()}`);
    } catch (e) {
      setCliStatus(toMessage(e));
    } finally {
      setCliBusy(false);
    }
  };

  const onRunSetupClick = async () => {
    setSetupBusy(true);
    setSetupError(null);
    try {
      await setSetting("onboarding.completed", "false");
      onRunSetup();
    } catch (e) {
      setSetupError(toMessage(e));
    } finally {
      setSetupBusy(false);
    }
  };

  return (
    <div>
      <h1 style={{ marginTop: 0 }}>General</h1>
      <p className="kea-muted" style={{ marginBottom: 24 }}>
        Appearance, startup, updates, sounds, notifications and permissions.
      </p>

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Appearance</h2>
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
        <h2 style={{ margin: "0 0 12px" }}>Startup & updates</h2>
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
        <h2 style={{ margin: "0 0 12px" }}>Sounds & notifications</h2>
        <RowGroup aria-label="Sounds and notifications">
          <Row
            label="Dictation sounds"
            hint="Play a short tone when dictation finishes or fails"
          >
            {savedKey === "sound-cues" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              checked={soundCues}
              onChange={(next) => void onSoundCuesChange(next)}
              label="Play dictation sounds"
              disabled={soundCuesBusy}
            />
          </Row>
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

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Local API &amp; scripting</h2>
        <p className="kea-muted" style={{ margin: "0 0 12px" }}>
          {API_THREAT_MODEL}
        </p>
        <RowGroup aria-label="Local API">
          <Row
            label="Local API"
            tone={api.error ? "danger" : "muted"}
            hint={
              api.error ??
              (!api.value.supported
                ? "The local API needs a unix domain socket, which this platform does not have."
                : api.value.running
                  ? `Listening on ${api.value.socket_path}`
                  : "Off. Raycast, Alfred, Shortcuts and the shell can still use kea:// links.")
            }
          >
            {savedKey === "local-api" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              checked={api.value.enabled}
              onChange={(next) => void onApiToggle(next)}
              label="Enable the local API"
              disabled={api.busy || !api.value.supported}
            />
          </Row>
          <Row
            label="Access token"
            hint={
              apiToken ??
              "Send it as the X-KEA-Token header. Regenerating it stops every script that has the old one."
            }
          >
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onRevealToken()}
              disabled={apiTokenBusy}
            >
              {apiToken ? "Reveal again" : "Reveal"}
            </button>
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onRegenerateToken()}
              disabled={apiTokenBusy}
            >
              Regenerate
            </button>
          </Row>
          <Row
            label="Rewrites per minute"
            hint="Caps what a runaway script can spend on LLM calls. Takes effect the next time the API starts."
          >
            {savedKey === "api-rate-limit" && <span className="kea-saved">Saved ✓</span>}
            <input
              className="kea-input"
              type="number"
              min={0}
              aria-label="Rewrites per minute"
              value={rateLimit}
              onChange={(e) => setRateLimit(e.target.value)}
              onBlur={() => void onRateLimitCommit()}
              style={{ width: 90 }}
            />
          </Row>
          <Row
            label="Command-line tool"
            hint={cliStatus ?? "Links the bundled kea command into /usr/local/bin"}
          >
            <button
              type="button"
              className="kea-btn"
              onClick={() => void onInstallCli()}
              disabled={cliBusy}
            >
              Install
            </button>
          </Row>
        </RowGroup>
      </section>

      <PermissionPanel />

      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 12px" }}>Setup assistant</h2>
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
