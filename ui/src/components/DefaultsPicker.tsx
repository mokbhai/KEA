import { useEffect, useMemo, useRef, useState } from "react";
import { getBinding, listProviders, previewVoice, type Binding, type Provider } from "../api";
import { useModelDownloads } from "../hooks/useModelDownloads";
import type { PendingActivation } from "../hooks/usePendingActivation";
import {
  buildCapabilityOptions,
  defaultTarget,
  loadKeyStates,
  CAPABILITY_LABELS,
  OPENAI_TTS_VOICES,
  type BindingTarget,
  type Capability,
  type CapabilityOption,
} from "../lib/capabilityDefaults";
import { cloudEngineFor } from "../lib/engines";
import { toMessage } from "../lib/format";
import ProviderModels from "./ProviderModels";
import LoadingBlock from "./LoadingBlock";

export { CAPABILITY_LABELS };
export type { Capability };

type PickerOption = CapabilityOption;

type Props = {
  capability: Capability;
  open: boolean;
  onClose: () => void;
  /**
   * The download-then-activate flow, owned by whoever renders this popover.
   * It has to outlive the picker: closing mid-download must not drop the
   * choice. `usePendingActivation(onApplied)` produces it.
   */
  activation: PendingActivation;
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

function findMatch(options: PickerOption[], binding: Binding): string | null {
  const match =
    options.find(
      (o) =>
        o.engine === binding.engine_id &&
        (!binding.provider_ref || o.providerRef === binding.provider_ref) &&
        (o.model ?? null) === (binding.model ?? null),
    ) ??
    options.find(
      (o) =>
        o.engine === binding.engine_id &&
        (!binding.provider_ref || o.providerRef === binding.provider_ref),
    );
  return match?.id ?? null;
}

export default function DefaultsPicker({
  capability,
  open,
  onClose,
  activation,
  target,
  title,
}: Props) {
  const resolved = target ?? defaultTarget(capability);
  const targetFeature = resolved.feature;
  const targetSlot = resolved.slot;
  const [loading, setLoading] = useState(true);
  const [options, setOptions] = useState<PickerOption[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [binding, setBinding] = useState<Binding | null>(null);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [voice, setVoice] = useState(OPENAI_TTS_VOICES[0]);
  const [advProvider, setAdvProvider] = useState("openai");
  const [advModel, setAdvModel] = useState("");
  const [advEngine, setAdvEngine] = useState("");

  const choices = useRef(new Map<string, PickerOption>());
  const voiceRef = useRef(voice);
  voiceRef.current = voice;

  const { savedId, error, clearFeedback, setError } = activation;
  // Only one activation at a time, wherever it was started from.
  const busy = activation.pending !== null;
  // A download started for another slot keeps running; only show it as
  // pending on the picker that started it.
  const pending =
    activation.pending &&
    activation.pending.capability === capability &&
    activation.pending.target.feature === targetFeature &&
    activation.pending.target.slot === targetSlot
      ? activation.pending.option
      : null;

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    clearFeedback();

    (async () => {
      try {
        const [providerList, binding] = await Promise.all([
          listProviders(),
          getBinding(targetFeature, targetSlot)
            .then(
              (binding) =>
                binding ?? (targetFeature !== "default" ? getBinding("default", capability) : null),
            )
            .catch(() => null),
        ]);
        const keyByRef = await loadKeyStates(providerList);
        const opts = await buildCapabilityOptions(capability, providerList, keyByRef);

        if (!cancelled) {
          setProviders(providerList);
          setBinding(binding);
          setExpandedId(null);
          setAdvProvider(providerList[0]?.provider_ref ?? "openai");
          setOptions(opts);
          setCurrentId(binding ? findMatch(opts, binding) : null);
        }
      } catch (e) {
        if (!cancelled) setError(toMessage(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [open, capability, targetFeature, targetSlot, clearFeedback, setError]);

  const downloadableIds = useMemo(
    () => new Set(options.flatMap((o) => (o.downloadKind && o.model ? [o.model] : []))),
    [options],
  );

  // Display only — activating is the hoisted flow's job. Watching the whole
  // catalog (not just the pending model) keeps a download started on the
  // Models page visible here, and stops an already-installed row from still
  // offering to download.
  const { progressById } = useModelDownloads({
    catalogIds: downloadableIds,
    onComplete: (modelId) =>
      setOptions((prev) =>
        prev.map((o) =>
          o.model === modelId ? { ...o, installed: true, status: "installed ✓" } : o,
        ),
      ),
  });

  // Reflect a finished download in the list even when this picker was closed
  // while it ran.
  useEffect(() => {
    if (!savedId) return;
    setOptions((prev) =>
      prev.map((o) => (o.id === savedId ? { ...o, installed: true, status: "installed ✓" } : o)),
    );
    setCurrentId(savedId);
    const choice = choices.current.get(savedId);
    if (choice)
      setBinding({
        engine_id: choice.engine,
        provider_ref: choice.providerRef,
        model: choice.model,
      });
  }, [savedId]);

  const pick = async (option: PickerOption) => {
    choices.current.set(option.id, option);
    await activation.start({
      capability,
      option,
      // Only the options that expose the voice dropdown carry a voice; the
      // dropdown always has a value, so sending it for a local pick would
      // overwrite the user's saved cloud voice with "alloy". Same predicate
      // the preview button uses below.
      voice: option.cloudVoices ? voiceRef.current : null,
      target: { feature: targetFeature, slot: targetSlot },
    });
  };

  const preview = async (option: PickerOption) => {
    setError(null);
    try {
      await previewVoice(
        option.engine,
        option.model,
        option.cloudVoices ? voiceRef.current : null,
        option.providerRef,
      );
    } catch (e) {
      setError(toMessage(e));
    }
  };

  const applyAdvanced = async () => {
    const engine = advEngine.trim() || cloudEngineFor(capability, advProvider);
    await pick({
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
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
      aria-label={title ?? `Choose ${CAPABILITY_LABELS[capability]}`}
    >
      <div className="kea-picker__header">
        <strong>
          {title ?? `Choose ${CAPABILITY_LABELS[capability].toLowerCase()}`}
          {binding?.model &&
            ` — ${providers.find((p) => p.provider_ref === binding.provider_ref)?.name ?? binding.engine_id} · ${binding.model}`}
        </strong>
        <button type="button" className="kea-icon-btn" aria-label="Close" onClick={onClose}>
          ✕
        </button>
      </div>
      {loading ? (
        <LoadingBlock label="Loading options…" minHeight={80} />
      ) : (
        <>
          <ul className="kea-picker__options">
            {options
              .filter((o) => o.ready)
              .map((option) => {
                const progress = option.model ? progressById.get(option.model) : undefined;
                const isPending = pending?.id === option.id;
                const isCurrent = option.providerRef
                  ? binding?.provider_ref === option.providerRef &&
                    binding.engine_id === option.engine
                  : currentId === option.id;
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
                          isCurrent ? " kea-picker__option--current" : ""
                        }`}
                        onClick={() =>
                          option.providerRef
                            ? setExpandedId((id) => (id === option.id ? null : option.id))
                            : void pick(option)
                        }
                        aria-expanded={option.providerRef ? expandedId === option.id : undefined}
                        disabled={!option.ready || busy}
                        aria-current={isCurrent ? "true" : undefined}
                      >
                        <span className="kea-picker__name">
                          {option.label}
                          {option.providerRef &&
                            binding?.provider_ref === option.providerRef &&
                            binding?.model &&
                            ` · ${binding.model}`}
                          <span className="kea-picker__detail">{option.detail}</span>
                        </span>
                        <span className="kea-picker__status">{statusText}</span>
                      </button>
                      {isPending && (
                        <button
                          type="button"
                          className="kea-btn"
                          aria-label={`Cancel download of ${option.label}`}
                          onClick={() => void activation.cancel()}
                        >
                          Cancel
                        </button>
                      )}
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
                    {option.providerRef && expandedId === option.id && (
                      <ProviderModels
                        key={option.id}
                        option={option}
                        capability={capability}
                        selected={
                          binding?.provider_ref === option.providerRef ? binding.model : null
                        }
                        busy={busy}
                        onPick={pick}
                      />
                    )}
                  </li>
                );
              })}
          </ul>
          {options.some((o) => !o.ready) && (
            <section aria-label="Not set up">
              <h3>Not set up</h3>
              <ul className="kea-picker__options">
                {options
                  .filter((o) => !o.ready)
                  .map((o) => (
                    <li key={o.id} className="kea-picker__row">
                      <span className="kea-picker__option">
                        {o.label}
                        <span className="kea-picker__status">{o.status}</span>
                      </span>
                      <a
                        className="kea-btn"
                        href="#ai-providers"
                        onClick={onClose}
                        aria-label={`Set up ${o.label} in AI Providers`}
                      >
                        Set up
                      </a>
                    </li>
                  ))}
              </ul>
            </section>
          )}
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
                  placeholder={cloudEngineFor(capability, advProvider)}
                />
              </label>
              <button type="button" className="kea-btn" onClick={() => void applyAdvanced()}>
                Use this
              </button>
            </div>
          </details>
          {busy && !pending && (
            <p className="kea-muted" style={{ marginTop: 8 }}>
              Another download is still finishing — one at a time.
            </p>
          )}
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
