import { useEffect, useRef, useState } from "react";
import {
  addCustomProvider,
  getBinding,
  getProviderConfig,
  hasCredential,
  listProviders,
  listSttEngines,
  listTtsEngines,
  setCredential,
  setProviderConfig,
  testProvider,
  type ProviderTestResult,
} from "../../api";
import {
  applyDefaultChoice,
  type Capability,
  type DefaultChoice,
} from "../../lib/capabilityDefaults";
import { slugify } from "../../lib/format";

export type AiChoice = "openai" | "local" | "custom";

type Props = {
  setCommit: (fn: (() => Promise<void>) | null) => void;
  onCommitted: (choice: AiChoice) => void;
};

/** Writes a ("default", capability) binding only when none exists yet, so
 * re-running the wizard never clobbers earlier choices. */
async function setDefaultIfUnset(
  capability: Capability,
  choice: DefaultChoice,
  voice?: string | null,
): Promise<void> {
  const existing = await getBinding("default", capability).catch(() => null);
  if (existing) return;
  await applyDefaultChoice(capability, choice, voice);
}

export default function ConnectStep({ setCommit, onCommitted }: Props) {
  const [selected, setSelected] = useState<AiChoice>("openai");
  const [hasKey, setHasKey] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const [testResult, setTestResult] = useState<ProviderTestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const [customName, setCustomName] = useState("");
  const [customUrl, setCustomUrl] = useState("");

  useEffect(() => {
    let cancelled = false;
    hasCredential("openai")
      .then((saved) => {
        if (!cancelled) setHasKey(saved);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const stateRef = useRef({ selected, keyDraft, customName, customUrl, hasKey });
  stateRef.current = { selected, keyDraft, customName, customUrl, hasKey };

  useEffect(() => {
    setCommit(async () => {
      const { selected, keyDraft, customName, customUrl, hasKey } = stateRef.current;

      if (selected === "openai") {
        const draft = keyDraft.trim();
        if (draft) {
          await setCredential("openai", draft);
          setHasKey(true);
          setKeyDraft("");
        }
        // Without a key the cloud defaults could never work — leave them
        // unset so amber status surfaces on the affected features instead.
        if (hasKey || draft) {
          await setDefaultIfUnset(
            "llm",
            { engine: "openai", model: "gpt-4o-mini", providerRef: "openai" },
          );
          await setDefaultIfUnset(
            "stt",
            { engine: "openai-stt", model: "whisper-1", providerRef: "openai" },
          );
          await setDefaultIfUnset(
            "tts",
            { engine: "openai-tts", model: "tts-1", providerRef: "openai" },
            "alloy",
          );
        }
      } else if (selected === "local") {
        const [sttEngines, ttsEngines] = await Promise.all([
          listSttEngines().catch(() => []),
          listTtsEngines().catch(() => []),
        ]);
        if (sttEngines.some((e) => e.id === "whisper")) {
          await setDefaultIfUnset("stt", {
            engine: "whisper",
            model: null,
            providerRef: null,
          });
        }
        if (ttsEngines.some((e) => e.id === "sherpa-tts")) {
          await setDefaultIfUnset("tts", {
            engine: "sherpa-tts",
            model: null,
            providerRef: null,
          });
        }
        // A local text default only makes sense once the local server has a
        // base URL; otherwise leave it unset (features show amber status).
        const config = await getProviderConfig("local-llm").catch(() => null);
        if (config?.base_url.trim()) {
          await setDefaultIfUnset("llm", {
            engine: "openai-compatible",
            model: null,
            providerRef: "local-llm",
          });
        }
      } else {
        const name = customName.trim();
        if (!name) {
          throw new Error("Enter a name for your server (or pick another option).");
        }
        const ref = slugify(name);
        if (!ref) {
          throw new Error("Server name needs at least one letter or number.");
        }
        const providers = await listProviders().catch(() => []);
        if (!providers.some((p) => p.provider_ref === ref)) {
          await addCustomProvider(ref, name);
        }
        if (customUrl.trim()) {
          // Preserve a previously configured default_model on re-run.
          const existing = await getProviderConfig(ref).catch(() => null);
          await setProviderConfig(ref, {
            base_url: customUrl.trim(),
            default_model: existing?.default_model ?? "",
          });
        }
        await setDefaultIfUnset("llm", {
          engine: "openai-compatible",
          model: null,
          providerRef: ref,
        });
      }

      onCommitted(selected);
    });
    return () => setCommit(null);
  }, [setCommit, onCommitted]);

  const runTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const draft = keyDraft.trim();
      if (draft) {
        await setCredential("openai", draft);
        setHasKey(true);
        setKeyDraft("");
      }
      setTestResult(await testProvider("openai"));
    } catch (e) {
      setTestResult({ ok: false, message: e instanceof Error ? e.message : String(e) });
    } finally {
      setTesting(false);
    }
  };

  const choiceCard = (
    value: AiChoice,
    title: string,
    hint: string,
    body?: React.ReactNode,
  ) => (
    <label className={`kea-choice${selected === value ? " kea-choice--selected" : ""}`}>
      <input
        type="radio"
        name="ai-choice"
        value={value}
        checked={selected === value}
        onChange={() => setSelected(value)}
      />
      <span className="kea-choice__body">
        <span className="kea-choice__title">{title}</span>
        <span className="kea-choice__hint">{hint}</span>
        {selected === value && body}
      </span>
    </label>
  );

  return (
    <div>
      <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
        Choose where KEA's AI runs. You can change this any time in Settings.
      </p>
      <div className="kea-choice-group" role="radiogroup" aria-label="AI provider">
        {choiceCard(
          "openai",
          "OpenAI",
          "Best quality. Needs an OpenAI API key.",
          <span className="kea-choice__extra">
            <span style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <input
                type="password"
                className="kea-input"
                value={keyDraft}
                onChange={(e) => setKeyDraft(e.target.value)}
                placeholder={hasKey ? "Paste a new key to replace" : "Paste your API key"}
                aria-label="OpenAI API key"
                autoComplete="off"
              />
              <button
                type="button"
                className="kea-btn"
                onClick={() => void runTest()}
                disabled={testing || (!hasKey && !keyDraft.trim())}
              >
                Test connection
              </button>
            </span>
            {hasKey && !keyDraft.trim() && (
              <span className="kea-saved">API key saved ✓</span>
            )}
            {testResult && (
              <span
                role="status"
                style={{
                  color: testResult.ok ? "var(--ok)" : "var(--danger)",
                  fontSize: 13,
                }}
              >
                {testResult.message}
              </span>
            )}
          </span>,
        )}
        {choiceCard(
          "local",
          "Local only",
          "Private and free. Everything runs on this Mac.",
        )}
      </div>
      <details className="kea-advanced">
        <summary>Advanced</summary>
        <div className="kea-advanced__body">
          {choiceCard(
            "custom",
            "Custom server",
            "An OpenAI-compatible server you run or subscribe to.",
            <span className="kea-choice__extra">
              <input
                className="kea-input"
                value={customName}
                onChange={(e) => setCustomName(e.target.value)}
                placeholder="Name (e.g. Groq)"
                aria-label="Server name"
              />
              <input
                className="kea-input"
                value={customUrl}
                onChange={(e) => setCustomUrl(e.target.value)}
                placeholder="Server URL (OpenAI-compatible)"
                aria-label="Server URL"
              />
            </span>,
          )}
        </div>
      </details>
    </div>
  );
}
