import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ProviderConfig = { base_url: string; default_model: string };

export type EngineInfo = { id: string; models: string[] };

export type Binding = {
  engine_id: string;
  model: string | null;
  provider_ref: string | null;
};

export type RewriteMode =
  | "improve"
  | "fix_grammar"
  | "professional"
  | "concise"
  | "friendly"
  | "audio_refinement"
  | "ask_kea"
  | "translate";

export type RewritePreset = {
  id: string;
  name: string;
  instruction: string;
};

export type VocabularyEntry = {
  id: string;
  term: string;
  /** Comma-separated misrecognitions to rewrite to `term`; null when none. */
  sounds_like: string | null;
  enabled: boolean;
  created_at: string;
};

export type RewriteEventPayload = { message: string };

export const REWRITE_MODES: { value: RewriteMode; label: string }[] = [
  { value: "improve", label: "Improve" },
  { value: "fix_grammar", label: "Fix grammar" },
  { value: "professional", label: "Professional" },
  { value: "concise", label: "Concise" },
  { value: "friendly", label: "Friendly" },
  { value: "audio_refinement", label: "Audio refinement" },
  { value: "ask_kea", label: "Ask KEA" },
  { value: "translate", label: "Translate" },
];

/**
 * The extra value a mode's prompt template needs, mirroring
 * `RewriteMode::parameter` in `kea_core::rewrite::mode`. Modes absent here take
 * no parameter.
 *
 * Both parameters travel in one argument (`customInstruction` at the Tauri
 * boundary); the backend routes it to the right template variable from this
 * same descriptor, so the UI never has to know which variable that is.
 */
export const MODE_PARAMETER: Partial<
  Record<RewriteMode, "instruction" | "target_language">
> = {
  ask_kea: "instruction",
  translate: "target_language",
};

export type TranslationTarget = { tag: string; label: string };

/**
 * The languages the translate picker offers, mirroring `TRANSLATION_TARGETS` in
 * `kea_core::rewrite::language` — which stays the source of truth, since it is
 * what renders the prompt and what accepts tags this list omits.
 */
export const TRANSLATION_TARGETS: TranslationTarget[] = [
  { tag: "en-US", label: "American English" },
  { tag: "ar", label: "Arabic" },
  { tag: "bn", label: "Bengali" },
  { tag: "pt-BR", label: "Brazilian Portuguese" },
  { tag: "en-GB", label: "British English" },
  { tag: "bg", label: "Bulgarian" },
  { tag: "fr-CA", label: "Canadian French" },
  { tag: "cs", label: "Czech" },
  { tag: "da", label: "Danish" },
  { tag: "nl", label: "Dutch" },
  { tag: "en", label: "English" },
  { tag: "tl", label: "Filipino" },
  { tag: "fi", label: "Finnish" },
  { tag: "fr", label: "French" },
  { tag: "de", label: "German" },
  { tag: "el", label: "Greek" },
  { tag: "he", label: "Hebrew" },
  { tag: "hi", label: "Hindi" },
  { tag: "hu", label: "Hungarian" },
  { tag: "is", label: "Icelandic" },
  { tag: "id", label: "Indonesian" },
  { tag: "it", label: "Italian" },
  { tag: "ja", label: "Japanese" },
  { tag: "ko", label: "Korean" },
  { tag: "es-419", label: "Latin American Spanish" },
  { tag: "ms", label: "Malay" },
  { tag: "nb", label: "Norwegian Bokmal" },
  { tag: "fa", label: "Persian" },
  { tag: "pl", label: "Polish" },
  { tag: "pt", label: "Portuguese" },
  { tag: "ro", label: "Romanian" },
  { tag: "ru", label: "Russian" },
  { tag: "zh-Hans", label: "Simplified Chinese" },
  { tag: "sk", label: "Slovak" },
  { tag: "es", label: "Spanish" },
  { tag: "sv", label: "Swedish" },
  { tag: "ta", label: "Tamil" },
  { tag: "th", label: "Thai" },
  { tag: "zh-Hant", label: "Traditional Chinese" },
  { tag: "tr", label: "Turkish" },
  { tag: "uk", label: "Ukrainian" },
  { tag: "ur", label: "Urdu" },
  { tag: "vi", label: "Vietnamese" },
];

/** The listed tag matching `locale` exactly or by its primary subtag. */
export function matchTranslationTarget(locale: string): string | null {
  const wanted = locale.toLowerCase();
  const exact = TRANSLATION_TARGETS.find((t) => t.tag.toLowerCase() === wanted);
  if (exact) return exact.tag;
  const primary = wanted.split("-")[0];
  return TRANSLATION_TARGETS.find((t) => t.tag.toLowerCase() === primary)?.tag ?? null;
}

/**
 * The target to start from: the language this Mac is set to, since translating
 * into your own language is what an unconfigured translate shortcut is for.
 */
export function systemTranslationTarget(): string {
  return matchTranslationTarget(navigator.language || "en") ?? "en";
}

/** The picker label for a tag, falling back to the tag for anything unlisted. */
export function translationTargetLabel(tag: string): string {
  return TRANSLATION_TARGETS.find((t) => t.tag === tag)?.label ?? tag;
}

/** The hotkey command id that translates into `tag`. */
export const translateCommand = (tag: string) => `translate.${tag}`;

export const listLlmEngines = () => invoke<EngineInfo[]>("list_llm_engines");

export const listFeatures = () => invoke<string[]>("list_features");

export const getSetting = (key: string) =>
  invoke<string | null>("get_setting", { key });

export const setSetting = (key: string, value: string) =>
  invoke<void>("set_setting", { key, value });

export const getBinding = (feature: string, slot: string) =>
  invoke<Binding | null>("get_binding", { feature, slot });

export const setBinding = (
  feature: string,
  slot: string,
  engine: string,
  model?: string | null,
  provider_ref?: string | null,
) =>
  invoke<void>("set_binding", {
    feature,
    slot,
    engine,
    model: model ?? null,
    providerRef: provider_ref ?? null,
  });

export const getProviderConfig = (provider_ref: string) =>
  invoke<ProviderConfig | null>("get_provider_config", { providerRef: provider_ref });

export const setProviderConfig = (provider_ref: string, config: ProviderConfig) =>
  invoke<void>("set_provider_config", { providerRef: provider_ref, config });

export const setCredential = (provider_ref: string, secret: string) =>
  invoke<void>("set_credential", { providerRef: provider_ref, secret });

export const deleteCredential = (provider_ref: string) =>
  invoke<void>("delete_credential", { providerRef: provider_ref });

export const deleteBinding = (feature: string, slot: string) =>
  invoke<void>("delete_binding", { feature, slot });

export const hasCredential = (provider_ref: string) =>
  invoke<boolean>("has_credential", { providerRef: provider_ref });

export type ProviderTestResult = { ok: boolean; message: string };

export const testProvider = (provider_ref: string) =>
  invoke<ProviderTestResult>("test_provider", { providerRef: provider_ref });

export type Provider = {
  provider_ref: string;
  name: string;
  built_in: boolean;
};

export const listProviders = () => invoke<Provider[]>("list_providers");

export const addCustomProvider = (provider_ref: string, name: string) =>
  invoke<void>("add_custom_provider", { providerRef: provider_ref, name });

export const updateCustomProvider = (provider_ref: string, name: string) =>
  invoke<void>("update_custom_provider", { providerRef: provider_ref, name });

export const removeCustomProvider = (provider_ref: string) =>
  invoke<void>("remove_custom_provider", { providerRef: provider_ref });

export const listPresets = () => invoke<RewritePreset[]>("list_presets");

export const upsertPreset = (preset: RewritePreset) =>
  invoke<void>("upsert_preset", { preset });

export const deletePreset = (id: string) => invoke<void>("delete_preset", { id });

export const listVocabulary = () => invoke<VocabularyEntry[]>("list_vocabulary");

export const upsertVocabularyEntry = (entry: VocabularyEntry) =>
  invoke<void>("upsert_vocabulary_entry", { entry });

export const deleteVocabularyEntry = (id: string) =>
  invoke<void>("delete_vocabulary_entry", { id });

/**
 * Runs the transcript replacement pass over `text` with the enabled entries —
 * the same code path dictation uses, so the settings page can show what the
 * vocabulary would actually do rather than reimplementing the rules in TS.
 */
export const previewVocabulary = (text: string) =>
  invoke<string>("preview_vocabulary", { text });

/**
 * One "when I am typing into X, do Y" rule, mirroring
 * `kea_core::app_context::AppProfile`.
 *
 * Every override is nullable and `null` means **inherit the global setting**,
 * not "off". `post_process` in particular is a tri-state: collapsing it to a
 * checkbox would turn AI clean-up off for every app that never opted in.
 */
export type AppProfile = {
  id: string;
  name: string;
  enabled: boolean;
  /** Higher wins between two rules that match equally specifically. */
  priority: number;
  /** Exact bundle id, matched case-insensitively; null matches any app. */
  match_bundle_id: string | null;
  /** `*` glob over host[/path], e.g. "*.slack.com/*"; null matches any page. */
  match_url_glob: string | null;
  /** A {@link RewriteMode} tag, or null to inherit. */
  rewrite_mode: string | null;
  preset_id: string | null;
  llm_engine_id: string | null;
  llm_model: string | null;
  llm_provider_ref: string | null;
  /** true forces clean-up on, false forces it off, null inherits. */
  post_process: boolean | null;
  /** {@link INSERTION_MODES} value, or null to inherit. */
  insertion_mode: string | null;
  created_at: string;
};

/**
 * Where a profile puts the finished text, mirroring
 * `kea_core::app_context::InsertionMode::as_str`.
 */
export const INSERTION_MODES: { value: string; label: string }[] = [
  { value: "ax", label: "Type it into the app (Accessibility)" },
  { value: "paste", label: "Paste it (⌘V)" },
];

/** What KEA could see about the app in front when it was asked. */
export type AppContext = {
  bundle_id: string | null;
  app_name: string | null;
  window_title: string | null;
  url: string | null;
};

/** Settings keys mirroring `CaptureOpts::SETTING_*`. Both default to off. */
export const CAPTURE_WINDOW_TITLE_SETTING = "profiles.capture_window_title";
export const CAPTURE_URL_SETTING = "profiles.capture_url";

export const listAppProfiles = () => invoke<AppProfile[]>("list_app_profiles");

export const upsertAppProfile = (profile: AppProfile) =>
  invoke<void>("upsert_app_profile", { profile });

export const deleteAppProfile = (id: string) =>
  invoke<void>("delete_app_profile", { id });

/**
 * Identifies the app the user wants a rule for.
 *
 * Takes a few seconds on purpose: clicking a button in KEA makes KEA the
 * frontmost app, so the backend waits for the user to switch back before it
 * looks. `null` means it could not identify anything other than KEA itself.
 */
export const captureAppContext = () => invoke<AppContext | null>("capture_app_context");

export const getPromptOverride = (mode: RewriteMode) =>
  invoke<string | null>("get_prompt_override", { mode });

export const setPromptOverride = (mode: RewriteMode, prompt: string) =>
  invoke<void>("set_prompt_override", { mode, prompt });

export const getHotkey = (feature: string, command: string) =>
  invoke<string | null>("get_hotkey", { feature, command });

export type EffectiveHotkey = {
  accelerator: string;
  source: "custom" | "default";
};

export const getEffectiveHotkey = (feature: string, command: string) =>
  invoke<EffectiveHotkey | null>("get_effective_hotkey", { feature, command });

export type HotkeyRegStatus = {
  feature: string;
  command: string;
  ok: boolean;
  error: string | null;
};

export const getHotkeyRegistrationStatus = () =>
  invoke<HotkeyRegStatus[]>("get_hotkey_registration_status");

export const setHotkey = (feature: string, command: string, accelerator: string) =>
  invoke<void>("set_hotkey", { feature, command, accelerator });

/**
 * Rewrites the current selection in the frontmost app and replaces it there.
 *
 * `parameter` is whatever the mode needs (see {@link MODE_PARAMETER}): Ask
 * KEA's instruction, Translate's BCP-47 target tag, null for the rest.
 */
export const triggerRewrite = (
  mode: RewriteMode,
  preset_id?: string | null,
  parameter?: string | null,
) =>
  invoke<string>("trigger_rewrite", {
    mode,
    presetId: preset_id ?? null,
    customInstruction: parameter ?? null,
  });

/**
 * Rewrites `text` with the Rewrite feature's own AI binding and returns the
 * result. Nothing is captured from, or pasted into, the frontmost app — this
 * backs the "Try it" card, unlike {@link triggerRewrite}. `parameter` carries
 * the mode's value, as it does there.
 */
export const previewRewrite = (
  text: string,
  mode: RewriteMode,
  preset_id?: string | null,
  parameter?: string | null,
) =>
  invoke<string>("preview_rewrite", {
    text,
    mode,
    presetId: preset_id ?? null,
    customInstruction: parameter ?? null,
  });

export const runDemo = (prompt: string) => invoke<string>("run_demo", { prompt });

export const onRewriteProgress = (
  handler: (message: string) => void,
): Promise<UnlistenFn> =>
  listen<RewriteEventPayload>("rewrite:progress", (event) =>
    handler(event.payload.message),
  );

export const onRewriteError = (handler: (message: string) => void): Promise<UnlistenFn> =>
  listen<RewriteEventPayload>("rewrite:error", (event) =>
    handler(event.payload.message),
  );

/**
 * `locked` is a recording started by a double tap of ⌥⇧: still listening, but
 * with no key holding it open, which is why it is a state of its own rather
 * than a flag on `listening`.
 */
export type DictationState = "idle" | "listening" | "locked" | "processing";

export type DictationSettings = {
  post_process: boolean;
  active_model: string | null;
  /** Hold ⌥⇧ to record, release to transcribe and insert. */
  hold_to_talk: boolean;
  /** Which microphone to record from, by name, or null for the system default. */
  input_device: string | null;
  /** Open the mic as ⌥⇧ is pressed so the first word is not clipped. */
  preroll: boolean;
  /** BCP-47 tag, or null to let the model detect it. Whisper only. */
  language: string | null;
};

/**
 * The languages the dictation picker offers, on top of auto-detect.
 *
 * Plain ISO-639-1 subtags, not full locales: the tag is handed to whisper.cpp
 * as-is (crates/infer/src/whisper.rs `set_language`), and it knows "pt", not
 * "pt-BR". A short list rather than whisper's ninety-nine — the model detects
 * the rest on its own, which is what the default is for.
 */
export const DICTATION_LANGUAGES: { tag: string; label: string }[] = [
  { tag: "en", label: "English" },
  { tag: "ar", label: "Arabic" },
  { tag: "zh", label: "Chinese" },
  { tag: "nl", label: "Dutch" },
  { tag: "fr", label: "French" },
  { tag: "de", label: "German" },
  { tag: "hi", label: "Hindi" },
  { tag: "it", label: "Italian" },
  { tag: "ja", label: "Japanese" },
  { tag: "ko", label: "Korean" },
  { tag: "pl", label: "Polish" },
  { tag: "pt", label: "Portuguese" },
  { tag: "ru", label: "Russian" },
  { tag: "es", label: "Spanish" },
  { tag: "tr", label: "Turkish" },
  { tag: "uk", label: "Ukrainian" },
];

export type InputDevice = {
  /** The device name, which is the only handle the audio layer has. */
  id: string;
  name: string;
  is_default: boolean;
};

/** The saved microphone was gone, so recording fell back to the default. */
export type DeviceFallback = {
  requested: string;
  using: string | null;
};

export type WhisperModel = {
  id: string;
  display_name: string;
  language: string;
  url: string;
  size_bytes: number;
  sha256: string;
  /**
   * Still resolvable, no longer offered. The catalog keeps retired models so
   * an already-downloaded one can still be removed; the picker hides them
   * unless they are installed.
   */
  deprecated: boolean;
};

export type ModelDownloadProgress = {
  model_id: string;
  bytes_received: number;
  bytes_total: number;
};

export const listSttEngines = () => invoke<EngineInfo[]>("list_stt_engines");

export const listWhisperModels = () => invoke<WhisperModel[]>("list_whisper_models");

export const listInstalledWhisperModels = () =>
  invoke<string[]>("list_installed_whisper_models");

export const downloadWhisperModel = (model_id: string) =>
  invoke<void>("download_whisper_model", { modelId: model_id });

export const getDictationSettings = () =>
  invoke<DictationSettings>("get_dictation_settings");

export const setDictationSettings = (settings: DictationSettings) =>
  invoke<void>("set_dictation_settings", { settings });

export const listInputDevices = () => invoke<InputDevice[]>("list_input_devices");

/**
 * Open the microphone purely to show its level. Takes the same capture gate as
 * a recording, so it is refused while one is running — and stops itself on a
 * timer, on window blur, and when a hotkey starts a real recording.
 */
export const startInputPreview = () => invoke<void>("start_input_preview");

export const stopInputPreview = () => invoke<void>("stop_input_preview");

export const startDictation = () => invoke<void>("start_dictation");

export const stopDictation = () => invoke<string>("stop_dictation");

export const onDictationState = (
  handler: (state: DictationState) => void,
): Promise<UnlistenFn> =>
  listen<{ state: DictationState }>("dictation:state", (event) =>
    handler(event.payload.state),
  );

export const onDictationLevel = (handler: (level: number) => void): Promise<UnlistenFn> =>
  listen<{ level: number }>("dictation:level", (event) => handler(event.payload.level));

export const onDictationPreview = (
  handler: (active: boolean) => void,
): Promise<UnlistenFn> =>
  listen<{ active: boolean }>("dictation:preview", (event) =>
    handler(event.payload.active),
  );

export const onDeviceFallback = (
  handler: (fallback: DeviceFallback) => void,
): Promise<UnlistenFn> =>
  listen<DeviceFallback>("dictation:device_fallback", (event) => handler(event.payload));

export const onDictationError = (handler: (message: string) => void): Promise<UnlistenFn> =>
  listen<RewriteEventPayload>("dictation:error", (event) =>
    handler(event.payload.message),
  );

export const onModelDownloadProgress = (
  handler: (progress: ModelDownloadProgress) => void,
): Promise<UnlistenFn> =>
  listen<ModelDownloadProgress>("model:download:progress", (event) =>
    handler(event.payload),
  );

export type ModelDownloadCompleteEvent = { model_id: string };

export const onModelDownloadComplete = (
  handler: (modelId: string) => void,
): Promise<UnlistenFn> =>
  listen<ModelDownloadCompleteEvent>("model:download:complete", (event) =>
    handler(event.payload.model_id),
  );

export type ModelDownloadErrorEvent = { model_id: string; message: string };

export const onModelDownloadError = (
  handler: (modelId: string, message: string) => void,
): Promise<UnlistenFn> =>
  listen<ModelDownloadErrorEvent>("model:download:error", (event) =>
    handler(event.payload.model_id, event.payload.message),
  );

export type MeetingState = "idle" | "recording" | "processing";

export type SystemAudioCapability =
  | "unavailable"
  | "screen_capture_kit"
  | "loopback_device"
  | "mic_only";

export type PermStatus = "Unknown" | "Granted" | "Denied";

/** Mirrors `kea_core::store::meetings::MeetingStatus`. */
export type MeetingStatus = "recording" | "completed" | "error";

/** Mirrors `kea_core::store::meetings::CaptureMode`. */
export type CaptureMode = "mic_only" | "mic_and_system";

export type Meeting = {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  status: MeetingStatus;
  capture_mode: CaptureMode;
  stt_engine_id: string | null;
  llm_engine_id: string | null;
  error: string | null;
};

export type MeetingSegment = {
  id: number;
  meeting_id: string;
  sequence: number;
  start_offset_ms: number;
  end_offset_ms: number;
  text: string;
};

export type MeetingSegmentEvent = {
  meeting_id: string;
  sequence: number;
  start_offset_ms: number;
  end_offset_ms: number;
  text: string;
};

export type MeetingNotes = {
  meeting_id: string;
  summary: string;
  decisions: string;
  action_items: string;
  follow_ups: string;
  open_questions: string;
  prompt_version: string;
  engine_id: string | null;
  model: string | null;
};

export type MeetingDetail = {
  meeting: Meeting;
  segments: MeetingSegment[];
  notes: MeetingNotes | null;
};

export type MeetingSettings = {
  segment_duration_secs: number;
  prefer_system_audio: boolean;
};

export const getMeetingSettings = () =>
  invoke<MeetingSettings>("get_meeting_settings");

export const setMeetingSettings = (settings: MeetingSettings) =>
  invoke<void>("set_meeting_settings", { settings });

export const getSystemAudioCapability = () =>
  invoke<SystemAudioCapability>("get_system_audio_capability");

export const getPermissionStatus = (
  kind: "microphone" | "screen_recording" | "accessibility",
) => invoke<PermStatus>("get_permission_status", { kind });

export const requestPermission = (
  kind: "microphone" | "screen_recording" | "accessibility",
) => invoke<PermStatus>("request_permission", { kind });

export const openAccessibilitySettings = () =>
  invoke<void>("open_accessibility_settings");

export type PermissionStatusItem = {
  kind: string;
  status: PermStatus;
};

export const getAllPermissionStatuses = () =>
  invoke<PermissionStatusItem[]>("get_all_permission_statuses");

export const showNotification = (title: string, body: string) =>
  invoke<void>("show_notification", { title, body });

export const listMeetings = (limit?: number) =>
  invoke<Meeting[]>("list_meetings", { limit });

export const getMeeting = (id: string) =>
  invoke<MeetingDetail>("get_meeting", { id });

export const deleteMeeting = (id: string) =>
  invoke<void>("delete_meeting", { id });

export const startMeeting = () => invoke<string>("start_meeting");

export const stopMeeting = () => invoke<MeetingDetail>("stop_meeting");

export type MeetingStatePayload = {
  state: MeetingState;
  active_meeting_id: string | null;
};

export const getMeetingState = () =>
  invoke<MeetingStatePayload>("get_meeting_state");

export const getDictationStateApi = () =>
  invoke<DictationState>("get_dictation_state");

export const onMeetingState = (
  handler: (state: MeetingState) => void,
): Promise<UnlistenFn> =>
  listen<{ state: MeetingState }>("meeting:state", (event) =>
    handler(event.payload.state),
  );

export const onMeetingSegment = (
  handler: (seg: MeetingSegmentEvent) => void,
): Promise<UnlistenFn> =>
  listen<MeetingSegmentEvent>("meeting:segment", (event) =>
    handler(event.payload),
  );

export const onMeetingLevel = (handler: (level: number) => void): Promise<UnlistenFn> =>
  listen<{ level: number }>("meeting:level", (event) =>
    handler(event.payload.level),
  );

export const onMeetingError = (handler: (message: string) => void): Promise<UnlistenFn> =>
  listen<RewriteEventPayload>("meeting:error", (event) =>
    handler(event.payload.message),
  );

/** Mirrors `kea_core::store::actions::ActionStatus`. */
export type ActionStatus = "started" | "ok" | "error";

export type ActionRow = {
  id: number;
  feature_id: string;
  command: string;
  engine_id: string;
  status: ActionStatus;
};

export type ActionDetail = {
  id: number;
  feature_id: string;
  command: string;
  engine_id: string;
  model: string | null;
  provider_ref: string | null;
  status: ActionStatus;
  error: string | null;
  started_at: string;
  finished_at: string | null;
};

export type ConversationSummary = {
  id: number;
  action_id: number | null;
  feature_id: string;
  engine_id: string;
  model: string | null;
  provider_ref: string | null;
  created_at: string;
};

/** Mirrors `kea_core::store::conversations::MessageRole`. */
export type MessageRole = "user" | "assistant";

export type Message = {
  id: number;
  conversation_id: number;
  role: MessageRole;
  content: string;
  token_count: number | null;
  created_at: string;
};

export const listActions = (query?: string, limit?: number) =>
  invoke<ActionRow[]>("list_actions", {
    query: query ?? null,
    limit: limit ?? null,
  });

export const getAction = (id: number) =>
  invoke<ActionDetail | null>("get_action", { id });

export const listConversations = (limit?: number) =>
  invoke<ConversationSummary[]>("list_conversations", { limit: limit ?? null });

export const listMessages = (conversationId: number) =>
  invoke<Message[]>("list_messages", { conversationId });

export const deleteConversation = (id: number) =>
  invoke<void>("delete_conversation", { id });

export const tailLogs = (max_bytes?: number) =>
  invoke<string>("tail_logs", { maxBytes: max_bytes ?? null });

export const openLogFolder = () => invoke<void>("open_log_folder");

export type TtsState = "idle" | "reading";

export type TtsSettings = {
  /** The chosen speaker, by name — never by index. */
  active_voice: string | null;
  active_model: string | null;
  /** Rate multiplier; 1 is the voice's natural pace. */
  speed: number;
};

/** Slowest and fastest the read-aloud rate may be set to. */
export const MIN_TTS_SPEED = 0.5;
export const MAX_TTS_SPEED = 2;

/**
 * Which sherpa model config a bundle loads through. Not a vendor list: it is
 * the set of bundle shapes the app knows how to fill.
 */
export type OnnxModelKind = "Parakeet" | "TtsVits" | "TtsKokoro" | "TtsKitten";

export type OnnxModel = {
  id: string;
  display_name: string;
  language: string;
  url: string;
  size_bytes: number;
  sha256: string;
  kind: OnnxModelKind;
  /** See {@link WhisperModel.deprecated}. */
  deprecated: boolean;
};

/** One speaker inside a multi-speaker local bundle (Kokoro, Kitten). */
export type OnnxVoice = {
  /** The index the bundle addresses this speaker by. */
  sid: number;
  name: string;
  language: string;
};

/** One voice the operating system itself offers. */
export type SystemVoice = {
  /** The identifier to ask for this voice again; names are not unique. */
  id: string;
  name: string;
  language: string;
  /** "default", "enhanced" or "premium". */
  quality: string;
};

export type OnnxModelKindParam = "parakeet" | "tts";

export const listTtsEngines = () => invoke<EngineInfo[]>("list_tts_engines");

export const getTtsSettings = () => invoke<TtsSettings>("get_tts_settings");

export const setTtsSettings = (settings: TtsSettings) =>
  invoke<void>("set_tts_settings", { settings });

export const runReadAloud = () => invoke<void>("run_read_aloud");

export const triggerTts = () => invoke<void>("trigger_tts");

export const listOnnxModels = (kind: OnnxModelKindParam) =>
  invoke<OnnxModel[]>("list_onnx_models", { kind });

/**
 * The speakers inside one local voice bundle. Empty for a single-speaker
 * model, which is the normal case for the Piper voices.
 */
export const listOnnxVoices = (model_id: string) =>
  invoke<OnnxVoice[]>("list_onnx_voices", { modelId: model_id });

/** Empty where the platform has no system synthesizer. */
export const listSystemVoices = () => invoke<SystemVoice[]>("list_system_voices");

export const listInstalledOnnxModels = (kind: OnnxModelKindParam) =>
  invoke<string[]>("list_installed_onnx_models", { kind });

export const downloadOnnxModel = (kind: OnnxModelKindParam, model_id: string) =>
  invoke<void>("download_onnx_model", { kind, modelId: model_id });

export type ModelKindParam = "whisper" | OnnxModelKindParam;

/**
 * Stops an in-flight download and discards its partial file. Safe to call for
 * a download the backend has already forgotten — it still reports the terminal
 * event, which is how a UI stuck on a dead transfer gets unstuck.
 */
export const cancelModelDownload = (kind: ModelKindParam, model_id: string) =>
  invoke<void>("cancel_model_download", { kind, modelId: model_id });

export const deleteModel = (kind: ModelKindParam, model_id: string) =>
  invoke<void>("delete_model", { kind, modelId: model_id });

/**
 * `providerRef` names whose key and base URL the preview should use. The cloud
 * TTS engine is registered once against the built-in "openai" ref, so a
 * preview of a voice bound to a user-added provider that omitted this would
 * read the wrong key and fail while the real read-aloud run worked.
 */
export const previewVoice = (
  engine: string,
  model?: string | null,
  voice?: string | null,
  providerRef?: string | null,
) =>
  invoke<void>("preview_voice", {
    engine,
    model: model ?? null,
    voice: voice ?? null,
    providerRef: providerRef ?? null,
  });

export const onTtsState = (handler: (state: TtsState) => void): Promise<UnlistenFn> =>
  listen<{ state: TtsState }>("tts:state", (event) => handler(event.payload.state));

export const onTtsError = (handler: (message: string) => void): Promise<UnlistenFn> =>
  listen<RewriteEventPayload>("tts:error", (event) => handler(event.payload.message));

export const getAutostart = () => invoke<boolean>("get_autostart");

export const setAutostart = (enabled: boolean) =>
  invoke<void>("set_autostart", { enabled });

export type UpdateStatus = {
  status: string;
  version: string | null;
  error: string | null;
};

export const checkUpdate = () => invoke<UpdateStatus>("check_update");
