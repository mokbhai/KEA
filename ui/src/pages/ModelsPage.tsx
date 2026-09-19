import { useCallback, useEffect, useMemo, useState } from "react";
import {
  cancelModelDownload,
  deleteModel,
  downloadOnnxModel,
  downloadWhisperModel,
  getBinding,
  previewVoice,
  type Binding,
  type OnnxModel,
  type WhisperModel,
} from "../api";
import Banner from "../components/Banner";
import LoadingBlock from "../components/LoadingBlock";
import { Row, RowGroup } from "../components/SettingsRow";
import { useModelDownloads } from "../hooks/useModelDownloads";
import {
  catalogEngines,
  loadCatalog,
  type Capability,
  type DownloadKind,
  type LocalEngineSpec,
} from "../lib/engines";
import { formatBytes, toMessage } from "../lib/format";

/** One section per local engine, in the order the engine table lists them. */
const CATALOGS = catalogEngines();

const CAPABILITIES = [...new Set(CATALOGS.map((spec) => spec.capability))];

type Section = {
  spec: LocalEngineSpec;
  models: (WhisperModel | OnnxModel)[];
  installed: Set<string>;
};

export default function ModelsPage() {
  const [sections, setSections] = useState<Section[] | null>(null);
  const [bindings, setBindings] = useState<Map<Capability, Binding | null>>(new Map());
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [catalogs, defaults] = await Promise.all([
        Promise.all(CATALOGS.map((spec) => loadCatalog(spec))),
        Promise.all(
          CAPABILITIES.map((capability) =>
            getBinding("default", capability).catch(() => null),
          ),
        ),
      ]);
      setSections(
        CATALOGS.map((spec, i) => ({
          spec,
          models: catalogs[i].models,
          installed: new Set(catalogs[i].installed),
        })),
      );
      setBindings(new Map(CAPABILITIES.map((capability, i) => [capability, defaults[i]])));
    } catch (e) {
      setError(toMessage(e));
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

  const download = async (kind: DownloadKind, modelId: string) => {
    setError(null);
    try {
      if (kind === "whisper") {
        await downloadWhisperModel(modelId);
      } else {
        await downloadOnnxModel(kind, modelId);
      }
    } catch (e) {
      setError(toMessage(e));
    }
  };

  // The row clears itself off the back of the error event the backend emits
  // for a cancel, so there is nothing local to unwind here.
  const cancel = async (kind: DownloadKind, modelId: string) => {
    setError(null);
    try {
      await cancelModelDownload(kind, modelId);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const remove = async (section: Section, model: WhisperModel | OnnxModel) => {
    const binding = bindings.get(section.spec.capability);
    const isActiveDefault = binding?.model === model.id;
    const capLabel = section.spec.capability === "stt" ? "speech-to-text" : "text-to-speech";
    const message = isActiveDefault
      ? `Remove ${model.display_name}? It's your current ${capLabel} default — the default will be unset until you choose another.`
      : `Remove ${model.display_name}?`;
    if (!window.confirm(message)) return;
    setError(null);
    try {
      await deleteModel(section.spec.catalog.kind, model.id);
      await refresh();
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const preview = async (engineId: string, modelId: string) => {
    setError(null);
    try {
      await previewVoice(engineId, modelId, null);
    } catch (e) {
      setError(toMessage(e));
    }
  };

  if (sections === null) {
    return (
      <LoadingBlock label="Loading models…" minHeight={120} />
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
        <section key={section.spec.id} style={{ marginBottom: 28 }}>
          <h2 style={{ margin: "0 0 12px" }}>{section.spec.catalog.title}</h2>
          {section.models.length === 0 ? (
            <p className="kea-muted" style={{ margin: 0 }}>
              No models available.
            </p>
          ) : (
            <RowGroup aria-label={section.spec.catalog.title}>
              {section.models.map((model) => {
                const installed = section.installed.has(model.id);
                const progress = progressById.get(model.id);
                const percent =
                  progress && progress.bytes_total > 0
                    ? Math.round((progress.bytes_received / progress.bytes_total) * 100)
                    : null;
                // A retired model only reaches this list while it is still
                // installed (see `loadCatalog`), so the note is advice on what
                // to do next, not an explanation of why it is offered.
                const hint = [
                  model.language,
                  formatBytes(model.size_bytes),
                  ...(model.deprecated ? ["no longer recommended — a newer model is smaller and better"] : []),
                ].join(" · ");
                return (
                  <Row key={model.id} label={model.display_name} hint={hint}>
                    {progress ? (
                      <>
                        <span className="kea-muted">
                          {percent !== null ? `${percent}%` : "Downloading…"}
                        </span>
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Cancel download of ${model.display_name}`}
                          onClick={() => void cancel(section.spec.catalog.kind, model.id)}
                        >
                          Cancel
                        </button>
                      </>
                    ) : installed ? (
                      <>
                        {section.spec.capability === "tts" && (
                          <button
                            type="button"
                            className="kea-btn"
                            aria-label={`Preview ${model.display_name}`}
                            onClick={() => void preview(section.spec.id, model.id)}
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
                        onClick={() => void download(section.spec.catalog.kind, model.id)}
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
