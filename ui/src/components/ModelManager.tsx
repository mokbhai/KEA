import { useCallback, useEffect, useRef, useState } from "react";
import {
  downloadOnnxModel,
  downloadWhisperModel,
  getDictationSettings,
  getTtsSettings,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listOnnxModels,
  listWhisperModels,
  onModelDownloadComplete,
  onModelDownloadError,
  onModelDownloadProgress,
  setDictationSettings,
  setTtsSettings,
  type ModelDownloadProgress,
  type OnnxModel,
  type OnnxModelKindParam,
  type WhisperModel,
} from "../api";
import { formatBytes } from "../lib/format";
import Spinner from "./Spinner";

type ModelKind = "whisper" | "parakeet" | "tts";

type Props = {
  kind?: ModelKind;
};

const SECTION_META: Record<
  ModelKind,
  { title: string; description: string; onnxKind?: OnnxModelKindParam }
> = {
  whisper: {
    title: "Whisper models",
    description:
      "Download local Whisper GGUF models for offline dictation. Select the active model used by the whisper STT engine.",
  },
  parakeet: {
    title: "Parakeet ONNX models",
    description:
      "Download sherpa-onnx Parakeet STT models for local transcription when the parakeet engine is enabled at build time.",
    onnxKind: "parakeet",
  },
  tts: {
    title: "Local TTS ONNX models",
    description:
      "Download sherpa-onnx VITS models for offline read-aloud when sherpa-tts is enabled at build time.",
    onnxKind: "tts",
  },
};

export default function ModelManager({ kind = "whisper" }: Props) {
  const meta = SECTION_META[kind];
  const [whisperModels, setWhisperModels] = useState<WhisperModel[]>([]);
  const [onnxModels, setOnnxModels] = useState<OnnxModel[]>([]);
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [activeModel, setActiveModel] = useState<string>("");
  const [progress, setProgress] = useState<ModelDownloadProgress | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const modelIdsRef = useRef<Set<string>>(new Set());

  const refreshInstalled = useCallback(() => {
    if (kind === "whisper") {
      return listInstalledWhisperModels()
        .then((ids) => setInstalled(new Set(ids)))
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
    }
    const onnxKind = meta.onnxKind!;
    return listInstalledOnnxModels(onnxKind)
      .then((ids) => setInstalled(new Set(ids)))
      .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
  }, [kind, meta.onnxKind]);

  useEffect(() => {
    setLoading(true);
    if (kind === "whisper") {
      Promise.all([listWhisperModels(), getDictationSettings()])
        .then(([catalog, settings]) => {
          modelIdsRef.current = new Set(catalog.map((m) => m.id));
          setWhisperModels(catalog);
          setActiveModel(settings.active_model ?? "");
        })
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
    } else {
      const onnxKind = meta.onnxKind!;
      Promise.all([listOnnxModels(onnxKind), kind === "tts" ? getTtsSettings() : null])
        .then(([catalog, ttsSettings]) => {
          modelIdsRef.current = new Set(catalog.map((m) => m.id));
          setOnnxModels(catalog);
          if (kind === "tts" && ttsSettings) {
            setActiveModel(ttsSettings.active_model ?? "");
          }
        })
        .catch((e) => setStatus(e instanceof Error ? e.message : String(e)));
    }
    refreshInstalled().finally(() => setLoading(false));
  }, [kind, meta.onnxKind, refreshInstalled]);

  useEffect(() => {
    const unsubs = Promise.all([
      (async () => {
        const unsub = await onModelDownloadProgress((p) => {
          setProgress(p);
        });
        return unsub;
      })(),
      (async () => {
        const unsub = await onModelDownloadComplete((modelId) => {
          if (!modelIdsRef.current.has(modelId)) return;
          refreshInstalled();
          setProgress(null);
          setBusy(false);
          setStatus(`Downloaded ${modelId}.`);
        });
        return unsub;
      })(),
      (async () => {
        const unsub = await onModelDownloadError((modelId, message) => {
          if (!modelIdsRef.current.has(modelId)) return;
          setProgress(null);
          setBusy(false);
          setStatus(`${modelId}: ${message}`);
        });
        return unsub;
      })(),
    ]);

    return () => {
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, [refreshInstalled]);

  const onDownload = async (modelId: string) => {
    setBusy(true);
    setStatus(null);
    setProgress(null);
    try {
      if (kind === "whisper") {
        await downloadWhisperModel(modelId);
      } else {
        await downloadOnnxModel(meta.onnxKind!, modelId);
      }
      setStatus(`Downloading ${modelId}…`);
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const onSelectActive = async (modelId: string) => {
    setActiveModel(modelId);
    setBusy(true);
    setStatus(null);
    try {
      if (kind === "whisper") {
        const current = await getDictationSettings();
        await setDictationSettings({
          ...current,
          active_model: modelId || null,
        });
      } else if (kind === "tts") {
        const current = await getTtsSettings();
        await setTtsSettings({
          ...current,
          active_model: modelId || null,
        });
      }
      setStatus(modelId ? `Active model set to ${modelId}.` : "Active model cleared.");
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const progressPercent =
    progress && progress.bytes_total > 0
      ? Math.round((progress.bytes_received / progress.bytes_total) * 100)
      : 0;

  const catalog =
    kind === "whisper"
      ? whisperModels.map((m) => ({
          id: m.id,
          display_name: m.display_name,
          size_bytes: m.size_bytes,
        }))
      : onnxModels.map((m) => ({
          id: m.id,
          display_name: m.display_name,
          size_bytes: m.size_bytes,
        }));

  const showActiveSelector = kind === "whisper" || kind === "tts";

  return (
    <section className="kea-card" style={{ marginTop: 24 }}>
      <h3 style={{ margin: "0 0 8px" }}>{meta.title}</h3>
      <p className="kea-muted" style={{ marginTop: 0, marginBottom: 16 }}>
        {meta.description}
      </p>
      {loading ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 120 }}>
          <Spinner size={16} />
          <span className="kea-muted">Loading models…</span>
        </div>
      ) : (
        <>
      {showActiveSelector && (
        <label style={{ display: "block", marginBottom: 16 }}>
          <span className="kea-label">Active model</span>
          <select
            className="kea-select"
            value={activeModel}
            onChange={(e) => onSelectActive(e.target.value)}
            disabled={busy}
            style={{ minWidth: 260 }}
          >
            <option value="">None</option>
            {catalog.map((m) => (
              <option key={m.id} value={m.id}>
                {m.display_name}
                {installed.has(m.id) ? " (installed)" : ""}
              </option>
            ))}
          </select>
        </label>
      )}
      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
        <thead>
          <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
            <th style={{ padding: "8px 4px" }}>Model</th>
            <th style={{ padding: "8px 4px" }}>Size</th>
            <th style={{ padding: "8px 4px" }}>Status</th>
            <th style={{ padding: "8px 4px" }} />
          </tr>
        </thead>
        <tbody>
          {catalog.map((model) => {
            const isInstalled = installed.has(model.id);
            const isDownloading = progress?.model_id === model.id;
            return (
              <tr key={model.id} style={{ borderBottom: "1px solid var(--border)" }}>
                <td style={{ padding: "10px 4px" }}>
                  <strong>{model.display_name}</strong>
                  <div className="kea-muted" style={{ fontSize: 12 }}>
                    {model.id}
                  </div>
                </td>
                <td style={{ padding: "10px 4px" }}>{formatBytes(model.size_bytes)}</td>
                <td style={{ padding: "10px 4px" }}>
                  {isInstalled ? (
                    <span style={{ color: "var(--accent)" }}>Installed</span>
                  ) : isDownloading ? (
                    <span style={{ color: "var(--accent)" }}>
                      {progressPercent}% ({formatBytes(progress!.bytes_received)} /{" "}
                      {formatBytes(progress!.bytes_total)})
                    </span>
                  ) : (
                    <span className="kea-muted">Not installed</span>
                  )}
                </td>
                <td style={{ padding: "10px 4px", textAlign: "right" }}>
                  {!isInstalled && (
                    <button
                      type="button"
                      className="kea-btn"
                      onClick={() => onDownload(model.id)}
                      disabled={busy}
                    >
                      Install
                    </button>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
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
