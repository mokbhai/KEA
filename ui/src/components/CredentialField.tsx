import { useState } from "react";
import { deleteCredential, setCredential } from "../api";

type Props = {
  provider_ref: string;
};

export default function CredentialField({ provider_ref }: Props) {
  const [secret, setSecret] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const save = async () => {
    if (!secret.trim()) return;
    setBusy(true);
    setStatus(null);
    try {
      await setCredential(provider_ref, secret.trim());
      setSecret("");
      setStatus("API key saved to keyring.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    setStatus(null);
    try {
      await deleteCredential(provider_ref);
      setSecret("");
      setStatus("API key removed.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ marginTop: 12 }}>
      <label className="kea-label">API key</label>
      <input
        type="password"
        className="kea-input"
        value={secret}
        onChange={(e) => setSecret(e.target.value)}
        placeholder="Enter API key (never displayed after save)"
        style={{ maxWidth: 420 }}
        autoComplete="off"
      />
      <div style={{ marginTop: 8, display: "flex", gap: 8 }}>
        <button
          type="button"
          className="kea-btn"
          onClick={save}
          disabled={busy || !secret.trim()}
        >
          Save key
        </button>
        <button type="button" className="kea-btn" onClick={clear} disabled={busy}>
          Remove key
        </button>
      </div>
      {status && (
        <p className="kea-muted" style={{ marginTop: 8 }}>
          {status}
        </p>
      )}
    </div>
  );
}
