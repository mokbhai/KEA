import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  downloadOnnxModel,
  downloadWhisperModel,
  getBinding,
  getDictationSettings,
  getTtsSettings,
  hasCredential,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listOnnxModels,
  listProviders,
  listWhisperModels,
  previewVoice,
  setBinding,
  setDictationSettings,
  setTtsSettings,
  type Binding,
  type OnnxModel,
  type Provider,
  type WhisperModel,
} from "../api";
import { useModelDownloads } from "../hooks/useModelDownloads";
import { formatBytes } from "../lib/format";
import Spinner from "./Spinner";

export type Capability = "llm" | "stt" | "tts";

export const CAPABILITY_LABELS: Record<Capability, string> = {
  llm: "Writing & rewriting",
  stt: "Speech to text",
  tts: "Text to speech",
};

const OPENAI_TTS_VOICES = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

type DownloadKind = "whisper" | "parakeet" | "tts";

type PickerOption = {
  id: string;
  label: string;
  detail: string;
  status: string;
  ready: boolean;
  engine: string;
  model: string | null;
  providerRef: string | null;
  installed?: boolean;
  downloadKind?: DownloadKind;
  cloudVoices?: boolean;
};

type Props = {
  capability: Capability;
  open: boolean;
  onClose: () => void;
  onApplied: () => void;
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

export default function DefaultsPicker({ capability, open, onClose, onApplied }: Props) {
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
          getBinding("default", capability).catch(() => null),
        ]);
        const keyByRef = new Map<string, boolean>();
        await Promise.all(
          providerList.map(async (p) => {
            const saved =
              p.provider_ref === "local-llm"
                ? true
                : await hasCredential(p.provider_ref).catch(() => false);
            keyByRef.set(p.provider_ref, saved);
          }),
        );

        const localOption = (
          m: WhisperModel | OnnxModel,
          engine: string,
          downloadKind: DownloadKind,
          installed: Set<string>,
        ): PickerOption => ({
          id: `${engine}:${m.id}`,
          label: m.display_name,
          detail: `on this Mac · ${m.language}`,
          status: installed.has(m.id) ? "installed ✓" : `${formatBytes(m.size_bytes)} ⬇`,
          ready: true,
          engine,
          model: m.id,
          providerRef: null,
          installed: installed.has(m.id),
          downloadKind,
        });
        const cloudStatus = (ref: string) => (keyByRef.get(ref) ? "key ✓" : "Key missing");
        const hasOpenAi = providerList.some((p) => p.provider_ref === "openai");

        const opts: PickerOption[] = [];
        if (capability === "stt") {
          const [whisper, whisperInstalled, parakeet, parakeetInstalled] = await Promise.all([
            listWhisperModels(),
            listInstalledWhisperModels(),
            listOnnxModels("parakeet"),
            listInstalledOnnxModels("parakeet"),
          ]);
          const wInstalled = new Set(whisperInstalled);
          const pInstalled = new Set(parakeetInstalled);
          whisper.forEach((m) => opts.push(localOption(m, "whisper", "whisper", wInstalled)));
          parakeet.forEach((m) => opts.push(localOption(m, "parakeet", "parakeet", pInstalled)));
          if (hasOpenAi) {
            opts.push({
              id: "openai-stt:whisper-1",
              label: "OpenAI whisper-1",
              detail: "cloud",
              status: cloudStatus("openai"),
              ready: keyByRef.get("openai") ?? false,
              engine: "openai-stt",
              model: "whisper-1",
              providerRef: "openai",
            });
          }
        } else if (capability === "tts") {
          const [voices, voicesInstalled] = await Promise.all([
            listOnnxModels("tts"),
            listInstalledOnnxModels("tts"),
          ]);
          const vInstalled = new Set(voicesInstalled);
          voices.forEach((m) => opts.push(localOption(m, "sherpa-tts", "tts", vInstalled)));
          if (hasOpenAi) {
            opts.push({
              id: "openai-tts",
              label: "OpenAI voices",
              detail: "cloud",
              status: cloudStatus("openai"),
              ready: keyByRef.get("openai") ?? false,
              engine: "openai-tts",
              model: null,
              providerRef: "openai",
              cloudVoices: true,
            });
          }
        } else {
          providerList.forEach((p) => {
            const isOpenAi = p.provider_ref === "openai";
            const isLocal = p.provider_ref === "local-llm";
            opts.push({
              id: `llm:${p.provider_ref}`,
              label: p.name,
              detail: isLocal ? "your server" : isOpenAi ? "cloud" : "custom server",
              status: isLocal ? "no key needed" : cloudStatus(p.provider_ref),
              ready: isLocal || (keyByRef.get(p.provider_ref) ?? false),
              engine: isOpenAi ? "openai" : "openai-compatible",
              model: null,
              providerRef: p.provider_ref,
            });
          });
        }

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
  }, [open, capability]);

  const apply = useCallback(
    async (option: PickerOption) => {
      setError(null);
      try {
        await setBinding("default", capability, option.engine, option.model, option.providerRef);
        if (capability === "stt" && option.engine === "whisper") {
          const settings = await getDictationSettings();
          await setDictationSettings({ ...settings, active_model: option.model });
        } else if (capability === "tts" && option.engine === "sherpa-tts") {
          const settings = await getTtsSettings();
          await setTtsSettings({ ...settings, active_model: option.model });
        } else if (capability === "tts" && option.engine === "openai-tts") {
          const settings = await getTtsSettings();
          await setTtsSettings({ ...settings, active_voice: voiceRef.current });
        }
        setPending(null);
        setSavedId(option.id);
        setCurrentId(option.id);
        onApplied();
      } catch (e) {
        setPending(null);
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [capability, onApplied],
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
      setPending(option);
      try {
        if (option.downloadKind === "whisper") {
          await downloadWhisperModel(option.model);
        } else {
          await downloadOnnxModel(option.downloadKind, option.model);
        }
      } catch (e) {
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
    <div className="kea-picker" role="dialog" aria-label={`Choose ${CAPABILITY_LABELS[capability]}`}>
      <div className="kea-picker__header">
        <strong>Choose {CAPABILITY_LABELS[capability].toLowerCase()}</strong>
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
