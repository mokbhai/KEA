import {
  downloadOnnxModel,
  downloadWhisperModel,
  getDictationSettings,
  getTtsSettings,
  hasCredential,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listOnnxModels,
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
    providers.forEach((p) => {
      const isOpenAi = p.provider_ref === "openai";
      const isLocal = p.provider_ref === "local-llm";
      opts.push({
        id: `llm:${p.provider_ref}`,
        label: p.name,
        detail: isLocal ? "your server" : isOpenAi ? "cloud" : "custom server",
        status: isLocal ? "No key needed" : cloudStatus(p.provider_ref),
        ready: isLocal || (keyByRef.get(p.provider_ref) ?? false),
        engine: isOpenAi ? "openai" : "openai-compatible",
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
 * The settings-level model is the *fallback* used when a binding carries no
 * model of its own, so it must mirror the choice for every engine — not only
 * the two local ones. A Parakeet or cloud pick that left a stale whisper id
 * behind would hand that id to the new engine the moment a binding without a
 * model resolved. Picks with no model of their own (OpenAI voices) clear it.
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
    const settings = await getDictationSettings();
    if (settings.active_model !== choice.model) {
      await setDictationSettings({ ...settings, active_model: choice.model });
    }
  } else if (capability === "tts") {
    const settings = await getTtsSettings();
    const next = {
      ...settings,
      active_model: choice.model,
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
