import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getBinding,
  listProviders,
  previewVoice,
  type Binding,
  type Provider,
} from "../api";
import { useModelDownloads } from "../hooks/useModelDownloads";
import {
  buildCapabilityOptions,
  defaultTarget,
  loadKeyStates,
  applyDefaultChoice,
  startOptionDownload,
  CAPABILITY_LABELS,
  OPENAI_TTS_VOICES,
  type BindingTarget,
  type Capability,
  type CapabilityOption,
} from "../lib/capabilityDefaults";
import Spinner from "./Spinner";

export { CAPABILITY_LABELS };
export type { Capability };

type PickerOption = CapabilityOption;

type Props = {
  capability: Capability;
  open: boolean;
  onClose: () => void;
  onApplied: () => void;
  /**
   * Binding row the choice is written to. Defaults to the capability-wide
   * ("default", capability) row; feature pages pass their own feature and slot
   * to write an override that only affects that feature. Feature and slot
   * travel together so a half-specified target can't silently fall back to
   * overwriting the global default.
   */
  target?: BindingTarget;
  /** Heading override, e.g. "Speech to text for Dictation". */
  title?: string;
};

function defaultEngineFor(capability: Capability, providerRef: string): string {
  if (capability === "stt") return "openai-stt";
  if (capability === "tts") return "openai-tts";
  return providerRef === "openai" ? "openai" : "openai-compatible";
}

function findMatch(options: PickerOption[], binding: Binding): string | null {
  const match =
    options.find(
      (o) => o.engine === binding.engine_id && (o.model ?? null) === (binding.model ?? null),
    ) ?? options.find((o) => o.engine === binding.engine_id);
  return match?.id ?? null;
}

export default function DefaultsPicker({
  capability,
  open,
  onClose,
  onApplied,
  target,
  title,
}: Props) {
  const resolved = target ?? defaultTarget(capability);
  const targetFeature = resolved.feature;
  const targetSlot = resolved.slot;
  const [loading, setLoading] = useState(true);
  const [options, setOptions] = useState<PickerOption[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [savedId, setSavedId] = useState<string | null>(null);
  const [pending, setPending] = useState<PickerOption | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [voice, setVoice] = useState(OPENAI_TTS_VOICES[0]);
  const [advProvider, setAdvProvider] = useState("openai");
  const [advModel, setAdvModel] = useState("");
  const [advEngine, setAdvEngine] = useState("");

  const pendingRef = useRef<PickerOption | null>(null);
  pendingRef.current = pending;
  const voiceRef = useRef(voice);
  voiceRef.current = voice;

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setSavedId(null);
    setPending(null);

    (async () => {
      try {
        const [providerList, binding] = await Promise.all([
          listProviders(),
          getBinding(targetFeature, targetSlot).catch(() => null),
        ]);
        const keyByRef = await loadKeyStates(providerList);
        const opts = await buildCapabilityOptions(capability, providerList, keyByRef);

        if (!cancelled) {
          setProviders(providerList);
          setAdvProvider(providerList[0]?.provider_ref ?? "openai");
          setOptions(opts);
          setCurrentId(binding ? findMatch(opts, binding) : null);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [open, capability, targetFeature, targetSlot]);

  const apply = useCallback(
    async (option: PickerOption) => {
      setError(null);
      try {
        await applyDefaultChoice(
          capability,
          option,
          capability === "tts" ? voiceRef.current : null,
          { feature: targetFeature, slot: targetSlot },
        );
        setPending(null);
        setSavedId(option.id);
        setCurrentId(option.id);
        onApplied();
      } catch (e) {
        setPending(null);
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [capability, onApplied, targetFeature, targetSlot],
  );

  const catalogIds = useMemo(
    () =>
      new Set(
        options.flatMap((o) => (o.downloadKind && o.model ? [o.model] : [])),
      ),
    [options],
  );

  const { progressById } = useModelDownloads({
    catalogIds,
    onComplete: (modelId) => {
      setOptions((prev) =>
        prev.map((o) =>
          o.model === modelId ? { ...o, installed: true, status: "installed ✓" } : o,
        ),
      );
      const picked = pendingRef.current;
      if (picked?.model === modelId) {
        void apply({ ...picked, installed: true });
      }
    },
    onError: (modelId, message) => {
      if (pendingRef.current?.model === modelId) setPending(null);
      setError(`${modelId}: ${message}`);
    },
  });

  const pick = async (option: PickerOption) => {
    if (pending) return;
    setError(null);
    setSavedId(null);
    if (option.downloadKind && option.model && !option.installed) {
      // Sync the ref imperatively: the download-complete event can arrive
      // before React re-renders and syncs pendingRef from state.
      pendingRef.current = option;
      setPending(option);
      try {
        await startOptionDownload(option);
      } catch (e) {
        pendingRef.current = null;
        setPending(null);
        setError(e instanceof Error ? e.message : String(e));
      }
      return;
    }
    await apply(option);
  };

  const preview = async (option: PickerOption) => {
    setError(null);
    try {
      await previewVoice(option.engine, option.model, option.cloudVoices ? voiceRef.current : null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const applyAdvanced = async () => {
    const engine = advEngine.trim() || defaultEngineFor(capability, advProvider);
    await apply({
      id: `advanced:${engine}:${advModel.trim()}`,
      label: advModel.trim() || engine,
      detail: "custom",
      status: "",
      ready: true,
      engine,
      model: advModel.trim() || null,
      providerRef: advProvider || null,
    });
  };

  if (!open) return null;

  return (
    <div
      className="kea-picker"
      role="dialog"
      aria-label={title ?? `Choose ${CAPABILITY_LABELS[capability]}`}
    >
      <div className="kea-picker__header">
        <strong>{title ?? `Choose ${CAPABILITY_LABELS[capability].toLowerCase()}`}</strong>
        <button type="button" className="kea-icon-btn" aria-label="Close" onClick={onClose}>
          ✕
        </button>
      </div>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 80 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading options…</span>
        </div>
      ) : (
        <>
          <ul className="kea-picker__options">
            {options.map((option) => {
              const progress = option.model ? progressById.get(option.model) : undefined;
              const isPending = pending?.id === option.id;
              const percent =
                progress && progress.bytes_total > 0
                  ? Math.round((progress.bytes_received / progress.bytes_total) * 100)
                  : null;
              const statusText = isPending
                ? percent !== null
                  ? `downloading ${percent}%`
                  : "starting download…"
                : savedId === option.id
                  ? "Saved ✓"
                  : option.status;
              return (
                <li key={option.id}>
                  <div className="kea-picker__row">
                    <button
                      type="button"
                      className={`kea-picker__option${
                        currentId === option.id ? " kea-picker__option--current" : ""
                      }`}
                      onClick={() => void pick(option)}
                      disabled={!option.ready || !!pending}
                      aria-current={currentId === option.id ? "true" : undefined}
                    >
                      <span className="kea-picker__name">
                        {option.label}
                        <span className="kea-picker__detail">{option.detail}</span>
                      </span>
                      <span className="kea-picker__status">{statusText}</span>
                    </button>
                    {capability === "tts" && option.cloudVoices && (
                      <select
                        className="kea-select"
                        aria-label="Voice"
                        value={voice}
                        onChange={(e) => setVoice(e.target.value)}
                      >
                        {OPENAI_TTS_VOICES.map((v) => (
                          <option key={v} value={v}>
                            {v}
                          </option>
                        ))}
                      </select>
                    )}
                    {capability === "tts" &&
                      (option.installed || (option.cloudVoices && option.ready)) && (
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Preview ${option.label}`}
                          onClick={() => void preview(option)}
                        >
                          ▶
                        </button>
                      )}
                  </div>
                </li>
              );
            })}
          </ul>
          <details className="kea-advanced">
            <summary>Advanced</summary>
            <div className="kea-advanced__body">
              <label>
                <span className="kea-label">Provider ref</span>
                <select
                  className="kea-select"
                  value={advProvider}
                  onChange={(e) => setAdvProvider(e.target.value)}
                >
                  {providers.map((p) => (
                    <option key={p.provider_ref} value={p.provider_ref}>
                      {p.provider_ref}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                <span className="kea-label">Model id</span>
                <input
                  className="kea-input"
                  value={advModel}
                  onChange={(e) => setAdvModel(e.target.value)}
                  placeholder="custom model id"
                />
              </label>
              <label>
                <span className="kea-label">Engine id</span>
                <input
                  className="kea-input"
                  value={advEngine}
                  onChange={(e) => setAdvEngine(e.target.value)}
                  placeholder={defaultEngineFor(capability, advProvider)}
                />
              </label>
              <button type="button" className="kea-btn" onClick={() => void applyAdvanced()}>
                Use this
              </button>
            </div>
          </details>
          {error && (
            <p className="kea-muted" style={{ marginTop: 8 }}>
              {error}
            </p>
          )}
        </>
      )}
    </div>
  );
}
