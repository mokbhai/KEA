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
  setApiTcpEnabled,
  setApiTcpPort,
  setAutostart,
  setSetting,
  showNotification,
  type ApiSettings,
} from "../api";
import Banner from "../components/Banner";
import ApiUsage from "../components/ApiUsage";
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
  unix_socket_supported: true,
  tcp_enabled: false,
  tcp_running: false,
  tcp_error: null,
  tcp_port: 8765,
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
  "current selection, open your microphone and spend your LLM API credits. By default KEA " +
  "listens on a file-permission-protected socket, so no web page can reach it and no other " +
  "account on this Mac can either — but any program running as you can. The optional " +
  "localhost port below is covered by its own warning.";

/**
 * Shown while the localhost port is on, or being turned on. The port is the one
 * place a browser page can reach the API, so the warning says what the token
 * buys and that the browser block is not the whole story.
 */
const TCP_WARNING =
  "Holding the token lets a program read your selection, type into the focused app, open " +
  "the microphone and spend LLM credits. Browsers are blocked from the localhost port, but " +
  "any program on this Mac that has the token can call it.";

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
  // The port toggle is shown as on the moment it is pressed, so the warning
  // appears with the decision rather than after the backend answers. Null
  // means no request is in flight and the saved value is what shows.
  const [tcpPending, setTcpPending] = useState<boolean | null>(null);
  const [tcpMessage, setTcpMessage] = useState<string | null>(null);
  const [portInput, setPortInput] = useState("8765");
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
        setPortInput(String(settings.tcp_port));
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

  /**
   * Re-reads the settings after a refused port change, so the toggle and the
   * port field show what is really listening rather than what was asked for.
   */
  const resyncApi = async () => {
    try {
      const settings = await getApiSettings();
      setApiValue(settings);
      setPortInput(String(settings.tcp_port));
    } catch {
      // Nothing better to show than the last known state.
    }
  };

  const onTcpToggle = async (enabled: boolean) => {
    setTcpPending(enabled);
    setTcpMessage(null);
    try {
      setApiValue(await setApiTcpEnabled(enabled));
      flash("local-api-port");
    } catch (e) {
      setTcpMessage(toMessage(e));
      await resyncApi();
    } finally {
      setTcpPending(null);
    }
  };

  const onPortCommit = async () => {
    const parsed = Number(portInput);
    if (!Number.isInteger(parsed) || parsed < 1 || parsed > 65535) {
      setPortInput(String(api.value.tcp_port));
      setTcpMessage("Enter a port between 1 and 65535.");
      return;
    }
    if (parsed === api.value.tcp_port) {
      setTcpMessage(null);
      return;
    }
    setTcpMessage(null);
    try {
      setApiValue(await setApiTcpPort(parsed));
      flash("local-api-port-number");
    } catch (e) {
      setTcpMessage(toMessage(e));
      await resyncApi();
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

  // The port is on as soon as it is pressed, until the backend answers.
  const tcpOn = tcpPending ?? api.value.tcp_enabled;

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
                  ? api.value.unix_socket_supported
                    ? `Listening on ${api.value.socket_path}`
                    : "Running. Reach it on the localhost port below."
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
        </RowGroup>

        <RowGroup aria-label="Localhost port">
          <Row
            label="Allow localhost port"
            tone={(tcpMessage ?? api.value.tcp_error) ? "danger" : "muted"}
            hint={
              tcpMessage ??
              api.value.tcp_error ??
              (tcpOn
                ? api.value.tcp_running
                  ? `Listening on 127.0.0.1:${api.value.tcp_port}`
                  : "Switched on, but not listening yet."
                : api.value.unix_socket_supported
                  ? "Off. Lets OpenAI-compatible tools reach the API on 127.0.0.1."
                  : "Off. On this platform the localhost port is the only way to reach the API.")
            }
          >
            {savedKey === "local-api-port" && <span className="kea-saved">Saved ✓</span>}
            <Toggle
              checked={tcpOn}
              onChange={(next) => void onTcpToggle(next)}
              label="Allow localhost port"
              disabled={tcpPending !== null}
            />
          </Row>
          <Row
            label="Port"
            hint="1 to 65535. Saved when you leave the field or press Enter."
          >
            {savedKey === "local-api-port-number" && (
              <span className="kea-saved">Saved ✓</span>
            )}
            <input
              className="kea-input"
              type="number"
              min={1}
              max={65535}
              aria-label="Localhost port"
              value={portInput}
              onChange={(e) => setPortInput(e.target.value)}
              onBlur={() => void onPortCommit()}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
              style={{ width: 90 }}
            />
          </Row>
        </RowGroup>
        {tcpOn && (
          <div style={{ marginTop: 12 }}>
            <Banner variant="warn">{TCP_WARNING}</Banner>
          </div>
        )}

        <RowGroup aria-label="Local API access">
          <Row
            label="Access token"
            hint={
              apiToken ??
              "Send it as the X-KEA-Token header or as a Bearer token. Regenerating it stops every script that has the old one."
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

        <ApiUsage
          port={api.value.tcp_port}
          portOn={api.value.tcp_enabled}
          socketPath={api.value.unix_socket_supported ? api.value.socket_path : null}
        />
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
