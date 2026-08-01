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
  | "ask_kea";

export type RewritePreset = {
  id: string;
  name: string;
  instruction: string;
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
];

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

export const triggerRewrite = (
  mode: RewriteMode,
  preset_id?: string | null,
  custom_instruction?: string | null,
) =>
  invoke<string>("trigger_rewrite", {
    mode,
    presetId: preset_id ?? null,
    customInstruction: custom_instruction ?? null,
  });

/**
 * Rewrites `text` with the Rewrite feature's own AI binding and returns the
 * result. Nothing is captured from, or pasted into, the frontmost app — this
 * backs the "Try it" card, unlike {@link triggerRewrite}.
 */
export const previewRewrite = (
  text: string,
  mode: RewriteMode,
  preset_id?: string | null,
  custom_instruction?: string | null,
) =>
  invoke<string>("preview_rewrite", {
    text,
    mode,
    presetId: preset_id ?? null,
    customInstruction: custom_instruction ?? null,
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

export type DictationState = "idle" | "listening" | "processing";

export type DictationSettings = {
  post_process: boolean;
  active_model: string | null;
};

export type WhisperModel = {
  id: string;
  display_name: string;
  language: string;
  url: string;
  size_bytes: number;
  sha256: string;
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

export const setDictationSttBinding = (
  engine: string,
  model?: string | null,
  provider_ref?: string | null,
) =>
  invoke<void>("set_dictation_stt_binding", {
    engine,
    model: model ?? null,
    providerRef: provider_ref ?? null,
  });

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

export type Meeting = {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  status: string;
  capture_mode: string;
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

export type ActionRow = {
  id: number;
  feature_id: string;
  command: string;
  engine_id: string;
  status: string;
};

export type ActionDetail = {
  id: number;
  feature_id: string;
  command: string;
  engine_id: string;
  model: string | null;
  provider_ref: string | null;
  status: string;
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

export type Message = {
  id: number;
  conversation_id: number;
  role: string;
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
  active_voice: string | null;
  active_model: string | null;
};

export type OnnxModelKind = "Parakeet" | "TtsVits";

export type OnnxModel = {
  id: string;
  display_name: string;
  language: string;
  url: string;
  size_bytes: number;
  sha256: string;
  kind: OnnxModelKind;
};

export type OnnxModelKindParam = "parakeet" | "tts";

export const listTtsEngines = () => invoke<EngineInfo[]>("list_tts_engines");

export const setTtsBinding = (
  engine: string,
  model?: string | null,
  provider_ref?: string | null,
) =>
  invoke<void>("set_tts_binding", {
    engine,
    model: model ?? null,
    providerRef: provider_ref ?? null,
  });

export const getTtsSettings = () => invoke<TtsSettings>("get_tts_settings");

export const setTtsSettings = (settings: TtsSettings) =>
  invoke<void>("set_tts_settings", { settings });

export const runReadAloud = () => invoke<void>("run_read_aloud");

export const triggerTts = () => invoke<void>("trigger_tts");

export const listOnnxModels = (kind: OnnxModelKindParam) =>
  invoke<OnnxModel[]>("list_onnx_models", { kind });

export const listInstalledOnnxModels = (kind: OnnxModelKindParam) =>
  invoke<string[]>("list_installed_onnx_models", { kind });

export const downloadOnnxModel = (kind: OnnxModelKindParam, model_id: string) =>
  invoke<void>("download_onnx_model", { kind, modelId: model_id });

export type ModelKindParam = "whisper" | OnnxModelKindParam;

export const deleteModel = (kind: ModelKindParam, model_id: string) =>
  invoke<void>("delete_model", { kind, modelId: model_id });

export const previewVoice = (
  engine: string,
  model?: string | null,
  voice?: string | null,
) =>
  invoke<void>("preview_voice", {
    engine,
    model: model ?? null,
    voice: voice ?? null,
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
