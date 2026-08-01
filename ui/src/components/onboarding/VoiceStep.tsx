import { useEffect, useMemo, useRef, useState } from "react";
import { getBinding, getTtsSettings, listProviders, previewVoice } from "../../api";
import { useModelDownloads } from "../../hooks/useModelDownloads";
import { useSavedFlash } from "../../hooks/useSavedFlash";
import {
  applyDefaultChoice,
  buildCapabilityOptions,
  loadKeyStates,
  startOptionDownload,
  OPENAI_TTS_VOICES,
  type CapabilityOption,
} from "../../lib/capabilityDefaults";
import Spinner from "../Spinner";
import type { AiChoice } from "./ConnectStep";

type Props = {
  setCommit: (fn: (() => Promise<void>) | null) => void;
  aiChoice: AiChoice | null;
};

export default function VoiceStep({ setCommit, aiChoice }: Props) {
  const [loading, setLoading] = useState(true);
  const [sttLocal, setSttLocal] = useState<CapabilityOption | null>(null);
  const [sttCloud, setSttCloud] = useState<CapabilityOption | null>(null);
  const [ttsVoices, setTtsVoices] = useState<CapabilityOption[]>([]);
  const [ttsCloudReady, setTtsCloudReady] = useState(false);
  const [sttChoice, setSttChoice] = useState<"local" | "cloud">("local");
  const [ttsMode, setTtsMode] = useState<"local" | "cloud">("local");
  const [cloudVoice, setCloudVoice] = useState(OPENAI_TTS_VOICES[0]);
  const [localVoiceId, setLocalVoiceId] = useState<string | null>(null);
  const [sttSeededFromBinding, setSttSeededFromBinding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedKey, flashSaved] = useSavedFlash();

  // Downloads the user already kicked off, so Continue doesn't restart them.
  const startedRef = useRef<Set<string>>(new Set());
  const ttsCloudReadyRef = useRef(false);
  ttsCloudReadyRef.current = ttsCloudReady;

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const providers = await listProviders();
        const keyByRef = await loadKeyStates(providers);
        const [sttOpts, ttsOpts, sttBinding, ttsBinding] = await Promise.all([
          buildCapabilityOptions("stt", providers, keyByRef),
          buildCapabilityOptions("tts", providers, keyByRef),
          getBinding("default", "stt").catch(() => null),
          getBinding("default", "tts").catch(() => null),
        ]);
        if (cancelled) return;

        // An existing default wins over the recommended preselection, so
        // re-running the wizard and pressing Continue keeps prior choices.
        const boundLocalStt =
          sttBinding && sttBinding.engine_id !== "openai-stt"
            ? sttOpts.find(
                (o) =>
                  o.engine === sttBinding.engine_id &&
                  (o.model ?? null) === (sttBinding.model ?? null),
              ) ?? null
            : null;
        const local =
          boundLocalStt ??
          sttOpts.find((o) => o.engine === "whisper" && o.model === "whisper-base") ??
          sttOpts.find((o) => o.engine === "whisper") ??
          null;
        const cloud = sttOpts.find((o) => o.engine === "openai-stt") ?? null;
        const voices = ttsOpts.filter((o) => o.engine === "sherpa-tts");
        const cloudTts = ttsOpts.find((o) => o.engine === "openai-tts") ?? null;
        const hasKey = keyByRef.get("openai") ?? false;
        const preferCloud = hasKey && aiChoice !== "local" && aiChoice !== "custom";

        setSttLocal(local);
        setSttCloud(cloud);
        setTtsVoices(voices);
        setTtsCloudReady(Boolean(cloudTts?.ready));
        setSttSeededFromBinding(Boolean(boundLocalStt));
        setSttChoice(
          sttBinding
            ? sttBinding.engine_id === "openai-stt" && cloud?.ready
              ? "cloud"
              : "local"
            : preferCloud && cloud?.ready
              ? "cloud"
              : "local",
        );
        setTtsMode(
          ttsBinding
            ? ttsBinding.engine_id === "openai-tts" && cloudTts?.ready
              ? "cloud"
              : "local"
            : preferCloud && cloudTts?.ready
              ? "cloud"
              : "local",
        );
        const boundLocalVoice =
          ttsBinding?.engine_id === "sherpa-tts"
            ? voices.find((v) => (v.model ?? null) === (ttsBinding.model ?? null)) ?? null
            : null;
        setLocalVoiceId((boundLocalVoice ?? voices[0])?.id ?? null);
        if (ttsBinding?.engine_id === "openai-tts") {
          const settings = await getTtsSettings().catch(() => null);
          if (
            !cancelled &&
            settings?.active_voice &&
            OPENAI_TTS_VOICES.includes(settings.active_voice)
          ) {
            setCloudVoice(settings.active_voice);
          }
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
  }, [aiChoice]);

  const stateRef = useRef({
    sttChoice,
    sttLocal,
    sttCloud,
    ttsMode,
    ttsVoices,
    localVoiceId,
    cloudVoice,
  });
  stateRef.current = {
    sttChoice,
    sttLocal,
    sttCloud,
    ttsMode,
    ttsVoices,
    localVoiceId,
    cloudVoice,
  };

  const catalogIds = useMemo(() => {
    const ids = new Set<string>();
    if (sttLocal?.model) ids.add(sttLocal.model);
    ttsVoices.forEach((v) => {
      if (v.model) ids.add(v.model);
    });
    return ids;
  }, [sttLocal, ttsVoices]);

  const markInstalled = (modelId: string) => {
    setSttLocal((prev) =>
      prev?.model === modelId ? { ...prev, installed: true, status: "installed ✓" } : prev,
    );
    setTtsVoices((prev) =>
      prev.map((v) =>
        v.model === modelId ? { ...v, installed: true, status: "installed ✓" } : v,
      ),
    );
  };

  const { progressById } = useModelDownloads({
    catalogIds,
    onComplete: (modelId) => {
      markInstalled(modelId);
      const { sttChoice, sttLocal, ttsMode, ttsVoices, localVoiceId } = stateRef.current;
      // Download-then-activate: if the finished model is the current choice,
      // write its default binding now (same flow as the defaults picker).
      if (sttChoice === "local" && sttLocal?.model === modelId) {
        void applyDefaultChoice("stt", { ...sttLocal, model: modelId })
          .then(() => flashSaved("stt"))
          .catch((e) => setError(e instanceof Error ? e.message : String(e)));
      }
      const voice = ttsVoices.find((v) => v.id === localVoiceId);
      if (ttsMode === "local" && voice?.model === modelId) {
        void applyDefaultChoice("tts", voice)
          .then(() => flashSaved("tts"))
          .catch((e) => setError(e instanceof Error ? e.message : String(e)));
      }
    },
    onError: (modelId, message) => {
      startedRef.current.delete(modelId);
      setError(`${modelId}: ${message}`);
    },
  });

  const startDownload = (option: CapabilityOption) => {
    if (!option.model || option.installed || startedRef.current.has(option.model)) return;
    startedRef.current.add(option.model);
    setError(null);
    startOptionDownload(option).catch((e) => {
      if (option.model) startedRef.current.delete(option.model);
      setError(e instanceof Error ? e.message : String(e));
    });
  };

  useEffect(() => {
    setCommit(async () => {
      const { sttChoice, sttLocal, sttCloud, ttsMode, ttsVoices, localVoiceId, cloudVoice } =
        stateRef.current;

      const stt = sttChoice === "cloud" && sttCloud?.ready ? sttCloud : sttLocal;
      if (stt) {
        await applyDefaultChoice("stt", stt);
        if (stt.downloadKind && stt.model && !stt.installed && !startedRef.current.has(stt.model)) {
          startedRef.current.add(stt.model);
          void startOptionDownload(stt).catch(() => {});
        }
      }

      if (ttsMode === "cloud" && ttsCloudReadyRef.current) {
        // model null matches what the defaults picker writes for OpenAI voices.
        await applyDefaultChoice(
          "tts",
          { engine: "openai-tts", model: null, providerRef: "openai" },
          cloudVoice,
        );
      } else {
        const voice = ttsVoices.find((v) => v.id === localVoiceId);
        if (voice) {
          await applyDefaultChoice("tts", voice);
          if (voice.model && !voice.installed && !startedRef.current.has(voice.model)) {
            startedRef.current.add(voice.model);
            void startOptionDownload(voice).catch(() => {});
          }
        }
      }
    });
    return () => setCommit(null);
  }, [setCommit]);

  const preview = async () => {
    setError(null);
    try {
      if (ttsMode === "cloud") {
        await previewVoice("openai-tts", null, cloudVoice);
      } else {
        const voice = ttsVoices.find((v) => v.id === localVoiceId);
        if (voice) await previewVoice(voice.engine, voice.model, null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const progressFor = (option: CapabilityOption | null | undefined) => {
    if (!option?.model) return null;
    const p = progressById.get(option.model);
    if (!p) return startedRef.current.has(option.model) && !option.installed ? 0 : null;
    return p.bytes_total > 0 ? Math.round((p.bytes_received / p.bytes_total) * 100) : 0;
  };

  if (loading) {
    return (
      <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 80 }}>
        <Spinner size={16} />
        <span className="kea-muted">Loading voice options…</span>
      </div>
    );
  }

  const localProgress = progressFor(sttLocal);
  const selectedLocalVoice = ttsVoices.find((v) => v.id === localVoiceId) ?? null;
  const voiceProgress = progressFor(selectedLocalVoice);

  return (
    <div>
      <section style={{ marginBottom: 24 }}>
        <h2 style={{ margin: "0 0 4px" }}>Speech to text</h2>
        <p className="kea-muted" style={{ margin: "0 0 12px" }}>
          How KEA turns your voice into words for dictation and meetings.
        </p>
        <div className="kea-choice-group" role="radiogroup" aria-label="Speech to text">
          {sttLocal && (
            <label
              className={`kea-choice${sttChoice === "local" ? " kea-choice--selected" : ""}`}
            >
              <input
                type="radio"
                name="stt-choice"
                value="local"
                checked={sttChoice === "local"}
                onChange={() => {
                  setSttChoice("local");
                  startDownload(sttLocal);
                }}
              />
              <span className="kea-choice__body">
                <span className="kea-choice__title">
                  {sttLocal.label}
                  {sttSeededFromBinding ? "" : " — recommended"}
                </span>
                <span className="kea-choice__hint">
                  Runs on this Mac, works offline.{" "}
                  {sttLocal.installed
                    ? "Installed ✓"
                    : localProgress !== null
                      ? `Downloading ${localProgress}% — you can keep going.`
                      : `${sttLocal.model ? sizeLabel(sttLocal) : ""} download.`}
                </span>
                {savedKey === "stt" && <span className="kea-saved">Saved ✓</span>}
              </span>
            </label>
          )}
          {sttCloud && (
            <label
              className={`kea-choice${sttChoice === "cloud" ? " kea-choice--selected" : ""}`}
            >
              <input
                type="radio"
                name="stt-choice"
                value="cloud"
                checked={sttChoice === "cloud"}
                onChange={() => setSttChoice("cloud")}
                disabled={!sttCloud.ready}
              />
              <span className="kea-choice__body">
                <span className="kea-choice__title">OpenAI cloud</span>
                <span className="kea-choice__hint">
                  {sttCloud.ready
                    ? "Uses your OpenAI account. No download."
                    : "Needs an OpenAI key (step 2)."}
                </span>
              </span>
            </label>
          )}
        </div>
      </section>

      <section>
        <h2 style={{ margin: "0 0 4px" }}>Text to speech</h2>
        <p className="kea-muted" style={{ margin: "0 0 12px" }}>
          The voice KEA uses to read text aloud.
        </p>
        <div
          className="kea-choice-group"
          role="radiogroup"
          aria-label="Text to speech"
          style={{ marginBottom: 12 }}
        >
          <label className={`kea-choice${ttsMode === "local" ? " kea-choice--selected" : ""}`}>
            <input
              type="radio"
              name="tts-choice"
              value="local"
              checked={ttsMode === "local"}
              onChange={() => setTtsMode("local")}
            />
            <span className="kea-choice__body">
              <span className="kea-choice__title">A voice on this Mac</span>
              <span className="kea-choice__hint">Private, works offline.</span>
            </span>
          </label>
          <label className={`kea-choice${ttsMode === "cloud" ? " kea-choice--selected" : ""}`}>
            <input
              type="radio"
              name="tts-choice"
              value="cloud"
              checked={ttsMode === "cloud"}
              onChange={() => setTtsMode("cloud")}
              disabled={!ttsCloudReady}
            />
            <span className="kea-choice__body">
              <span className="kea-choice__title">OpenAI cloud voices</span>
              <span className="kea-choice__hint">
                {ttsCloudReady
                  ? "Uses your OpenAI account. No download."
                  : "Needs an OpenAI key (step 2)."}
              </span>
            </span>
          </label>
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
          {ttsMode === "cloud" ? (
            <select
              className="kea-select"
              aria-label="Voice"
              value={cloudVoice}
              onChange={(e) => setCloudVoice(e.target.value)}
            >
              {OPENAI_TTS_VOICES.map((v) => (
                <option key={v} value={v}>
                  {v}
                </option>
              ))}
            </select>
          ) : ttsVoices.length > 0 ? (
            <select
              className="kea-select"
              aria-label="Voice"
              value={localVoiceId ?? ""}
              onChange={(e) => {
                setLocalVoiceId(e.target.value);
                const voice = ttsVoices.find((v) => v.id === e.target.value);
                if (voice) startDownload(voice);
              }}
            >
              {ttsVoices.map((v) => (
                <option key={v.id} value={v.id}>
                  {v.label} — {v.installed ? "installed" : sizeLabel(v)}
                </option>
              ))}
            </select>
          ) : (
            <span className="kea-muted">No local voices available yet.</span>
          )}
          <button
            type="button"
            className="kea-btn"
            aria-label="Preview voice"
            onClick={() => void preview()}
            disabled={ttsMode === "local" && !selectedLocalVoice?.installed}
          >
            ▶
          </button>
          {ttsMode === "local" && voiceProgress !== null && (
            <span className="kea-muted">Downloading {voiceProgress}%…</span>
          )}
          {savedKey === "tts" && <span className="kea-saved">Saved ✓</span>}
        </div>
      </section>

      {error && (
        <p className="kea-muted" style={{ margin: "12px 0 0" }}>
          {error}
        </p>
      )}
    </div>
  );
}

function sizeLabel(option: CapabilityOption): string {
  // Option statuses look like "148.0 MB ⬇" for missing models; reuse that.
  return option.status.replace(" ⬇", "");
}
