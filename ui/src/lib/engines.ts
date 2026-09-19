import {
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listOnnxModels,
  listWhisperModels,
  type Binding,
  type ModelKindParam,
  type OnnxModel,
  type Provider,
  type WhisperModel,
} from "../api";

/**
 * The one place that knows which engines exist.
 *
 * Every screen used to re-spell the engine ids — the picker, the slot
 * diagnosis, the Models page — so a new provider meant editing six files and
 * hoping none was missed. They all read this table instead: adding an engine
 * is one entry here plus whatever loader it needs.
 *
 * The table is deliberately not a complete abstraction. The local catalogs
 * come from genuinely different commands (`list_whisper_models` vs
 * `list_onnx_models(kind)`), so each local engine carries its own loader
 * rather than pretending one call serves all of them.
 */

/** What a feature asks an engine for. */
export type Capability = "llm" | "stt" | "tts";

export const CAPABILITY_LABELS: Record<Capability, string> = {
  llm: "Writing & rewriting",
  stt: "Speech to text",
  tts: "Text to speech",
};

/** The built-in provider that points at a server on this Mac. */
const LOCAL_PROVIDER_REF = "local-llm";

/** The built-in cloud provider the OpenAI engines fall back to. */
const OPENAI_PROVIDER_REF = "openai";

/** Built-in providers whose engines are branded rather than OpenAI-shaped. */
const ANTHROPIC_PROVIDER_REF = "anthropic";
const DEEPGRAM_PROVIDER_REF = "deepgram";
const ELEVENLABS_PROVIDER_REF = "elevenlabs";

/**
 * Whether this provider is reached with an API key. Only the local server is
 * keyless — asked here so a second keyless provider, or a rename of
 * "local-llm", is one edit.
 */
export const needsKey = (provider: Provider): boolean =>
  provider.provider_ref !== LOCAL_PROVIDER_REF;

/** The catalog a local engine downloads from — the IPC `kind` parameter. */
export type DownloadKind = ModelKindParam;

export type LocalModel = WhisperModel | OnnxModel;

/**
 * How a local engine reaches its catalog. Listing and installed-ids stay two
 * calls so a caller that degrades on failure can degrade each on its own.
 */
export type EngineCatalog = {
  kind: DownloadKind;
  /** Section heading on the Models page. */
  title: string;
  list: () => Promise<LocalModel[]>;
  listInstalled: () => Promise<string[]>;
};

/**
 * The single option a *local* engine with no catalog contributes to a picker.
 * There is nothing to download and no key to enter, so the row is fixed and
 * always ready — whatever it speaks or listens with is a feature setting, not
 * part of the binding.
 */
export type LocalOption = {
  label: string;
  /** The second line under the label, e.g. "on this Mac · your voices". */
  detail: string;
};

/** The single option a cloud speech engine contributes to a picker. */
export type CloudOption = {
  label: string;
  model: string | null;
  /** Renders the voice dropdown and sends the picked voice on activation. */
  cloudVoices?: boolean;
};

/** Everything `describeBinding` needs to name a binding in plain language. */
export type BindingContext = {
  providers: Provider[];
  modelNames: Map<string, string>;
};

export type EngineId =
  | "openai"
  | "anthropic"
  | "openai-compatible"
  | "whisper"
  | "parakeet"
  | "apple-speech"
  | "openai-stt"
  | "deepgram-stt"
  | "elevenlabs-stt"
  | "sherpa-tts"
  | "system-tts"
  | "openai-tts";

export type EngineSpec = {
  id: EngineId;
  capability: Capability;
  /** Runs on this Mac, so it needs a downloaded model and never a key. */
  runsLocally: boolean;
  /** Local engines only. */
  catalog?: EngineCatalog;
  /** The one row a local engine with no catalog offers. */
  localOption?: LocalOption;
  /**
   * Whether the engine actually applies the dictation language setting. The
   * ONNX transducer has no language parameter, so a language passed to it can
   * only be dropped (crates/engines/src/stt/parakeet.rs) — which is why this
   * is asked of the engine rather than assumed of the capability.
   */
  acceptsLanguage?: boolean;
  /** Whose key a cloud engine uses when the binding names no provider. */
  credentialRef?: string;
  /** The cloud engine that also serves providers other than `credentialRef`. */
  acceptsAnyProvider?: boolean;
  /**
   * Whether the capability's settings-level `active_model` fallback holds ids
   * this engine can load (see `applyDefaultChoice`).
   */
  ownsActiveModel?: boolean;
  /** Cloud engines that appear in the picker as one fixed row. */
  cloudOption?: CloudOption;
  /** The sentence a non-technical user reads for a binding on this engine. */
  describe: (binding: Binding, ctx: BindingContext) => string;
};

const withModel = (text: string, binding: Binding) =>
  `${text}${binding.model ? ` · ${binding.model}` : ""}`;

/** "OpenAI · gpt-4o-mini" — a vendor-branded engine. */
const vendorSummary = (vendor: string) => (binding: Binding) => withModel(vendor, binding);

/** "Groq · llama-3" — an engine that is whatever server the provider names. */
const providerSummary = (binding: Binding, ctx: BindingContext) =>
  withModel(
    ctx.providers.find((p) => p.provider_ref === binding.provider_ref)?.name ?? "Custom server",
    binding,
  );

/** "Whisper Base — on this Mac" */
const localSummary = (binding: Binding, ctx: BindingContext) => {
  const name = binding.model
    ? ctx.modelNames.get(binding.model) ?? binding.model
    : binding.engine_id;
  return `${name} — on this Mac`;
};

/**
 * "Samantha — on this Mac". A system voice is named by an identifier
 * ("com.apple.voice.compact.en-US.Samantha") whose last component is the only
 * part a person recognises; the full string belongs in a tooltip, not a
 * sentence.
 */
const systemVoiceSummary = (binding: Binding) => {
  const name = binding.model?.split(".").pop();
  return `${name ? name : "System voice"} — on this Mac`;
};

const onnxCatalog = (kind: "parakeet" | "tts", title: string): EngineCatalog => ({
  kind,
  title,
  list: () => listOnnxModels(kind),
  listInstalled: () => listInstalledOnnxModels(kind),
});

/** Declaration order is the order the picker and the Models page list them. */
export const ENGINES: Record<EngineId, EngineSpec> = {
  openai: {
    id: "openai",
    capability: "llm",
    runsLocally: false,
    credentialRef: OPENAI_PROVIDER_REF,
    describe: vendorSummary("OpenAI"),
  },
  anthropic: {
    id: "anthropic",
    capability: "llm",
    runsLocally: false,
    credentialRef: ANTHROPIC_PROVIDER_REF,
    describe: vendorSummary("Anthropic"),
  },
  "openai-compatible": {
    id: "openai-compatible",
    capability: "llm",
    runsLocally: false,
    // No ref of its own: a binding always names the provider it talks to, and
    // an auto-resolved one falls back to OpenAI's.
    credentialRef: OPENAI_PROVIDER_REF,
    acceptsAnyProvider: true,
    describe: providerSummary,
  },
  whisper: {
    id: "whisper",
    capability: "stt",
    runsLocally: true,
    ownsActiveModel: true,
    acceptsLanguage: true,
    catalog: {
      kind: "whisper",
      title: "Speech to text — Whisper",
      list: () => listWhisperModels(),
      listInstalled: () => listInstalledWhisperModels(),
    },
    describe: localSummary,
  },
  parakeet: {
    id: "parakeet",
    capability: "stt",
    runsLocally: true,
    // One catalog, two model families: Moonshine and Parakeet install the
    // same way and load through the same engine, differing only in the
    // sherpa config their bundle is read through (see `ModelEntry::onnx_kind`).
    // The section is named for what it holds, not for one of them.
    catalog: onnxCatalog("parakeet", "Speech to text — Moonshine & Parakeet"),
    describe: localSummary,
  },
  // No catalog and no key: Apple's recognizer downloads nothing and manages
  // nothing, so it is one fixed row like `system-tts`. It is registered only
  // where the OS reports on-device recognition (see `register_apple_stt_engine`),
  // which is why nothing here has to ask whether it would work.
  "apple-speech": {
    id: "apple-speech",
    capability: "stt",
    runsLocally: true,
    // The recognizer picks its model per locale, so the dictation language
    // setting genuinely reaches it — unlike the ONNX transducer.
    acceptsLanguage: true,
    localOption: {
      label: "Apple speech",
      detail: "on this Mac · no download",
    },
    describe: () => "Apple speech — on this Mac",
  },
  "openai-stt": {
    id: "openai-stt",
    capability: "stt",
    runsLocally: false,
    credentialRef: OPENAI_PROVIDER_REF,
    // Also how Groq is reached: an OpenAI-shaped /audio/transcriptions behind
    // the `groq` provider_ref, which is why Groq has no engine of its own.
    acceptsAnyProvider: true,
    cloudOption: { label: "OpenAI whisper-1", model: "whisper-1" },
    describe: vendorSummary("OpenAI"),
  },
  "deepgram-stt": {
    id: "deepgram-stt",
    capability: "stt",
    runsLocally: false,
    acceptsLanguage: true,
    credentialRef: DEEPGRAM_PROVIDER_REF,
    cloudOption: { label: "Deepgram Nova-3", model: "nova-3" },
    describe: vendorSummary("Deepgram"),
  },
  "elevenlabs-stt": {
    id: "elevenlabs-stt",
    capability: "stt",
    runsLocally: false,
    acceptsLanguage: true,
    credentialRef: ELEVENLABS_PROVIDER_REF,
    cloudOption: { label: "ElevenLabs Scribe", model: "scribe_v1" },
    describe: vendorSummary("ElevenLabs"),
  },
  "sherpa-tts": {
    id: "sherpa-tts",
    capability: "tts",
    runsLocally: true,
    ownsActiveModel: true,
    catalog: onnxCatalog("tts", "Text to speech — Local voices"),
    describe: localSummary,
  },
  "system-tts": {
    id: "system-tts",
    capability: "tts",
    runsLocally: true,
    // The one local engine with no catalog: its voices are whatever the user
    // has installed through System Settings, so there is nothing to download
    // and nothing for the Models page to show.
    localOption: {
      label: "System voice",
      detail: "on this Mac · the voices macOS ships",
    },
    describe: systemVoiceSummary,
  },
  "openai-tts": {
    id: "openai-tts",
    capability: "tts",
    runsLocally: false,
    credentialRef: OPENAI_PROVIDER_REF,
    acceptsAnyProvider: true,
    cloudOption: { label: "OpenAI voices", model: null, cloudVoices: true },
    describe: vendorSummary("OpenAI"),
  },
};

export const ENGINE_LIST: EngineSpec[] = Object.values(ENGINES);

/** The spec for an engine id, or undefined for one this build does not know. */
export const engineSpec = (engineId: string): EngineSpec | undefined =>
  ENGINES[engineId as EngineId];

export const enginesFor = (capability: Capability): EngineSpec[] =>
  ENGINE_LIST.filter((e) => e.capability === capability);

/** Engines that run on this Mac. */
export const runsLocally = (engineId: string): boolean =>
  engineSpec(engineId)?.runsLocally ?? false;

/**
 * Whether this engine has anything to download at all.
 *
 * Not the same question as [`runsLocally`], and the difference is load-bearing:
 * Apple's recognizer and the system voices run here and install nothing, so
 * diagnosing them against the "is that model on disk?" rule would report a
 * missing download for a binding that works perfectly.
 */
export const needsDownload = (engineId: string): boolean =>
  engineSpec(engineId)?.catalog !== undefined;

/** Whether a dictation language can be chosen for this engine at all. */
export const acceptsLanguage = (engineId: string): boolean =>
  engineSpec(engineId)?.acceptsLanguage === true;

/** An engine whose models live on disk, so `catalog` is always there. */
export type LocalEngineSpec = EngineSpec & { catalog: EngineCatalog };

/** Local engines, in listing order, optionally narrowed to one capability. */
export const catalogEngines = (capability?: Capability): LocalEngineSpec[] =>
  ENGINE_LIST.filter(
    (e): e is LocalEngineSpec => !!e.catalog && (!capability || e.capability === capability),
  );

/**
 * Loads a local engine's catalog and the ids already on disk.
 *
 * Takes anything that *has* a catalog rather than an `EngineSpec`, because not
 * every catalog belongs to a bindable engine: the streaming recogniser is
 * downloaded and deleted like any other model while nothing resolves to it
 * (see `ModelsPage`). Every `EngineSpec` still satisfies this.
 */
export async function loadCatalog(
  spec: { catalog?: EngineCatalog },
): Promise<{ models: LocalModel[]; installed: string[] }> {
  if (!spec.catalog) return { models: [], installed: [] };
  const [models, installed] = await Promise.all([
    spec.catalog.list(),
    spec.catalog.listInstalled(),
  ]);
  // A retired model stays visible while it is still on disk — otherwise the
  // gigabyte someone already downloaded has no Remove button anywhere. It is
  // only no longer *offered*, which is the whole difference between retiring
  // an entry and deleting it.
  const onDisk = new Set(installed);
  return {
    models: models.filter((m) => !m.deprecated || onDisk.has(m.id)),
    installed,
  };
}

/**
 * The cloud engine a pick lands on when the user names a provider but no
 * engine. Only text has a separate engine for the built-in OpenAI provider;
 * the speech engines talk to any OpenAI-shaped server.
 */
export function cloudEngineFor(capability: Capability, providerRef: string | null): EngineId {
  const cloud = enginesFor(capability).filter((e) => !e.runsLocally);
  const spec =
    cloud.find((e) => !e.acceptsAnyProvider && e.credentialRef === providerRef) ??
    cloud.find((e) => e.acceptsAnyProvider) ??
    cloud[0];
  return spec.id;
}

/** The provider whose key a binding needs, if it needs one at all. */
export function credentialRefFor(binding: Binding): string | null {
  const spec = engineSpec(binding.engine_id);
  if (spec?.runsLocally) return null;
  if (binding.provider_ref) return binding.provider_ref;
  // Auto-resolved cloud bindings carry no provider_ref; the OpenAI engines
  // fall back to the built-in "openai" provider.
  return spec?.credentialRef ?? null;
}

/**
 * Whether the capability's settings-level `active_model` holds ids this engine
 * can load — the fallback is only meaningful for the one local engine that
 * owns it (crates/features/src/dictation.rs, tts.rs).
 */
export function ownsActiveModel(capability: Capability, engineId: string): boolean {
  const spec = engineSpec(engineId);
  return spec?.capability === capability && spec.ownsActiveModel === true;
}

/** Turns a binding into the sentence a non-technical user reads. */
export function describeBinding(
  binding: Binding | null,
  ctx: BindingContext,
): string | null {
  if (!binding) return null;
  return engineSpec(binding.engine_id)?.describe(binding, ctx) ?? binding.model ?? binding.engine_id;
}
