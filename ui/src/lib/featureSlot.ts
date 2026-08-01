import {
  getBinding,
  listInstalledOnnxModels,
  listInstalledWhisperModels,
  listLlmEngines,
  listOnnxModels,
  listSttEngines,
  listTtsEngines,
  listProviders,
  listWhisperModels,
  type Binding,
  type Provider,
} from "../api";
import { loadKeyStates, type Capability } from "./capabilityDefaults";

/** Where the "fix this" action on a blocked feature sends the user. */
export type FixTarget = "ai-providers" | "models" | "picker";

/** One AI slot a feature page depends on, in plain language. */
export type SlotSpec = {
  /** Feature id as stored in bindings ("dictation", "tts", "rewrite", "meetings"). */
  feature: string;
  /** Slot id within the feature ("llm", "stt", "tts"). */
  slot: string;
  capability: Capability;
  /** Human label for the row and the banner ("Speech to text"). */
  label: string;
};

export type BlockedReason = {
  message: string;
  actionLabel: string;
  fix: FixTarget;
};

export type SlotSource = "override" | "default" | "auto" | "none";

export type SlotStatus = {
  spec: SlotSpec;
  /** The feature-scoped override binding, if the user set one. */
  override: Binding | null;
  /** What the backend resolver will actually use (override > default > auto). */
  effective: Binding | null;
  source: SlotSource;
  /** Plain-language description of `effective` ("OpenAI · gpt-4o-mini"). */
  summary: string | null;
  /** Non-null when the feature cannot run as configured. */
  blocked: BlockedReason | null;
};

/** Engines that run on this Mac and therefore need a downloaded model. */
const LOCAL_ENGINES = new Set(["whisper", "parakeet", "sherpa-tts"]);

/** Turns a binding into the sentence a non-technical user reads. */
export function describeBinding(
  binding: Binding | null,
  ctx: { providers: Provider[]; modelNames: Map<string, string> },
): string | null {
  if (!binding) return null;
  const providerName = (ref: string | null) =>
    ctx.providers.find((p) => p.provider_ref === ref)?.name ?? null;

  switch (binding.engine_id) {
    case "openai":
    case "openai-stt":
    case "openai-tts":
      return `OpenAI${binding.model ? ` · ${binding.model}` : ""}`;
    case "openai-compatible":
      return `${providerName(binding.provider_ref) ?? "Custom server"}${
        binding.model ? ` · ${binding.model}` : ""
      }`;
    case "whisper":
    case "parakeet":
    case "sherpa-tts": {
      const name = binding.model
        ? ctx.modelNames.get(binding.model) ?? binding.model
        : binding.engine_id;
      return `${name} — on this Mac`;
    }
    default:
      return binding.model ?? binding.engine_id;
  }
}

type Env = {
  providers: Provider[];
  keyByRef: Map<string, boolean>;
  engineIds: Record<Capability, Set<string>>;
  installed: Set<string>;
  modelNames: Map<string, string>;
};

const emptyEngineIds = (): Record<Capability, Set<string>> => ({
  llm: new Set<string>(),
  stt: new Set<string>(),
  tts: new Set<string>(),
});

async function loadEnv(capabilities: Set<Capability>): Promise<Env> {
  const providers = await listProviders().catch(() => [] as Provider[]);
  const keyByRef = await loadKeyStates(providers).catch(
    () => new Map<string, boolean>(),
  );

  const engineIds = emptyEngineIds();
  await Promise.all(
    [...capabilities].map(async (capability) => {
      const lister =
        capability === "stt"
          ? listSttEngines
          : capability === "tts"
            ? listTtsEngines
            : listLlmEngines;
      const engines = await lister().catch(() => []);
      engineIds[capability] = new Set(engines.map((e) => e.id));
    }),
  );

  const needsStt = capabilities.has("stt");
  const needsTts = capabilities.has("tts");
  const [whisper, whisperInstalled, parakeet, parakeetInstalled, voices, voicesInstalled] =
    await Promise.all([
      needsStt ? listWhisperModels().catch(() => []) : [],
      needsStt ? listInstalledWhisperModels().catch(() => []) : [],
      needsStt ? listOnnxModels("parakeet").catch(() => []) : [],
      needsStt ? listInstalledOnnxModels("parakeet").catch(() => []) : [],
      needsTts ? listOnnxModels("tts").catch(() => []) : [],
      needsTts ? listInstalledOnnxModels("tts").catch(() => []) : [],
    ]);

  const modelNames = new Map<string, string>();
  [...whisper, ...parakeet, ...voices].forEach((m) => modelNames.set(m.id, m.display_name));

  return {
    providers,
    keyByRef,
    engineIds,
    installed: new Set([...whisperInstalled, ...parakeetInstalled, ...voicesInstalled]),
    modelNames,
  };
}

/** The provider whose key a binding needs, if it needs one at all. */
function credentialRef(binding: Binding): string | null {
  if (LOCAL_ENGINES.has(binding.engine_id)) return null;
  if (binding.provider_ref) return binding.provider_ref;
  // Auto-resolved cloud bindings carry no provider_ref; the OpenAI engines
  // fall back to the built-in "openai" provider.
  return binding.engine_id.startsWith("openai") ? "openai" : null;
}

function blockedReason(
  spec: SlotSpec,
  effective: Binding | null,
  env: Env,
): BlockedReason | null {
  const what = spec.label.toLowerCase();

  if (!effective) {
    return {
      message: `no ${what} is set up yet.`,
      actionLabel: "Choose…",
      fix: "picker",
    };
  }

  if (!env.engineIds[spec.capability].has(effective.engine_id)) {
    return {
      message: `the chosen ${what} isn't available in this build.`,
      actionLabel: "Change…",
      fix: "picker",
    };
  }

  if (LOCAL_ENGINES.has(effective.engine_id) && effective.model) {
    if (!env.installed.has(effective.model)) {
      const name = env.modelNames.get(effective.model) ?? effective.model;
      return {
        message: `${name} isn't downloaded yet.`,
        actionLabel: "Open Models",
        fix: "models",
      };
    }
  }

  const ref = credentialRef(effective);
  if (ref && !env.keyByRef.get(ref)) {
    const name = env.providers.find((p) => p.provider_ref === ref)?.name ?? ref;
    return {
      message: `${name} needs an API key before ${what} can run.`,
      actionLabel: "Open AI Providers",
      fix: "ai-providers",
    };
  }

  return null;
}

/**
 * Mirrors the backend resolver (override > capability default > single-engine
 * auto-pick) and reports, in plain language, whatever blocks the feature.
 */
export async function loadSlotStatuses(specs: SlotSpec[]): Promise<SlotStatus[]> {
  const capabilities = new Set(specs.map((s) => s.capability));
  const env = await loadEnv(capabilities);

  const defaults = new Map<Capability, Binding | null>();
  await Promise.all(
    [...capabilities].map(async (capability) => {
      defaults.set(capability, await getBinding("default", capability).catch(() => null));
    }),
  );

  return Promise.all(
    specs.map(async (spec) => {
      const override = await getBinding(spec.feature, spec.slot).catch(() => null);
      const engines = env.engineIds[spec.capability];

      let effective: Binding | null = null;
      let source: SlotSource = "none";
      if (override) {
        effective = override;
        source = "override";
      } else {
        // A default naming a missing engine falls through, exactly like
        // SlotResolver does; a feature override does not.
        const fallback = defaults.get(spec.capability) ?? null;
        if (fallback && engines.has(fallback.engine_id)) {
          effective = fallback;
          source = "default";
        } else if (engines.size === 1) {
          effective = {
            engine_id: [...engines][0],
            model: null,
            provider_ref: null,
          };
          source = "auto";
        }
      }

      return {
        spec,
        override,
        effective,
        source,
        summary: describeBinding(effective, env),
        blocked: blockedReason(spec, effective, env),
      };
    }),
  );
}
