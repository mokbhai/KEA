import { useCallback, useEffect, useMemo, useState } from "react";
import {
  deleteModel,
  downloadOnnxModel,
  downloadWhisperModel,
  getBinding,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listOnnxModels,
  listWhisperModels,
  previewVoice,
  type Binding,
  type OnnxModel,
  type WhisperModel,
} from "../api";
import Banner from "../components/Banner";
import { Row, RowGroup } from "../components/SettingsRow";
import Spinner from "../components/Spinner";
import { useModelDownloads } from "../hooks/useModelDownloads";
import { formatBytes } from "../lib/format";

type CatalogKind = "whisper" | "parakeet" | "tts";

type Section = {
  kind: CatalogKind;
  title: string;
  capability: "stt" | "tts";
  models: (WhisperModel | OnnxModel)[];
  installed: Set<string>;
};

const SECTION_TITLES: Record<CatalogKind, string> = {
  whisper: "Speech to text — Whisper",
  parakeet: "Speech to text — Parakeet",
  tts: "Text to speech — Local voices",
};

export default function ModelsPage() {
  const [sections, setSections] = useState<Section[] | null>(null);
  const [bindings, setBindings] = useState<{ stt: Binding | null; tts: Binding | null }>({
    stt: null,
    tts: null,
  });
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [
        whisper,
        whisperInstalled,
        parakeet,
        parakeetInstalled,
        tts,
        ttsInstalled,
        sttBinding,
        ttsBinding,
      ] = await Promise.all([
        listWhisperModels(),
        listInstalledWhisperModels(),
        listOnnxModels("parakeet"),
        listInstalledOnnxModels("parakeet"),
        listOnnxModels("tts"),
        listInstalledOnnxModels("tts"),
        getBinding("default", "stt").catch(() => null),
        getBinding("default", "tts").catch(() => null),
      ]);
      setSections([
        {
          kind: "whisper",
          title: SECTION_TITLES.whisper,
          capability: "stt",
          models: whisper,
          installed: new Set(whisperInstalled),
        },
        {
          kind: "parakeet",
          title: SECTION_TITLES.parakeet,
          capability: "stt",
          models: parakeet,
          installed: new Set(parakeetInstalled),
        },
        {
          kind: "tts",
          title: SECTION_TITLES.tts,
          capability: "tts",
          models: tts,
          installed: new Set(ttsInstalled),
        },
      ]);
      setBindings({ stt: sttBinding, tts: ttsBinding });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSections([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const catalogIds = useMemo(
    () => new Set((sections ?? []).flatMap((s) => s.models.map((m) => m.id))),
    [sections],
  );

  const { progressById } = useModelDownloads({
    catalogIds,
    onComplete: () => void refresh(),
    onError: (modelId, message) => setError(`${modelId}: ${message}`),
  });

  const download = async (kind: CatalogKind, modelId: string) => {
    setError(null);
    try {
      if (kind === "whisper") {
        await downloadWhisperModel(modelId);
      } else {
        await downloadOnnxModel(kind, modelId);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const remove = async (section: Section, model: WhisperModel | OnnxModel) => {
    const binding = bindings[section.capability];
    const isActiveDefault = binding?.model === model.id;
    const capLabel = section.capability === "stt" ? "speech-to-text" : "text-to-speech";
    const message = isActiveDefault
      ? `Remove ${model.display_name}? It's your current ${capLabel} default — the default will be unset until you choose another.`
      : `Remove ${model.display_name}?`;
    if (!window.confirm(message)) return;
    setError(null);
    try {
      await deleteModel(section.kind, model.id);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const preview = async (modelId: string) => {
    setError(null);
    try {
      await previewVoice("sherpa-tts", modelId, null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  if (sections === null) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 120 }}>
        <Spinner size={16} />
        <span className="kea-muted">Loading models…</span>
      </div>
    );
  }

  return (
    <div>
      <h1 style={{ marginTop: 0 }}>Models</h1>
      <p className="kea-muted" style={{ marginBottom: 24 }}>
        Download models to run speech features privately on this Mac.
      </p>

      {error && <Banner variant="error">{error}</Banner>}

      {sections.map((section) => (
        <section key={section.kind} style={{ marginBottom: 28 }}>
          <h2 style={{ margin: "0 0 12px" }}>{section.title}</h2>
          {section.models.length === 0 ? (
            <p className="kea-muted" style={{ margin: 0 }}>
              No models available.
            </p>
          ) : (
            <RowGroup aria-label={section.title}>
              {section.models.map((model) => {
                const installed = section.installed.has(model.id);
                const progress = progressById.get(model.id);
                const percent =
                  progress && progress.bytes_total > 0
                    ? Math.round((progress.bytes_received / progress.bytes_total) * 100)
                    : null;
                return (
                  <Row
                    key={model.id}
                    label={model.display_name}
                    hint={`${model.language} · ${formatBytes(model.size_bytes)}`}
                  >
                    {progress ? (
                      <span className="kea-muted">
                        {percent !== null ? `${percent}%` : "Downloading…"}
                      </span>
                    ) : installed ? (
                      <>
                        {section.kind === "tts" && (
                          <button
                            type="button"
                            className="kea-btn"
                            aria-label={`Preview ${model.display_name}`}
                            onClick={() => void preview(model.id)}
                          >
                            ▶
                          </button>
                        )}
                        <span className="kea-saved">Installed ✓</span>
                        <button
                          type="button"
                          className="kea-btn"
                          onClick={() => void remove(section, model)}
                        >
                          Remove
                        </button>
                      </>
                    ) : (
                      <button
                        type="button"
                        className="kea-btn"
                        onClick={() => void download(section.kind, model.id)}
                      >
                        Download
                      </button>
                    )}
                  </Row>
                );
              })}
            </RowGroup>
          )}
        </section>
      ))}
    </div>
  );
}
