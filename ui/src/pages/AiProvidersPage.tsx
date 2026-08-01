import { useCallback, useEffect, useState } from "react";
import {
  addCustomProvider,
  getBinding,
  listOnnxModels,
  listProviders,
  listWhisperModels,
  setProviderConfig,
  type Binding,
  type Provider,
} from "../api";
import Banner from "../components/Banner";
import DefaultsPicker, {
  CAPABILITY_LABELS,
  type Capability,
} from "../components/DefaultsPicker";
import ProviderRow from "../components/ProviderRow";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import { describeBinding } from "../lib/featureSlot";
import { slugify } from "../lib/format";

const CAPABILITIES: { capability: Capability; icon: string }[] = [
  { capability: "llm", icon: "✍️" },
  { capability: "stt", icon: "🎙" },
  { capability: "tts", icon: "🔊" },
];

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

  const refreshProviders = useCallback(async () => {
    try {
      setProviders(await listProviders());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
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
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const missing = CAPABILITIES.filter(({ capability }) => !bindings[capability]);

  return (
    <div>
      <h2 style={{ marginTop: 0 }}>AI Providers</h2>
      <p className="kea-muted" style={{ marginBottom: 24 }}>
        Connect AI services once — every feature reuses them.
      </p>

      {error && <Banner variant="error">{error}</Banner>}

      <section style={{ marginBottom: 32 }}>
        <h3 style={{ margin: "0 0 12px" }}>Providers</h3>
        {providers === null ? (
          <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 60 }}>
            <Spinner size={16} />
            <span className="kea-muted">Loading providers…</span>
          </div>
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
                placeholder="Name (e.g. Groq)"
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
            <button type="button" className="kea-btn" onClick={() => setAdding(true)}>
              ＋ Add provider
            </button>
          )}
        </div>
      </section>

      <section>
        <h3 style={{ margin: "0 0 12px" }}>Defaults</h3>
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
            onApplied={() => void refreshBindings()}
          />
        )}
      </section>
    </div>
  );
}
