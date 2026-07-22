import { useEffect, useState } from "react";
import {
  getProviderConfig,
  setProviderConfig,
  type ProviderConfig,
} from "../api";
import CredentialField from "./CredentialField";
import Spinner from "./Spinner";

type Props = {
  provider_ref: string;
  title: string;
};

const emptyConfig = (): ProviderConfig => ({
  base_url: "",
  default_model: "",
});

export default function EngineConfig({ provider_ref, title }: Props) {
  const [config, setConfig] = useState<ProviderConfig>(emptyConfig());
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    getProviderConfig(provider_ref)
      .then((cfg) => setConfig(cfg ?? emptyConfig()))
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [provider_ref]);

  const save = async () => {
    setBusy(true);
    setStatus(null);
    try {
      await setProviderConfig(provider_ref, config);
      setStatus("Provider settings saved.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <h3 style={{ margin: "0 0 12px" }}>{title}</h3>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 100 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading config…</span>
        </div>
      ) : (
        <>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Base URL</span>
        <input
          className="kea-input"
          value={config.base_url}
          onChange={(e) =>
            setConfig((c) => ({ ...c, base_url: e.target.value }))
          }
          placeholder="https://api.openai.com/v1"
          style={{ maxWidth: 420 }}
        />
      </label>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Default model</span>
        <input
          className="kea-input"
          value={config.default_model}
          onChange={(e) =>
            setConfig((c) => ({ ...c, default_model: e.target.value }))
          }
          placeholder="gpt-4o-mini"
          style={{ maxWidth: 420 }}
        />
      </label>
      <button type="button" className="kea-btn" onClick={save} disabled={busy}>
        Save provider config
      </button>
      <CredentialField provider_ref={provider_ref} />
      {status && (
        <p className="kea-muted" style={{ marginTop: 12 }}>
          {status}
        </p>
      )}
        </>
      )}
    </section>
  );
}
