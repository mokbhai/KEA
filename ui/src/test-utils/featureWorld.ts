import type { Binding } from "../api";

/**
 * The invoke handlers every feature page needs: providers, engine lists,
 * model catalogs, bindings and hotkeys. Pages add their own on top.
 *
 *   onInvoke(featureHandlers({ engines: { stt: ["whisper"] } , ...}))
 */

type InvokeArgs = Record<string, unknown> | undefined;
type Handlers = Record<string, (args?: InvokeArgs) => unknown>;

export const WHISPER_CATALOG = [
  {
    id: "whisper-base",
    display_name: "Whisper Base",
    language: "Multilingual",
    url: "",
    size_bytes: 148 * 1024 * 1024,
    sha256: "",
  },
];

export type WorldOptions = {
  /** Bindings keyed "feature/slot"; anything else resolves to null. */
  bindings?: Record<string, Binding>;
  /** Whether cloud providers have a saved key. */
  hasKey?: boolean;
  /** Registered engine ids per capability. Two or more prevents auto-binding. */
  engines?: { llm?: string[]; stt?: string[]; tts?: string[] };
  installedWhisper?: string[];
  installedOnnx?: string[];
  /** Page-specific handlers, merged last. */
  extra?: Handlers;
};

export function featureHandlers({
  bindings = {},
  hasKey = true,
  engines = {},
  installedWhisper = ["whisper-base"],
  installedOnnx = [],
  extra = {},
}: WorldOptions = {}): Handlers {
  const infos = (ids: string[] = []) => ids.map((id) => ({ id, models: [] }));

  return {
    list_providers: () => [
      { provider_ref: "openai", name: "OpenAI", built_in: true },
      { provider_ref: "local-llm", name: "Local server", built_in: true },
    ],
    has_credential: () => hasKey,
    list_llm_engines: () => infos(engines.llm),
    list_stt_engines: () => infos(engines.stt),
    list_tts_engines: () => infos(engines.tts),
    list_whisper_models: () => WHISPER_CATALOG,
    list_installed_whisper_models: () => installedWhisper,
    list_onnx_models: () => [],
    list_installed_onnx_models: () => installedOnnx,
    get_binding: (args) => bindings[`${args?.feature}/${args?.slot}`] ?? null,
    set_binding: () => undefined,
    delete_binding: () => undefined,
    get_effective_hotkey: () => ({
      accelerator: "CommandOrControl+Shift+K",
      source: "default",
    }),
    get_hotkey_registration_status: () => [],
    get_dictation_settings: () => ({ post_process: false, active_model: null }),
    set_dictation_settings: () => undefined,
    get_tts_settings: () => ({ active_voice: null, active_model: null }),
    set_tts_settings: () => undefined,
    ...extra,
  };
}

/** A cloud binding that resolves cleanly when a key is saved. */
export const openAiBinding = (engine: string, model: string | null = null): Binding => ({
  engine_id: engine,
  model,
  provider_ref: "openai",
});
