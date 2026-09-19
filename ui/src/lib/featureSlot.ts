import {
  getBinding,
  getDictationSettings,
  getTtsSettings,
  listLlmEngines,
  listSttEngines,
  listTtsEngines,
  listProviders,
  type Binding,
  type Provider,
} from "../api";
import { loadKeyStates } from "./capabilityDefaults";
import {
  catalogEngines,
  credentialRefFor,
  describeBinding,
  needsDownload,
  type Capability,
  type LocalModel,
} from "./engines";
import { toMessage } from "./format";

export { describeBinding };

/**
 * Why a feature cannot run, in the resolver's terms. Which screen (or picker)
 * clears it, and what the button says, is the banner's business — this module
 * mirrors the backend resolver and must not know the UI's routes.
 */
export type BlockedCause =
  /** No binding at all: the user has never chosen. */
  | "unset"
  /** The saved binding names an engine this build does not register. */
  | "unavailable"
  /** A local engine whose model is not downloaded. */
  | "model"
  /** A remote provider with no API key stored. */
  | "credentials";

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
  cause: BlockedCause;
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

export type SlotStatusReport = {
  statuses: SlotStatus[];
  /**
   * Non-null when a lookup failed. Every diagnosis is suppressed in that case:
   * a transient IPC failure must surface as itself, never as a confident and
   * wrong "needs an API key".
   */
  error: string | null;
};

/**
 * Collects the first failure across the lookups instead of letting each one
 * silently degrade into a wrong answer.
 */
function createGuard() {
  let message: string | null = null;
  return {
    async run<T>(work: Promise<T>, fallback: T): Promise<T> {
      try {
        return await work;
      } catch (e) {
        message ??= toMessage(e);
        return fallback;
      }
    },
    failure: () => message,
  };
}

type Guard = ReturnType<typeof createGuard>;

type Env = {
  providers: Provider[];
  keyByRef: Map<string, boolean>;
  engineIds: Record<Capability, Set<string>>;
  installed: Set<string>;
  modelNames: Map<string, string>;
  /**
   * The per-feature fallback model the backend uses when a binding carries no
   * model of its own (dictation.rs / tts.rs). Keyed by capability; null where
   * no such fallback exists.
   */
  activeModels: Record<Capability, string | null>;
};

const emptyEngineIds = (): Record<Capability, Set<string>> => ({
  llm: new Set<string>(),
  stt: new Set<string>(),
  tts: new Set<string>(),
});

async function loadEnv(capabilities: Set<Capability>, guard: Guard): Promise<Env> {
  const providers = await guard.run(listProviders(), [] as Provider[]);
  const keyByRef = await guard.run(
    loadKeyStates(providers),
    new Map<string, boolean>(),
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
      const engines = await guard.run(lister(), []);
      engineIds[capability] = new Set(engines.map((e) => e.id));
    }),
  );

  const needsStt = capabilities.has("stt");
  const needsTts = capabilities.has("tts");
  // Every local engine of a needed capability: each one owns its catalog, and
  // the two halves are guarded apart so one failing list cannot blank the rest.
  const [catalogs, dictationSettings, ttsSettings] = await Promise.all([
    Promise.all(
      catalogEngines()
        .filter((spec) => capabilities.has(spec.capability))
        .map(async ({ catalog }) => {
          const [models, installed] = await Promise.all([
            guard.run(catalog.list(), [] as LocalModel[]),
            guard.run(catalog.listInstalled(), [] as string[]),
          ]);
          return { models, installed };
        }),
    ),
    needsStt ? guard.run(getDictationSettings(), null) : null,
    needsTts ? guard.run(getTtsSettings(), null) : null,
  ]);

  const modelNames = new Map<string, string>();
  catalogs.forEach(({ models }) => models.forEach((m) => modelNames.set(m.id, m.display_name)));

  return {
    providers,
    keyByRef,
    engineIds,
    installed: new Set(catalogs.flatMap((c) => c.installed)),
    modelNames,
    activeModels: {
      llm: null,
      stt: dictationSettings?.active_model ?? null,
      tts: ttsSettings?.active_model ?? null,
    },
  };
}

/**
 * Whether this slot inherits the global active_model when its binding carries
 * no model — true only for the features that own those settings, matching
 * dictation.rs and tts.rs. Meetings passes its binding model straight through,
 * so it must not be diagnosed against dictation's fallback.
 */
function usesActiveModelFallback(spec: SlotSpec): boolean {
  if (spec.capability === "stt") return spec.feature === "dictation";
  if (spec.capability === "tts") return spec.feature === "tts";
  return false;
}

function blockedReason(
  spec: SlotSpec,
  effective: Binding | null,
  env: Env,
): BlockedReason | null {
  if (!effective) {
    return {
      message: "Nothing is set up for this yet.",
      cause: "unset",
    };
  }

  if (!env.engineIds[spec.capability].has(effective.engine_id)) {
    return {
      message: "The saved choice isn't available in this version.",
      cause: "unavailable",
    };
  }

  if (needsDownload(effective.engine_id)) {
    // The backend uses the binding's model, or the feature's saved fallback.
    const model =
      effective.model ??
      (usesActiveModelFallback(spec) ? env.activeModels[spec.capability] : null);
    if (model && !env.installed.has(model)) {
      const name = env.modelNames.get(model) ?? model;
      return {
        message: `${name} isn't downloaded yet.`,
        cause: "model",
      };
    }
  }

  const ref = credentialRefFor(effective);
  if (ref && !env.keyByRef.get(ref)) {
    const name = env.providers.find((p) => p.provider_ref === ref)?.name ?? ref;
    return {
      message: `${name} needs an API key.`,
      cause: "credentials",
    };
  }

  return null;
}

/**
 * Mirrors the backend resolver (override > capability default > single-engine
 * auto-pick) and reports, in plain language, whatever blocks the feature.
 *
 * A diagnosis is only trustworthy if every lookup it rests on succeeded, so a
 * failed lookup suppresses all of them and is reported as an error instead.
 */
export async function loadSlotStatuses(specs: SlotSpec[]): Promise<SlotStatusReport> {
  const guard = createGuard();
  const capabilities = new Set(specs.map((s) => s.capability));
  const env = await loadEnv(capabilities, guard);

  const defaults = new Map<Capability, Binding | null>();
  await Promise.all(
    [...capabilities].map(async (capability) => {
      defaults.set(
        capability,
        await guard.run(getBinding("default", capability), null),
      );
    }),
  );

  const statuses = await Promise.all(
    specs.map(async (spec) => {
      const override = await guard.run(getBinding(spec.feature, spec.slot), null);
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

  const error = guard.failure();
  return {
    statuses: error ? statuses.map((s) => ({ ...s, blocked: null })) : statuses,
    error,
  };
}
