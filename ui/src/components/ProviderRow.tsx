import { useEffect, useRef, useState } from "react";
import {
  deleteCredential,
  getProviderConfig,
  hasCredential,
  removeCustomProvider,
  setCredential,
  setProviderConfig,
  testProvider,
  type Provider,
  type ProviderConfig,
  type ProviderTestResult,
} from "../api";
import { useSavedFlash } from "../hooks/useSavedFlash";
import { Row } from "./SettingsRow";

const emptyConfig = (): ProviderConfig => ({ base_url: "", default_model: "" });

type Props = {
  provider: Provider;
  onRemoved: () => void;
};

export default function ProviderRow({ provider, onRemoved }: Props) {
  const isLocal = provider.provider_ref === "local-llm";
  const [hasKey, setHasKey] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [replacing, setReplacing] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const [config, setConfig] = useState<ProviderConfig>(emptyConfig());
  const [testResult, setTestResult] = useState<ProviderTestResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedKey, flash] = useSavedFlash();
  const lastSavedConfig = useRef<ProviderConfig>(emptyConfig());

  useEffect(() => {
    let cancelled = false;
    hasCredential(provider.provider_ref)
      .then((saved) => {
        if (!cancelled) setHasKey(saved);
      })
      .catch(() => {});
    getProviderConfig(provider.provider_ref)
      .then((cfg) => {
        if (!cancelled) {
          const next = cfg ?? emptyConfig();
          setConfig(next);
          lastSavedConfig.current = next;
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [provider.provider_ref]);

  const keyState = hasKey ? "API key saved ✓" : isLocal ? "No key needed" : "Key missing";
  const ready = hasKey || isLocal;

  const saveKey = async () => {
    const secret = keyDraft.trim();
    // Enter can fire while a save is in flight; the button is disabled then.
    if (busy || !secret) return;
    setBusy(true);
    setError(null);
    try {
      await setCredential(provider.provider_ref, secret);
      setHasKey(true);
      setKeyDraft("");
      setReplacing(false);
      flash("key");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const removeKey = async () => {
    setBusy(true);
    setError(null);
    try {
      await deleteCredential(provider.provider_ref);
      setHasKey(false);
      setReplacing(false);
      setKeyDraft("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const runTest = async () => {
    setBusy(true);
    setError(null);
    setTestResult(null);
    try {
      setTestResult(await testProvider(provider.provider_ref));
    } catch (e) {
      setTestResult({ ok: false, message: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  const saveConfig = async () => {
    // Blur fires even without edits; skip the invoke (and the "Saved ✓"
    // flash) when nothing changed since the last save.
    const last = lastSavedConfig.current;
    if (config.base_url === last.base_url && config.default_model === last.default_model) {
      return;
    }
    setError(null);
    try {
      await setProviderConfig(provider.provider_ref, config);
      lastSavedConfig.current = config;
      flash("config");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const removeProvider = async () => {
    if (!window.confirm(`Remove ${provider.name}? Features using it will fall back to defaults.`)) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await removeCustomProvider(provider.provider_ref);
      onRemoved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <>
      <Row label={provider.name} hint={keyState}>
        <span
          className={`kea-dot ${ready ? "kea-dot--ok" : "kea-dot--warn"}`}
          aria-hidden="true"
        />
        <button
          type="button"
          className="kea-btn"
          aria-expanded={expanded}
          onClick={() => setExpanded((e) => !e)}
        >
          {expanded ? "Done" : "Edit"}
        </button>
      </Row>
      {expanded && (
        <div className="kea-provider-detail">
          <div className="kea-provider-detail__row">
            <span className="kea-label">API key</span>
            {hasKey && !replacing ? (
              <span className="kea-provider-detail__control">
                <span className="kea-masked-key" aria-label="API key saved">
                  ••••••••
                </span>
                {savedKey === "key" && <span className="kea-saved">Saved ✓</span>}
                <button
                  type="button"
                  className="kea-btn"
                  onClick={() => setReplacing(true)}
                  disabled={busy}
                >
                  Replace
                </button>
                <button type="button" className="kea-btn" onClick={removeKey} disabled={busy}>
                  Remove
                </button>
              </span>
            ) : (
              <span className="kea-provider-detail__control">
                <input
                  type="password"
                  className="kea-input"
                  value={keyDraft}
                  onChange={(e) => setKeyDraft(e.target.value)}
                  // Enter commits, like every other field on these pages.
                  // The button stays: a password field gives no hint that
                  // Enter would do anything, and it carries the disabled and
                  // busy states.
                  onKeyDown={(e) => {
                    if (e.key !== "Enter") return;
                    e.preventDefault();
                    void saveKey();
                  }}
                  placeholder={isLocal ? "Optional — only if your server needs one" : "Paste API key"}
                  aria-label={`${provider.name} API key`}
                  autoComplete="off"
                />
                <button
                  type="button"
                  className="kea-btn"
                  onClick={saveKey}
                  disabled={busy || !keyDraft.trim()}
                >
                  Save key
                </button>
                {replacing && (
                  <button
                    type="button"
                    className="kea-btn"
                    onClick={() => {
                      setReplacing(false);
                      setKeyDraft("");
                    }}
                  >
                    Cancel
                  </button>
                )}
                {savedKey === "key" && <span className="kea-saved">Saved ✓</span>}
              </span>
            )}
          </div>
          <div className="kea-provider-detail__row">
            <span className="kea-label">Connection</span>
            <span className="kea-provider-detail__control">
              <button type="button" className="kea-btn" onClick={runTest} disabled={busy}>
                Test connection
              </button>
              {testResult && (
                <span
                  style={{
                    color: testResult.ok ? "var(--ok)" : "var(--danger)",
                    fontSize: 13,
                  }}
                >
                  {testResult.message}
                </span>
              )}
            </span>
          </div>
          <details className="kea-advanced">
            <summary>Advanced</summary>
            <div className="kea-advanced__body">
              <p className="kea-muted" style={{ margin: 0, fontSize: 13 }}>
                Provider ref: <code>{provider.provider_ref}</code>
              </p>
              <label>
                <span className="kea-label">Base URL</span>
                <input
                  className="kea-input"
                  value={config.base_url}
                  onChange={(e) => setConfig((c) => ({ ...c, base_url: e.target.value }))}
                  onBlur={saveConfig}
                  placeholder="https://api.openai.com/v1"
                />
              </label>
              <label>
                <span className="kea-label">Default model</span>
                <input
                  className="kea-input"
                  value={config.default_model}
                  onChange={(e) => setConfig((c) => ({ ...c, default_model: e.target.value }))}
                  onBlur={saveConfig}
                  placeholder="gpt-4o-mini"
                />
              </label>
              {savedKey === "config" && <span className="kea-saved">Saved ✓</span>}
            </div>
          </details>
          {!provider.built_in && (
            <div style={{ marginTop: 12 }}>
              <button type="button" className="kea-btn" onClick={removeProvider} disabled={busy}>
                Remove provider
              </button>
            </div>
          )}
          {error && (
            <p className="kea-muted" style={{ margin: "8px 0 0" }}>
              {error}
            </p>
          )}
        </div>
      )}
    </>
  );
}
