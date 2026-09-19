import { useCallback, useEffect, useState } from "react";
import {
  addCustomProvider,
  discoverLocalLlms,
  getBinding,
  listOnnxModels,
  listProviders,
  listWhisperModels,
  setProviderConfig,
  type Binding,
  type LocalLlmServer,
  type Provider,
} from "../api";
import Banner from "../components/Banner";
import DefaultsPicker, {
  CAPABILITY_LABELS,
  type Capability,
} from "../components/DefaultsPicker";
import LoadingBlock from "../components/LoadingBlock";
import ProviderRow from "../components/ProviderRow";
import { Row, RowGroup } from "../components/SettingsRow";
import { usePendingActivation } from "../hooks/usePendingActivation";
import { describeBinding } from "../lib/featureSlot";
import { slugify, toMessage } from "../lib/format";

const CAPABILITIES: { capability: Capability; icon: string }[] = [
  { capability: "llm", icon: "✍️" },
  { capability: "stt", icon: "🎙" },
  { capability: "tts", icon: "🔊" },
];

/** The built-in provider a discovered server is written to. */
const LOCAL_PROVIDER_REF = "local-llm";

export default function AiProvidersPage() {
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [bindings, setBindings] = useState<Record<Capability, Binding | null>>({
    llm: null,
    stt: null,
    tts: null,
  });
  const [modelNames, setModelNames] = useState<Map<string, string>>(new Map());
  const [pickerFor, setPickerFor] = useState<Capability | null>(null);
  const [adding, setAdding] = useState(false);
  const [newName, setNewName] = useState("");
  const [newUrl, setNewUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  // null = never looked; [] = looked and found nothing, which is worth saying.
  const [found, setFound] = useState<LocalLlmServer[] | null>(null);
  const [scanning, setScanning] = useState(false);
  const [connected, setConnected] = useState<string | null>(null);

  const refreshProviders = useCallback(async () => {
    try {
      setProviders(await listProviders());
    } catch (e) {
      setError(toMessage(e));
      setProviders([]);
    }
  }, []);

  const refreshBindings = useCallback(async () => {
    const [llm, stt, tts] = await Promise.all(
      CAPABILITIES.map(({ capability }) =>
        getBinding("default", capability).catch(() => null),
      ),
    );
    setBindings({ llm, stt, tts });
  }, []);

  // Owned by the page, not the popover: a model download outlives the picker,
  // and the default still gets set if the user closes it mid-download.
  const activation = usePendingActivation(() => void refreshBindings());

  useEffect(() => {
    void refreshProviders();
    void refreshBindings();
    Promise.all([
      listWhisperModels().catch(() => []),
      listOnnxModels("parakeet").catch(() => []),
      listOnnxModels("tts").catch(() => []),
    ])
      .then((catalogs) => {
        const names = new Map<string, string>();
        catalogs.flat().forEach((m) => names.set(m.id, m.display_name));
        setModelNames(names);
      })
      .catch(() => {});
  }, [refreshProviders, refreshBindings]);

  const summary = (binding: Binding | null): string | null =>
    describeBinding(binding, { providers: providers ?? [], modelNames });

  const addProvider = async () => {
    const name = newName.trim();
    if (!name) return;
    setError(null);
    const ref = slugify(name);
    if (!ref) {
      setError("Provider name needs at least one letter or number.");
      return;
    }
    if (providers?.some((p) => p.provider_ref === ref)) {
      setError(`A provider "${ref}" already exists.`);
      return;
    }
    try {
      await addCustomProvider(ref, name);
      if (newUrl.trim()) {
        await setProviderConfig(ref, { base_url: newUrl.trim(), default_model: "" });
      }
      setAdding(false);
      setNewName("");
      setNewUrl("");
      await refreshProviders();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  // Probing takes at most one 2s timeout, so the button just reports busy
  // rather than growing a spinner of its own.
  const scanForServers = async () => {
    setScanning(true);
    setError(null);
    setConnected(null);
    try {
      setFound(await discoverLocalLlms());
    } catch (e) {
      setError(toMessage(e));
    } finally {
      setScanning(false);
    }
  };

  /**
   * Points the built-in "Local server" provider at what was found. Nothing
   * else is needed: `openai-compatible` reads its base URL and default model
   * straight off this config, so the next pick in Defaults just works.
   */
  const useServer = async (server: LocalLlmServer, model: string) => {
    setError(null);
    try {
      await setProviderConfig(LOCAL_PROVIDER_REF, {
        base_url: server.base_url,
        default_model: model,
      });
      setConnected(`${server.display_name} saved to Local server.`);
      setFound(null);
      await refreshProviders();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const missing = CAPABILITIES.filter(({ capability }) => !bindings[capability]);

  return (
    <div>
      <h1 style={{ marginTop: 0 }}>AI Providers</h1>
      <p className="kea-muted" style={{ marginBottom: 24 }}>
        Connect AI services once — every feature reuses them.
      </p>

      {error && <Banner variant="error">{error}</Banner>}

      <section style={{ marginBottom: 32 }}>
        <h2 style={{ margin: "0 0 12px" }}>Providers</h2>
        {providers === null ? (
          <LoadingBlock label="Loading providers…" minHeight={60} />
        ) : (
          <RowGroup aria-label="Providers">
            {providers.map((provider) => (
              <ProviderRow
                key={provider.provider_ref}
                provider={provider}
                onRemoved={() => void refreshProviders()}
              />
            ))}
          </RowGroup>
        )}
        <div style={{ marginTop: 12 }}>
          {adding ? (
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <input
                className="kea-input"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="Name (e.g. Mistral)"
                aria-label="Provider name"
              />
              <input
                className="kea-input"
                value={newUrl}
                onChange={(e) => setNewUrl(e.target.value)}
                placeholder="Server URL (OpenAI-compatible)"
                aria-label="Server URL"
                style={{ minWidth: 260 }}
              />
              <button
                type="button"
                className="kea-btn"
                onClick={() => void addProvider()}
                disabled={!newName.trim()}
              >
                Add
              </button>
              <button
                type="button"
                className="kea-btn"
                onClick={() => {
                  setAdding(false);
                  setNewName("");
                  setNewUrl("");
                }}
              >
                Cancel
              </button>
            </div>
          ) : (
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <button type="button" className="kea-btn" onClick={() => setAdding(true)}>
                ＋ Add provider
              </button>
              <button
                type="button"
                className="kea-btn"
                onClick={() => void scanForServers()}
                disabled={scanning}
              >
                {scanning ? "Looking…" : "Find local servers"}
              </button>
              {connected && <span className="kea-saved">{connected}</span>}
            </div>
          )}
          {found !== null && (
            <div style={{ marginTop: 12 }}>
              {found.length === 0 ? (
                <p className="kea-muted" style={{ margin: 0 }}>
                  No server answered on this Mac. Start Ollama or LM Studio and look again.
                </p>
              ) : (
                <RowGroup aria-label="Local servers found">
                  {found.map((server) => (
                    <Row key={server.id} label={server.display_name} hint={server.base_url}>
                      {server.models.length === 0 ? (
                        // A running server with nothing pulled is a real state:
                        // say so rather than leaving an empty dropdown.
                        <span className="kea-muted">No models installed</span>
                      ) : (
                        <LocalServerPicker server={server} onUse={useServer} />
                      )}
                    </Row>
                  ))}
                </RowGroup>
              )}
            </div>
          )}
        </div>
      </section>

      <section>
        <h2 style={{ margin: "0 0 12px" }}>Defaults</h2>
        {missing.length > 0 && (
          <Banner variant="warn">
            Not set: {missing.map(({ capability }) => CAPABILITY_LABELS[capability]).join(", ")}.
            Features that need them may not work until you choose a default.
          </Banner>
        )}
        <RowGroup aria-label="Capability defaults">
          {CAPABILITIES.map(({ capability, icon }) => {
            const binding = bindings[capability];
            return (
              <Row
                key={capability}
                label={`${icon} ${CAPABILITY_LABELS[capability]}`}
                hint={summary(binding) ?? undefined}
              >
                {!binding && (
                  <>
                    <span className="kea-dot kea-dot--warn" aria-hidden="true" />
                    <span className="kea-muted">Not set</span>
                  </>
                )}
                <button
                  type="button"
                  className="kea-btn"
                  onClick={() => setPickerFor(capability)}
                >
                  {binding ? "Change…" : "Choose…"}
                </button>
              </Row>
            );
          })}
        </RowGroup>
        {pickerFor && (
          <DefaultsPicker
            capability={pickerFor}
            open
            onClose={() => setPickerFor(null)}
            activation={activation}
          />
        )}
        {!pickerFor && activation.pending && (
          <p className="kea-muted" style={{ marginTop: 8 }}>
            Downloading {activation.pending.option.label} — it becomes the default when the
            download finishes.
          </p>
        )}
      </section>
    </div>
  );
}

/**
 * One found server's model list plus the button that commits it. A component
 * of its own so each row keeps its own selection — a single piece of page
 * state would make picking a model on one row move the other's dropdown.
 */
function LocalServerPicker({
  server,
  onUse,
}: {
  server: LocalLlmServer;
  onUse: (server: LocalLlmServer, model: string) => Promise<void>;
}) {
  const [model, setModel] = useState(server.models[0]);
  return (
    <>
      <select
        className="kea-select"
        aria-label={`${server.display_name} model`}
        value={model}
        onChange={(e) => setModel(e.target.value)}
      >
        {server.models.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
      <button type="button" className="kea-btn" onClick={() => void onUse(server, model)}>
        Use this
      </button>
    </>
  );
}
