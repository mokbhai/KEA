import {
  downloadOnnxModel,
  downloadWhisperModel,
  getDictationSettings,
  getTtsSettings,
  hasCredential,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listLlmEngines,
  listOnnxModels,
  listSttEngines,
  listTtsEngines,
  listWhisperModels,
  setBinding,
  setDictationSettings,
  setTtsSettings,
  type OnnxModel,
  type Provider,
  type WhisperModel,
} from "../api";
import { formatBytes } from "./format";

export type Capability = "llm" | "stt" | "tts";

export const CAPABILITY_LABELS: Record<Capability, string> = {
  llm: "Writing & rewriting",
  stt: "Speech to text",
  tts: "Text to speech",
};

export const OPENAI_TTS_VOICES = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

export type DownloadKind = "whisper" | "parakeet" | "tts";

/** A concrete, pickable default for a capability (shared between the
 * DefaultsPicker popover and the onboarding wizard). */
export type CapabilityOption = {
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

/** Which providers have a saved credential ("local-llm" never needs one). */
export async function loadKeyStates(providers: Provider[]): Promise<Map<string, boolean>> {
  const keyByRef = new Map<string, boolean>();
  await Promise.all(
    providers.map(async (p) => {
      const saved =
        p.provider_ref === "local-llm"
          ? true
          : await hasCredential(p.provider_ref).catch(() => false);
      keyByRef.set(p.provider_ref, saved);
    }),
  );
  return keyByRef;
}

function localOption(
  m: WhisperModel | OnnxModel,
  engine: string,
  downloadKind: DownloadKind,
  installed: Set<string>,
): CapabilityOption {
  return {
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
  };
}

/** Builds the selectable options for one capability from the model catalogs
 * and provider list — identical for the picker and the wizard. */
export async function buildCapabilityOptions(
  capability: Capability,
  providers: Provider[],
  keyByRef: Map<string, boolean>,
): Promise<CapabilityOption[]> {
  const cloudStatus = (ref: string) => (keyByRef.get(ref) ? "key ✓" : "Key missing");
  const hasOpenAi = providers.some((p) => p.provider_ref === "openai");

  // Only offer engines this build actually registered. Local engines are
  // behind cargo features, so a build without them would otherwise list
  // Whisper, download a gigabyte for it, and only fail on save with
  // "unknown engine id".
  const lister =
    capability === "stt" ? listSttEngines : capability === "tts" ? listTtsEngines : listLlmEngines;
  const available = new Set((await lister()).map((e) => e.id));

  const opts: CapabilityOption[] = [];
  if (capability === "stt") {
    const [whisper, whisperInstalled, parakeet, parakeetInstalled] = await Promise.all([
      listWhisperModels(),
      listInstalledWhisperModels(),
      listOnnxModels("parakeet"),
      listInstalledOnnxModels("parakeet"),
    ]);
    const wInstalled = new Set(whisperInstalled);
    const pInstalled = new Set(parakeetInstalled);
    if (available.has("whisper")) {
      whisper.forEach((m) => opts.push(localOption(m, "whisper", "whisper", wInstalled)));
    }
    if (available.has("parakeet")) {
      parakeet.forEach((m) => opts.push(localOption(m, "parakeet", "parakeet", pInstalled)));
    }
    if (hasOpenAi && available.has("openai-stt")) {
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
    if (available.has("sherpa-tts")) {
      voices.forEach((m) => opts.push(localOption(m, "sherpa-tts", "tts", vInstalled)));
    }
    if (hasOpenAi && available.has("openai-tts")) {
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
    providers.forEach((p) => {
      const isOpenAi = p.provider_ref === "openai";
      const isLocal = p.provider_ref === "local-llm";
      const engine = isOpenAi ? "openai" : "openai-compatible";
      if (!available.has(engine)) return;
      opts.push({
        id: `llm:${p.provider_ref}`,
        label: p.name,
        detail: isLocal ? "your server" : isOpenAi ? "cloud" : "custom server",
        status: isLocal ? "No key needed" : cloudStatus(p.provider_ref),
        ready: isLocal || (keyByRef.get(p.provider_ref) ?? false),
        engine,
        model: null,
        providerRef: p.provider_ref,
      });
    });
  }
  return opts;
}

export type DefaultChoice = {
  engine: string;
  model: string | null;
  providerRef: string | null;
};

/** Which binding row a choice is written to. */
export type BindingTarget = { feature: string; slot: string };

/** The capability-wide default row for a capability. */
export const defaultTarget = (capability: Capability): BindingTarget => ({
  feature: "default",
  slot: capability,
});

/**
 * Whether writing this target should also update the global dictation / TTS
 * settings. Those settings are the *fallback* model & voice for their own
 * feature, so a capability default and the feature that owns them stay in
 * sync — but an override for another feature (e.g. meetings) must not move
 * them, since its binding already carries the model.
 */
function ownsSettings(capability: Capability, target: BindingTarget): boolean {
  if (target.feature === "default") return true;
  // Only the STT and TTS settings have a per-feature owner; there is no
  // equivalent for text, so an llm override never writes one.
  if (capability === "llm") return false;
  return capability === "stt" ? target.feature === "dictation" : target.feature === "tts";
}

/**
 * Writes a binding — the ("default", capability) row unless `target` says
 * otherwise — and keeps the per-feature active_model / active_voice settings
 * consistent. The single write path shared by the DefaultsPicker (both the
 * defaults on AI Providers and the per-feature overrides) and the wizard.
 *
 * The settings-level model is the fallback for whichever engine a *model-less*
 * binding later resolves to — it is not scoped to the engine picked here
 * (crates/features/src/dictation.rs: `binding.model.or(settings.active_model)`).
 * Only the local engine that owns it can load what it holds, so a pick either
 * writes a model that engine could load or leaves the setting alone. Two
 * things follow:
 *
 * - Never mirror a foreign id. A cloud id ("whisper-1") or a parakeet id
 *   (separate ONNX storage) would fail as "model not installed" the next time
 *   a model-less whisper binding resolved — and those are reachable, the local
 *   onboarding path writes whisper/null.
 * - Never clear it either. A retained whisper id is inert: every reachable
 *   non-whisper binding carries its own model, so the fallback is read only
 *   when a model-less whisper binding resolves, which is exactly the case it
 *   exists for. Clearing would break that case (whisper hard-errors with
 *   "whisper requires a model id"; sherpa falls back to the first catalog
 *   voice, which may not be installed). So item #3's "stale id" is cosmetic:
 *   an id that disagrees with the current binding is deliberately kept,
 *   because nothing else ever reads it. A model whose *files* are gone is a
 *   different matter, and the backend clears it on delete_model.
 */
export async function applyDefaultChoice(
  capability: Capability,
  choice: DefaultChoice,
  voice?: string | null,
  target?: BindingTarget,
): Promise<void> {
  const to = target ?? defaultTarget(capability);
  await setBinding(to.feature, to.slot, choice.engine, choice.model, choice.providerRef);
  if (!ownsSettings(capability, to)) return;
  if (capability === "stt") {
    // Only a whisper pick with a model of its own has anything to say here.
    if (choice.engine !== "whisper" || choice.model === null) return;
    const settings = await getDictationSettings();
    if (settings.active_model !== choice.model) {
      await setDictationSettings({ ...settings, active_model: choice.model });
    }
  } else if (capability === "tts") {
    // Same rule for the model; the voice is independent — it belongs to the
    // cloud engine and is passed only by picks that carry one.
    const keepsModel = choice.engine !== "sherpa-tts" || choice.model === null;
    const settings = await getTtsSettings();
    const next = {
      ...settings,
      active_model: keepsModel ? settings.active_model : choice.model,
      active_voice: voice ?? settings.active_voice,
    };
    if (
      next.active_model !== settings.active_model ||
      next.active_voice !== settings.active_voice
    ) {
      await setTtsSettings(next);
    }
  }
}

/** Kicks off the download for a not-yet-installed local option. */
export async function startOptionDownload(option: CapabilityOption): Promise<void> {
  if (!option.downloadKind || !option.model) return;
  if (option.downloadKind === "whisper") {
    await downloadWhisperModel(option.model);
  } else {
    await downloadOnnxModel(option.downloadKind, option.model);
  }
}
