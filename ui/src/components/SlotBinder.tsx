import { useEffect, useState } from "react";
import {
  getBinding,
  listLlmEngines,
  listSttEngines,
  listTtsEngines,
  setBinding,
  setDictationSttBinding,
  setTtsBinding,
  type EngineInfo,
} from "../api";
import Spinner from "./Spinner";

const PROVIDER_REFS = ["openai", "local-llm"] as const;

type Props = {
  feature: string;
  slot: string;
  slotKind?: "llm" | "stt" | "tts";
  title?: string;
};

export default function SlotBinder({
  feature,
  slot,
  slotKind = "llm",
  title,
}: Props) {
  const [engines, setEngines] = useState<EngineInfo[]>([]);
  const [engineId, setEngineId] = useState("");
  const [model, setModel] = useState("");
  const [providerRef, setProviderRef] = useState<string>(PROVIDER_REFS[0]);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);

  const sectionTitle =
    title ??
    (slotKind === "stt"
      ? "STT slot"
      : slotKind === "tts"
        ? "TTS slot"
        : "LLM slot");

  useEffect(() => {
    const loadEngines =
      slotKind === "stt"
        ? listSttEngines
        : slotKind === "tts"
          ? listTtsEngines
          : listLlmEngines;
    Promise.all([
      loadEngines()
        .then(setEngines)
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e))),
      getBinding(feature, slot)
        .then((binding) => {
          if (!binding) return;
          setEngineId(binding.engine_id);
          setModel(binding.model ?? "");
          setProviderRef(binding.provider_ref ?? PROVIDER_REFS[0]);
        })
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e))),
    ]).finally(() => setLoading(false));
  }, [slotKind, feature, slot]);

  const selectedEngine = engines.find((e) => e.id === engineId);

  const save = async () => {
    if (!engineId) return;
    setBusy(true);
    setStatus(null);
    try {
      if (slotKind === "stt" && feature === "dictation") {
        await setDictationSttBinding(
          engineId,
          model.trim() || null,
          providerRef || null,
        );
      } else if (slotKind === "tts" && feature === "tts") {
        await setTtsBinding(
          engineId,
          model.trim() || null,
          providerRef || null,
        );
      } else {
        await setBinding(
          feature,
          slot,
          engineId,
          model.trim() || null,
          providerRef || null,
        );
      }
      setStatus("Slot binding saved.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="kea-card" style={{ marginBottom: 16 }}>
      <h3 style={{ margin: "0 0 12px" }}>{sectionTitle}</h3>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 120 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading engines…</span>
        </div>
      ) : (
        <>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Engine</span>
        <select
          className="kea-select"
          value={engineId}
          onChange={(e) => setEngineId(e.target.value)}
          style={{ minWidth: 220 }}
        >
          <option value="">Select engine…</option>
          {engines.map((e) => (
            <option key={e.id} value={e.id}>
              {e.id}
            </option>
          ))}
        </select>
      </label>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Model</span>
        <input
          className="kea-input"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          placeholder={
            selectedEngine?.models[0] ? `e.g. ${selectedEngine.models[0]}` : "Model name"
          }
          list={engineId ? `${engineId}-models` : undefined}
          style={{ maxWidth: 320 }}
        />
        {selectedEngine && selectedEngine.models.length > 0 && (
          <datalist id={`${engineId}-models`}>
            {selectedEngine.models.map((m) => (
              <option key={m} value={m} />
            ))}
          </datalist>
        )}
      </label>
      <label style={{ display: "block", marginBottom: 12 }}>
        <span className="kea-label">Provider ref</span>
        <select
          className="kea-select"
          value={providerRef}
          onChange={(e) => setProviderRef(e.target.value)}
          style={{ minWidth: 220 }}
        >
          {PROVIDER_REFS.map((ref) => (
            <option key={ref} value={ref}>
              {ref}
            </option>
          ))}
        </select>
      </label>
      <button
        type="button"
        className="kea-btn"
        onClick={save}
        disabled={busy || !engineId}
      >
        Save binding
      </button>
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
