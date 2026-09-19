import {
  cancelModelDownload,
  downloadOnnxModel,
  downloadWhisperModel,
  getDictationSettings,
  getTtsSettings,
  hasCredential,
  listLlmEngines,
  listSttEngines,
  listTtsEngines,
  setBinding,
  setDictationSettings,
  setTtsSettings,
  type OnnxModel,
  type Provider,
  type WhisperModel,
} from "../api";
import {
  CAPABILITY_LABELS,
  cloudEngineFor,
  engineSpec,
  enginesFor,
  loadCatalog,
  needsKey,
  ownsActiveModel,
  type Capability,
  type DownloadKind,
} from "./engines";
import { formatBytes } from "./format";

export { CAPABILITY_LABELS };
export type { Capability, DownloadKind };

export const OPENAI_TTS_VOICES = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

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

/** Which providers have a saved credential (a keyless provider never needs one). */
export async function loadKeyStates(providers: Provider[]): Promise<Map<string, boolean>> {
  const keyByRef = new Map<string, boolean>();
  await Promise.all(
    providers.map(async (p) => {
      const saved = needsKey(p) ? await hasCredential(p.provider_ref).catch(() => false) : true;
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

  // Only offer engines this build actually registered. Local engines are
  // behind cargo features, so a build without them would otherwise list
  // Whisper, download a gigabyte for it, and only fail on save with
  // "unknown engine id".
  const lister =
    capability === "stt" ? listSttEngines : capability === "tts" ? listTtsEngines : listLlmEngines;
  const available = new Set((await lister()).map((e) => e.id));
  const specs = enginesFor(capability).filter((spec) => available.has(spec.id));

  const opts: CapabilityOption[] = [];
  if (capability === "llm") {
    // Text engines are picked per provider, not per catalog: one row for each
    // provider the user has connected.
    providers.forEach((p) => {
      const engine = cloudEngineFor("llm", p.provider_ref);
      if (!available.has(engine)) return;
      const keyless = !needsKey(p);
      // A provider with an engine of its own is a known vendor; anything on
      // the generic engine is the user's own server.
      const custom = engineSpec(engine)?.acceptsAnyProvider === true;
      opts.push({
        id: `llm:${p.provider_ref}`,
        label: p.name,
        detail: keyless ? "your server" : custom ? "custom server" : "cloud",
        status: keyless ? "No key needed" : cloudStatus(p.provider_ref),
        ready: keyless || (keyByRef.get(p.provider_ref) ?? false),
        engine,
        model: null,
        providerRef: p.provider_ref,
      });
    });
    return opts;
  }

  // The catalogs the local engines need, fetched together as before.
  const catalogs = new Map(
    await Promise.all(
      specs
        .filter((spec) => spec.catalog)
        .map(async (spec) => [spec.id, await loadCatalog(spec)] as const),
    ),
  );

  specs.forEach((spec) => {
    const loaded = catalogs.get(spec.id);
    if (spec.catalog && loaded) {
      const installed = new Set(loaded.installed);
      const kind = spec.catalog.kind;
      loaded.models.forEach((m) => opts.push(localOption(m, spec.id, kind, installed)));
      return;
    }
    if (spec.runsLocally && !spec.catalog) {
      // A local engine with no catalog has nothing to download and no key to
      // enter, so it is one fixed, always-ready row. Before this it had
      // neither a catalog nor a cloudOption and fell through the bottom of
      // this loop — registered in Rust, and bindable nowhere in the UI.
      // Labelled by its id if it declared no row of its own: a picker entry
      // named "system-tts" is ugly, an engine that silently disappears is
      // the bug this arm exists to close.
      const fixed = spec.localOption;
      opts.push({
        id: spec.id,
        label: fixed?.label ?? spec.id,
        detail: fixed?.detail ?? "on this Mac",
        status: "ready ✓",
        ready: true,
        engine: spec.id,
        // The binding names the engine only. What it speaks with is the
        // feature's own setting (Read-aloud's voice), not part of the choice
        // made here.
        model: null,
        providerRef: null,
      });
      return;
    }
    const cloud = spec.cloudOption;
    const ref = spec.credentialRef;
    // A cloud engine is only offerable once its provider exists.
    if (!cloud || !ref || !providers.some((p) => p.provider_ref === ref)) return;
    opts.push({
      id: cloud.model ? `${spec.id}:${cloud.model}` : spec.id,
      label: cloud.label,
      detail: "cloud",
      status: cloudStatus(ref),
      ready: keyByRef.get(ref) ?? false,
      engine: spec.id,
      model: cloud.model,
      providerRef: ref,
      ...(cloud.cloudVoices ? { cloudVoices: true } : {}),
    });
  });
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
  // Only a pick by the engine that owns the fallback, carrying a model of its
  // own, has anything to say here.
  const ownsFallback = ownsActiveModel(capability, choice.engine);
  if (capability === "stt") {
    if (!ownsFallback || choice.model === null) return;
    const settings = await getDictationSettings();
    if (settings.active_model !== choice.model) {
      await setDictationSettings({ ...settings, active_model: choice.model });
    }
  } else if (capability === "tts") {
    // Same rule for the model; the voice is independent — it belongs to the
    // cloud engine and is passed only by picks that carry one.
    const keepsModel = !ownsFallback || choice.model === null;
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

/** Stops an option's download and discards whatever it had transferred. */
export async function cancelOptionDownload(option: CapabilityOption): Promise<void> {
  if (!option.downloadKind || !option.model) return;
  await cancelModelDownload(option.downloadKind, option.model);
}
