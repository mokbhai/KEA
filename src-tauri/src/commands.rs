use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kea_core::dictation::{DictationSettings, DictationSettingsRepo};
use kea_core::log::{current_log_path, tail_log_file};
use kea_core::meetings::{MeetingSettings, MeetingSettingsRepo};
use kea_core::resolve::{Resolution, DEFAULT_FEATURE_ID};
use kea_core::rewrite::{
    PresetRepo, PromptOverrideRepo, ProviderConfig, ProviderConfigRepo, RewriteInput,
    RewriteMode, RewritePreset,
};
use kea_core::store::actions::{ActionDetail, ActionRepo, ActionRow};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::conversations::{ConversationRepo, ConversationSummary, Message};
use kea_core::store::hotkeys::{HotkeyBindingRepo, HotkeyBindingRow};
use kea_core::store::meetings::{Meeting, MeetingDetail};
use kea_core::store::settings::SettingsRepo;
use kea_core::tts::{TtsSettings, TtsSettingsRepo};
use kea_engines::{EngineRegistry, TtsOpts};
use kea_features::demo::run_ping;
use kea_features::{
    drain_and_stop_meeting, run_dictation_with_storage, run_meeting_poll_segment,
    run_meeting_start, run_meeting_stop, run_tts_synthesize, ActiveMeeting, ContentStorageOpts,
    MeetingRunContext,
};
use kea_features::run_rewrite_with_storage;
use kea_core::resolve::SlotResolver;
use kea_infer::{DownloadTransport, InferError, ModelDownloader, ModelRegistry, ModelStorage, OnnxModelEntry};
use kea_platform::{
    new_text_io, parse_accelerator, AudioIo, AudioIoError, DictationState, HotkeyBinding,
    Hotkeys, MeetingState, PermKind, PermStatus, PcmFrame, SystemAudioCapability,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

use crate::events::{
    emit_dictation_level, emit_dictation_state, emit_meeting_error,
    emit_meeting_level, emit_meeting_segment, emit_meeting_state,
    emit_model_download_complete, emit_model_download_error, emit_model_download_progress,
    emit_tts_state, MeetingSegmentPayload,
};
use crate::AppState;

pub const REWRITE_ACTION_ID: &str = "rewrite:rewrite_selection";
pub const REWRITE_FEATURE_ID: &str = "rewrite";
pub const REWRITE_COMMAND_ID: &str = "rewrite_selection";

pub const DICTATION_ACTION_ID: &str = "dictation:push_to_talk";
pub const DICTATION_FEATURE_ID: &str = "dictation";
pub const DICTATION_COMMAND_ID: &str = "push_to_talk";

pub const TTS_ACTION_ID: &str = "tts:read_selection";
pub const TTS_FEATURE_ID: &str = "tts";
pub const TTS_COMMAND_ID: &str = "read_selection";

pub const MEETINGS_ACTION_ID: &str = "meetings:toggle_meeting";
pub const MEETINGS_FEATURE_ID: &str = "meetings";
pub const MEETINGS_COMMAND_ID: &str = "toggle_meeting";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EffectiveHotkey {
    pub accelerator: String,
    /// "custom" when the DB row exists, "default" when falling back to the compiled-in
    /// accelerator.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HotkeyRegStatus {
    pub feature: String,
    pub command: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Drop-guard that clears a per-feature busy flag. Each spawned handler holds
/// one until it completes (Drop), even if the handler task panics.
pub struct BusyGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Try to acquire a per-feature busy flag. Returns `Some(BusyGuard)` when the
/// flag was `false` (idle) and marks it busy, or `None` when a handler is already
/// in-flight (the press should be dropped).
pub fn try_acquire_busy(flag: &Arc<AtomicBool>) -> Option<BusyGuard> {
    if flag
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
    {
        Some(BusyGuard {
            flag: Arc::clone(flag),
        })
    } else {
        None
    }
}

/// Pure helper: resolve the effective hotkey for a known (feature, command) pair.
/// Returns the DB row if present, otherwise the compiled default.
pub fn effective_hotkey(feature: &str, command: &str, db_accel: Option<String>) -> Option<EffectiveHotkey> {
    match db_accel {
        Some(accel) => Some(EffectiveHotkey {
            accelerator: accel,
            source: "custom".into(),
        }),
        None => compiled_default_accelerator(feature, command).map(|a| EffectiveHotkey {
            accelerator: a.to_string(),
            source: "default".into(),
        }),
    }
}

/// Record a hotkey-registration outcome in the shared status map.
pub fn record_hotkey_reg_status(
    statuses: &mut HashMap<String, HotkeyRegStatus>,
    feature: &str,
    command: &str,
    result: Result<(), String>,
) {
    let key = format!("{feature}:{command}");
    let (ok, error) = match &result {
        Ok(()) => (true, None),
        Err(e) => {
            tracing::warn!(feature = %feature, command = %command, %e, "hotkey registration failed");
            (false, Some(e.clone()))
        }
    };
    statuses.insert(key, HotkeyRegStatus {
        feature: feature.to_string(),
        command: command.to_string(),
        ok,
        error,
    });
}

/// Pure helper: clears a single registration-status entry when re-registration succeeds.
pub fn clear_hotkey_reg_status(statuses: &mut HashMap<String, HotkeyRegStatus>, feature: &str, command: &str) {
    let key = format!("{feature}:{command}");
    statuses.remove(&key);
}

/// Poll-time state for an active meeting recording session.
pub struct ActiveMeetingSession {
    pub session: ActiveMeeting,
    pub sequence: i32,
    pub elapsed_ms: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EngineInfoDto {
    pub id: String,
    pub models: Vec<String>,
}

#[derive(Serialize)]
pub struct BindingDto {
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

/// Pure helper kept separate from the #[tauri::command] wrapper so it is unit-testable.
pub fn engine_ids(reg: &EngineRegistry) -> Vec<String> {
    reg.list_llm_ids()
}

/// Maps registered LLM engines to UI-friendly metadata.
pub fn engine_infos(reg: &EngineRegistry) -> Vec<EngineInfoDto> {
    reg.list_llm_ids()
        .into_iter()
        .filter_map(|id| {
            reg.llm(&id).map(|engine| EngineInfoDto {
                id: id.clone(),
                models: engine.capabilities().models,
            })
        })
        .collect()
}

/// Maps registered STT engines to UI-friendly metadata.
pub fn stt_engine_infos(reg: &EngineRegistry) -> Vec<EngineInfoDto> {
    reg.list_stt_ids()
        .into_iter()
        .filter_map(|id| {
            reg.stt(&id).map(|engine| EngineInfoDto {
                id: id.clone(),
                models: engine.capabilities().models,
            })
        })
        .collect()
}

/// Maps registered TTS engines to UI-friendly metadata.
pub fn tts_engine_infos(reg: &EngineRegistry) -> Vec<EngineInfoDto> {
    reg.list_tts_ids()
        .into_iter()
        .filter_map(|id| {
            reg.tts(&id).map(|engine| EngineInfoDto {
                id: id.clone(),
                models: engine.capabilities().models,
            })
        })
        .collect()
}

/// Maps slot resolution outcomes to user-facing error strings (pure, unit-testable).
pub fn resolution_error(res: Resolution) -> Option<String> {
    match res {
        Resolution::Bound(_) => None,
        Resolution::NeedsChoice(candidates) => Some(format!(
            "multiple llm engines available; bind a slot or choose one of: {candidates:?}"
        )),
        Resolution::Unresolvable => Some("no llm engine available".into()),
    }
}

/// Maps [`SystemAudioCapability`] to the UI-facing snake_case string.
pub fn system_audio_capability_dto(cap: SystemAudioCapability) -> String {
    match cap {
        SystemAudioCapability::Unavailable => "unavailable".into(),
        SystemAudioCapability::ScreenCaptureKit => "screen_capture_kit".into(),
        SystemAudioCapability::LoopbackDevice => "loopback_device".into(),
        SystemAudioCapability::MicOnly => "mic_only".into(),
    }
}

fn parse_perm_kind(kind: &str) -> Result<PermKind, String> {
    match kind {
        "microphone" => Ok(PermKind::Microphone),
        "screen_recording" => Ok(PermKind::ScreenRecording),
        "accessibility" => Err(
            "accessibility is queried via get_all_permission_statuses or accessibility_status()"
                .into(),
        ),
        _ => Err(format!("unknown permission kind: {kind}")),
    }
}

/// Best-effort Accessibility trust probe (macOS AX APIs).
pub fn accessibility_status() -> PermStatus {
    #[cfg(target_os = "macos")]
    {
        if kea_platform::textio::macos_ax::is_ax_trusted() {
            PermStatus::Granted
        } else {
            PermStatus::Denied
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermStatus::Unknown
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionStatusItem {
    pub kind: String,
    pub status: PermStatus,
}

pub fn all_permission_statuses(permissions: &dyn kea_platform::Permissions) -> Vec<PermissionStatusItem> {
    vec![
        PermissionStatusItem {
            kind: "microphone".into(),
            status: permissions.status(PermKind::Microphone),
        },
        PermissionStatusItem {
            kind: "screen_recording".into(),
            status: permissions.status(PermKind::ScreenRecording),
        },
        PermissionStatusItem {
            kind: "accessibility".into(),
            status: accessibility_status(),
        },
    ]
}

pub fn onnx_catalog_for_kind(kind: &str) -> Result<Vec<OnnxModelEntry>, String> {
    match kind {
        "parakeet" => Ok(ModelRegistry::parakeet_catalog()),
        "tts" => Ok(ModelRegistry::tts_catalog()),
        _ => Err(format!("unknown onnx model kind: {kind} (expected parakeet or tts)")),
    }
}

pub fn installed_onnx_model_ids(storage: &ModelStorage, catalog: &[OnnxModelEntry]) -> Vec<String> {
    catalog
        .iter()
        .filter(|entry| storage.is_onnx_installed(&entry.id))
        .map(|entry| entry.id.clone())
        .collect()
}

/// Providers that ship with the app and can never be removed.
pub const BUILT_IN_PROVIDERS: [(&str, &str); 2] =
    [("openai", "OpenAI"), ("local-llm", "Local server")];

/// Settings key holding the JSON list of user-added providers.
pub const CUSTOM_PROVIDERS_KEY: &str = "providers.custom";

/// A user-added provider entry as stored in settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomProvider {
    pub provider_ref: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderEntry {
    pub provider_ref: String,
    pub name: String,
    pub built_in: bool,
}

/// Merges built-in providers with the stored custom list (pure, unit-testable).
/// A custom entry shadowed by a built-in ref is dropped.
pub fn provider_entries(custom: &[CustomProvider]) -> Vec<ProviderEntry> {
    let mut entries: Vec<ProviderEntry> = BUILT_IN_PROVIDERS
        .iter()
        .map(|(provider_ref, name)| ProviderEntry {
            provider_ref: (*provider_ref).into(),
            name: (*name).into(),
            built_in: true,
        })
        .collect();
    for provider in custom {
        if entries.iter().any(|e| e.provider_ref == provider.provider_ref) {
            continue;
        }
        entries.push(ProviderEntry {
            provider_ref: provider.provider_ref.clone(),
            name: provider.name.clone(),
            built_in: false,
        });
    }
    entries
}

/// Validates a to-be-added custom provider (pure, unit-testable).
pub fn validate_new_provider(
    provider_ref: &str,
    name: &str,
    existing: &[CustomProvider],
) -> Result<(), String> {
    if provider_ref.trim().is_empty() {
        return Err("Provider id can't be empty".into());
    }
    if name.trim().is_empty() {
        return Err("Provider name can't be empty".into());
    }
    if BUILT_IN_PROVIDERS.iter().any(|(r, _)| *r == provider_ref) {
        return Err(format!("\"{provider_ref}\" is a built-in provider"));
    }
    if existing.iter().any(|p| p.provider_ref == provider_ref) {
        return Err(format!("A provider \"{provider_ref}\" already exists"));
    }
    Ok(())
}

async fn load_custom_providers(settings: &SettingsRepo) -> Result<Vec<CustomProvider>, String> {
    settings
        .get(CUSTOM_PROVIDERS_KEY)
        .await
        .map(|list| list.unwrap_or_default())
        .map_err(|e| e.to_string())
}

async fn save_custom_providers(
    settings: &SettingsRepo,
    providers: &Vec<CustomProvider>,
) -> Result<(), String> {
    settings
        .set(CUSTOM_PROVIDERS_KEY, providers)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderTestResult {
    pub ok: bool,
    pub message: String,
}

/// Maps a `GET {base_url}/models` HTTP status to a human-readable test
/// outcome (pure, unit-testable).
pub fn provider_test_result_for_status(status: u16) -> ProviderTestResult {
    match status {
        200..=299 => ProviderTestResult {
            ok: true,
            message: "Connected".into(),
        },
        401 | 403 => ProviderTestResult {
            ok: false,
            message: "Invalid key".into(),
        },
        s => ProviderTestResult {
            ok: false,
            message: format!("Server responded with status {s}"),
        },
    }
}

/// Existence-only credential probe: the secret never crosses this boundary.
pub async fn credential_exists(
    store: &dyn kea_core::secrets::CredentialStore,
    provider_ref: &str,
) -> Result<bool, String> {
    store
        .get(provider_ref)
        .await
        .map(|secret| secret.is_some())
        .map_err(|e| e.to_string())
}

/// Drops the capability-default binding for `slot` when it references
/// `model_id`. Returns whether a binding was deleted.
pub async fn clear_default_binding_for_model(
    bindings: &BindingRepo,
    slot: &str,
    model_id: &str,
) -> Result<bool, String> {
    if let Some(binding) = bindings
        .get(DEFAULT_FEATURE_ID, slot)
        .await
        .map_err(|e| e.to_string())?
    {
        if binding.model.as_deref() == Some(model_id) {
            bindings
                .delete(DEFAULT_FEATURE_ID, slot)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn default_rewrite_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+R"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+R"
    }
}

pub async fn resolve_rewrite_accelerator(config_pool: &SqlitePool) -> String {
    let repo = HotkeyBindingRepo::new(config_pool.clone());
    match repo
        .get(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID)
        .await
    {
        Ok(Some(row)) => row.accelerator,
        _ => default_rewrite_accelerator().to_string(),
    }
}

pub async fn default_rewrite_input(config_pool: &SqlitePool) -> RewriteInput {
    let settings = SettingsRepo::new(config_pool.clone());
    let mode = settings
        .get::<String>("rewrite.active_mode")
        .await
        .ok()
        .flatten()
        .and_then(|s| RewriteMode::from_str(&s))
        .unwrap_or(RewriteMode::Improve);
    let preset_id = PresetRepo::new(config_pool.clone())
        .active_id()
        .await
        .ok()
        .flatten();
    let custom_instruction = if matches!(mode, RewriteMode::AskKea) {
        settings
            .get::<String>("rewrite.custom_instruction")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    RewriteInput {
        source_text: String::new(),
        mode,
        preset_id,
        custom_instruction,
    }
}

async fn store_conversations_enabled(config_pool: &SqlitePool) -> bool {
    let settings = SettingsRepo::new(config_pool.clone());
    // The generic set_setting command stores values as JSON strings (the UI
    // writes "true"/"false"), while other callers may store a JSON bool —
    // accept both so a user's opt-out is never silently ignored.
    match settings
        .get::<serde_json::Value>("history.store_conversations")
        .await
    {
        Ok(Some(serde_json::Value::Bool(v))) => v,
        Ok(Some(serde_json::Value::String(s))) => s != "false",
        Ok(Some(other)) => {
            tracing::warn!(value = %other, "unexpected history.store_conversations value, defaulting to true");
            true
        }
        Ok(None) => true, // default on
        Err(e) => {
            tracing::warn!(%e, "failed to read history.store_conversations, defaulting to true");
            true
        }
    }
}

pub async fn execute_rewrite(state: &AppState, input: RewriteInput) -> Result<String, String> {
    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let presets = PresetRepo::new(state.config_pool.clone());
    let overrides = PromptOverrideRepo::new(state.config_pool.clone());
    let textio = new_text_io();
    let conversations = ConversationRepo::new(state.data_pool.clone());
    let storage = if store_conversations_enabled(&state.config_pool).await {
        ContentStorageOpts::enabled(&conversations)
    } else {
        ContentStorageOpts::default()
    };
    run_rewrite_with_storage(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &overrides,
        textio.as_ref(),
        input,
        storage,
    )
    .await
}

pub fn register_rewrite_hotkey(
    hotkeys: &mut Box<dyn Hotkeys>,
    accelerator: &str,
) -> Result<(), String> {
    hotkeys
        .register(
            HotkeyBinding {
                accelerator: accelerator.to_string(),
            },
            REWRITE_ACTION_ID.into(),
        )
        .map_err(|e| e.to_string())
}

pub fn default_dictation_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+D"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+D"
    }
}

pub async fn resolve_dictation_accelerator(config_pool: &SqlitePool) -> String {
    let repo = HotkeyBindingRepo::new(config_pool.clone());
    match repo
        .get(DICTATION_FEATURE_ID, DICTATION_COMMAND_ID)
        .await
    {
        Ok(Some(row)) => row.accelerator,
        _ => default_dictation_accelerator().to_string(),
    }
}

pub fn register_dictation_hotkey(
    hotkeys: &mut Box<dyn Hotkeys>,
    accelerator: &str,
) -> Result<(), String> {
    hotkeys
        .register(
            HotkeyBinding {
                accelerator: accelerator.to_string(),
            },
            DICTATION_ACTION_ID.into(),
        )
        .map_err(|e| e.to_string())
}

pub fn default_tts_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+T"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+T"
    }
}

pub async fn resolve_tts_accelerator(config_pool: &SqlitePool) -> String {
    let repo = HotkeyBindingRepo::new(config_pool.clone());
    match repo.get(TTS_FEATURE_ID, TTS_COMMAND_ID).await {
        Ok(Some(row)) => row.accelerator,
        _ => default_tts_accelerator().to_string(),
    }
}

pub fn register_tts_hotkey(
    hotkeys: &mut Box<dyn Hotkeys>,
    accelerator: &str,
) -> Result<(), String> {
    hotkeys
        .register(
            HotkeyBinding {
                accelerator: accelerator.to_string(),
            },
            TTS_ACTION_ID.into(),
        )
        .map_err(|e| e.to_string())
}

pub fn default_meeting_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+M"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+M"
    }
}

pub async fn resolve_meeting_accelerator(config_pool: &SqlitePool) -> String {
    let repo = HotkeyBindingRepo::new(config_pool.clone());
    match repo
        .get(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID)
        .await
    {
        Ok(Some(row)) => row.accelerator,
        _ => default_meeting_accelerator().to_string(),
    }
}

pub fn register_meeting_hotkey(
    hotkeys: &mut Box<dyn Hotkeys>,
    accelerator: &str,
) -> Result<(), String> {
    hotkeys
        .register(
            HotkeyBinding {
                accelerator: accelerator.to_string(),
            },
            MEETINGS_ACTION_ID.into(),
        )
        .map_err(|e| e.to_string())
}

pub async fn trigger_tts_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    emit_tts_state(app, "reading");
    // Any early failure between here and the terminal emit must still return
    // the UI to idle, otherwise the global status banner sticks on
    // "Reading selection aloud…".
    let result = trigger_tts_run(state, app).await;
    if result.is_err() {
        emit_tts_state(app, "idle");
    }
    result
}

async fn trigger_tts_run(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    let settings = TtsSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map_err(|e| e.to_string())?;
    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let textio = new_text_io();

    let (action_id, pcm) = run_tts_synthesize(
        &state.engines,
        &bindings,
        &actions,
        textio.as_ref(),
        &settings,
    )
    .await?;

    let play_result = tokio::task::spawn_blocking(move || {
        kea_platform::audio::playback::play_pcm_blocking(&pcm)
    })
    .await
    .map_err(|e| format!("playback failed: {e}"))?;

    match play_result {
        Ok(()) => {
            actions
                .finish(action_id, "ok", None)
                .await
                .map_err(|e| e.to_string())?;
        }
        Err(e) => {
            if let Err(inner) = actions
                .finish(action_id, "error", Some(&e.to_string()))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "tts: failed to finish action as error in DB"
                );
            }
            emit_tts_state(app, "idle");
            return Err(e.to_string());
        }
    }

    emit_tts_state(app, "idle");
    Ok(())
}

/// Toggle push-to-talk: global-hotkey currently delivers press events only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationHotkeyAction {
    Start,
    Stop,
    Ignore,
}

pub fn dictation_hotkey_action(current: DictationState, meeting_active: bool, in_flight: bool) -> DictationHotkeyAction {
    if meeting_active {
        return DictationHotkeyAction::Ignore;
    }
    match current {
        DictationState::Listening => DictationHotkeyAction::Stop,
        DictationState::Idle if in_flight => DictationHotkeyAction::Ignore,
        DictationState::Idle => DictationHotkeyAction::Start,
        DictationState::Processing => DictationHotkeyAction::Ignore,
    }
}

/// Meeting hotkey toggle decision (pure, testable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingHotkeyAction {
    Start,
    Stop,
    Ignore,
}

pub fn meeting_hotkey_action(recording: bool, processing: bool) -> MeetingHotkeyAction {
    // A prior meeting still synthesizing (processing) must not toggle: starting
    // would launch a concurrent meeting the UI can't stop, and there's no
    // active meeting to stop.
    if processing {
        return MeetingHotkeyAction::Ignore;
    }
    if recording {
        MeetingHotkeyAction::Stop
    } else {
        MeetingHotkeyAction::Start
    }
}

/// Replays a pre-captured PCM buffer through [`run_dictation`]'s mic lifecycle.
struct ReplayAudioIo {
    pcm: PcmFrame,
    state: DictationState,
}

impl ReplayAudioIo {
    fn new(pcm: PcmFrame) -> Self {
        Self {
            pcm,
            state: DictationState::Idle,
        }
    }
}

#[async_trait]
impl AudioIo for ReplayAudioIo {
    async fn start_mic(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        self.state = DictationState::Listening;
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }

    async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
        self.state = DictationState::Idle;
        Ok(self.pcm.clone())
    }

    fn current_level(&self) -> f32 {
        0.0
    }

    fn state(&self) -> DictationState {
        self.state
    }
}

struct ReqwestDownloadTransport;

#[async_trait]
impl DownloadTransport for ReqwestDownloadTransport {
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, InferError> {
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|e| InferError::Other(e.to_string()))?;
        if !response.status().is_success() {
            return Err(InferError::Other(format!(
                "download failed with status {}",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| InferError::Other(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}

pub fn new_model_downloader(storage: ModelStorage) -> ModelDownloader {
    ModelDownloader::new(Arc::new(ReqwestDownloadTransport), storage)
}

fn onnx_storage_for<'a>(state: &'a AppState, kind: &str) -> Result<&'a ModelStorage, String> {
    match kind {
        "parakeet" => Ok(&state.parakeet_storage),
        "tts" => Ok(&state.tts_storage),
        _ => Err(format!("unknown onnx model kind: {kind}")),
    }
}

fn open_path_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Open the macOS Accessibility privacy pane in System Settings.
#[tauri::command]
pub fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn stop_level_poll(state: &AppState) {
    if let Ok(mut guard) = state.level_poll_cancel.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(true);
        }
    }
}

fn stop_segment_poll(state: &AppState) {
    if let Ok(mut guard) = state.segment_poll_cancel.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(true);
        }
    }
}

fn spawn_level_poll(state: &Arc<AppState>, app: &AppHandle) {
    stop_level_poll(state);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    if let Ok(mut guard) = state.level_poll_cancel.lock() {
        *guard = Some(cancel_tx);
    }

    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let level = state.audio.lock().await.current_level();
                    emit_dictation_level(&app, level);
                }
            }
        }
    });
}

fn spawn_meeting_level_poll(state: &Arc<AppState>, app: &AppHandle) {
    stop_level_poll(state);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    if let Ok(mut guard) = state.level_poll_cancel.lock() {
        *guard = Some(cancel_tx);
    }

    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let level = state.audio.lock().await.current_level();
                    emit_meeting_level(&app, level);
                }
            }
        }
    });
}

fn spawn_segment_poll(state: &Arc<AppState>, app: &AppHandle, interval_secs: u32) {
    stop_segment_poll(state);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    if let Ok(mut guard) = state.segment_poll_cancel.lock() {
        *guard = Some(cancel_tx);
    }

    let state = state.clone();
    let app = app.clone();
    let poll_interval = Duration::from_secs(interval_secs.max(1) as u64);
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(poll_interval);
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    let poll_state = {
                        let guard = state.active_meeting.lock().expect("active_meeting lock");
                        guard.as_ref().map(|active| {
                            (
                                active.session.meeting_id.clone(),
                                active.sequence,
                                active.elapsed_ms,
                            )
                        })
                    };
                    let Some((meeting_id, mut sequence, mut elapsed_ms)) = poll_state else {
                        break;
                    };

                    let settings = match MeetingSettingsRepo::new(SettingsRepo::new(
                        state.config_pool.clone(),
                    ))
                    .get()
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            emit_meeting_error(&app, &e.to_string());
                            continue;
                        }
                    };

                    let bindings = BindingRepo::new(state.config_pool.clone());
                    let actions = ActionRepo::new(state.data_pool.clone());
                    let meetings = &state.meeting_repo;

                    let poll_result = {
                        let mut audio = state.audio.lock().await;
                        let mut ctx = MeetingRunContext {
                            engines: &state.engines,
                            bindings: &bindings,
                            actions: &actions,
                            meetings,
                            audio: audio.as_mut(),
                            settings: &settings,
                        };
                        run_meeting_poll_segment(
                            &mut ctx,
                            &meeting_id,
                            &mut sequence,
                            &mut elapsed_ms,
                        )
                        .await
                    };

                    if let Ok(mut guard) = state.active_meeting.lock() {
                        if let Some(active) = guard.as_mut() {
                            if active.session.meeting_id == meeting_id {
                                active.sequence = sequence;
                                active.elapsed_ms = elapsed_ms;
                            }
                        }
                    }

                    match poll_result {
                        Ok(Some(ev)) => {
                            emit_meeting_segment(
                                &app,
                                &MeetingSegmentPayload {
                                    meeting_id: ev.meeting_id,
                                    sequence: ev.sequence,
                                    text: ev.text,
                                    start_offset_ms: ev.start_offset_ms,
                                    end_offset_ms: ev.end_offset_ms,
                                },
                            );
                        }
                        Ok(None) => {}
                        Err(e) => emit_meeting_error(&app, &e),
                    }
                }
            }
        }
    });
}

pub async fn start_meeting_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<String, String> {
    // Reject before the audio lock while a prior meeting is still finishing:
    // during synthesis active_meeting is None and audio is Idle, so without
    // this a hotkey press would start a concurrent, UI-unstoppable meeting.
    if state.meeting_processing.load(Ordering::SeqCst) {
        return Err("a meeting is finishing; wait for it to complete".into());
    }

    {
        let guard = state
            .active_meeting
            .lock()
            .map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Err("a meeting is already recording".into());
        }
    }

    {
        let audio = state.audio.lock().await;
        if audio.state() != DictationState::Idle {
            return Err("dictation is active; stop dictation before starting a meeting".into());
        }
        if audio.meeting_state() == MeetingState::Recording {
            return Err("meeting capture is already active".into());
        }
    }

    let settings = MeetingSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map_err(|e| e.to_string())?;

    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let meetings = &state.meeting_repo;

    let session = {
        let mut audio = state.audio.lock().await;
        let mut ctx = MeetingRunContext {
            engines: &state.engines,
            bindings: &bindings,
            actions: &actions,
            meetings,
            audio: audio.as_mut(),
            settings: &settings,
        };
        run_meeting_start(&mut ctx).await?
    };

    let meeting_id = session.meeting_id.clone();
    {
        let mut guard = state
            .active_meeting
            .lock()
            .map_err(|e| e.to_string())?;
        *guard = Some(ActiveMeetingSession {
            session,
            sequence: 0,
            elapsed_ms: 0,
        });
    }

    emit_meeting_state(app, "recording");
    spawn_meeting_level_poll(state, app);
    spawn_segment_poll(state, app, settings.segment_duration_secs);

    Ok(meeting_id)
}

pub async fn stop_meeting_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
) -> Result<MeetingDetail, String> {
    stop_segment_poll(state);
    stop_level_poll(state);

    let session = {
        let mut guard = state
            .active_meeting
            .lock()
            .map_err(|e| e.to_string())?;
        guard.take().ok_or_else(|| "no meeting is recording".to_string())?
    };

    // Mark the post-capture processing window so dictation / a new meeting
    // reject immediately instead of parking on the audio lock and replaying
    // once synthesis finishes. Cleared on every exit via the guard below.
    state.meeting_processing.store(true, Ordering::SeqCst);
    struct ProcessingGuard<'a>(&'a std::sync::atomic::AtomicBool);
    impl Drop for ProcessingGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _processing_guard = ProcessingGuard(&state.meeting_processing);

    emit_meeting_state(app, "processing");

    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let meetings = &state.meeting_repo;

    // Hold the shared audio lock only for the drain + capture release, then
    // release it before the tens-of-seconds STT / LLM synthesis so dictation
    // and new meetings can acquire it promptly.
    let drain_result = {
        let mut audio = state.audio.lock().await;
        drain_and_stop_meeting(audio.as_mut()).await
    };

    let detail = run_meeting_stop(
        &state.engines,
        &bindings,
        &actions,
        meetings,
        &session.session,
        drain_result,
    )
    .await;

    match &detail {
        Ok(_) => emit_meeting_state(app, "idle"),
        Err(e) => {
            emit_meeting_error(app, e);
            emit_meeting_state(app, "idle");
        }
    }

    detail
}

pub async fn start_dictation_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    // Reject before touching the audio lock so a press during meeting
    // synthesis can't park on the lock and start once it's released.
    if state.meeting_processing.load(Ordering::SeqCst) {
        return Err("a meeting is finishing; wait for it to complete".into());
    }

    {
        let in_flight = state
            .dictation_current_run
            .lock()
            .map_err(|e| e.to_string())?
            .is_some();
        if in_flight {
            return Err("dictation is processing a previous run; wait for it to finish".into());
        }
    }

    {
        let mut audio = state.audio.lock().await;
        if audio.state() == DictationState::Listening {
            return Err("dictation is already listening".into());
        }
        if audio.state() == DictationState::Processing {
            return Err("dictation is processing".into());
        }
        if let Err(e) = audio.start_mic().await {
            emit_dictation_state(app, "idle");
            return Err(e.to_string());
        }
    }

    emit_dictation_state(app, "listening");
    spawn_level_poll(state, app);
    Ok(())
}

pub async fn stop_dictation_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<String, String> {
    stop_level_poll(state);

    let pcm = {
        let mut audio = state.audio.lock().await;
        if audio.state() != DictationState::Listening {
            // Re-sync listeners to idle only when nothing is in-flight.
            if audio.state() == DictationState::Idle {
                let in_flight = state
                    .dictation_current_run
                    .lock()
                    .map_err(|e| e.to_string())?
                    .is_some();
                if !in_flight {
                    emit_dictation_state(app, "idle");
                }
            }
            return Err("dictation is not listening".into());
        }
        match audio.stop_mic().await {
            Ok(pcm) => pcm,
            Err(e) => {
                emit_dictation_state(app, "idle");
                return Err(e.to_string());
            }
        }
    };

    // Allocate a run id and mark in-flight before emitting "processing".
    let run_id = state.dictation_run_counter.fetch_add(1, Ordering::SeqCst);
    {
        let mut guard = state
            .dictation_current_run
            .lock()
            .map_err(|e| e.to_string())?;
        *guard = Some(run_id);
    }

    // RAII: clear the in-flight flag and emit idle on every exit — normal
    // return, early error, or a panic inside the processing pipeline — but
    // only if this run is still the current one, so a newer run's "listening"
    // state is never clobbered. Mirrors the meeting path's ProcessingGuard;
    // without it a panic here would wedge dictation as "processing" forever.
    struct RunGuard<'a> {
        flag: &'a std::sync::Mutex<Option<u64>>,
        run_id: u64,
        app: &'a AppHandle,
    }
    impl Drop for RunGuard<'_> {
        fn drop(&mut self) {
            let mut g = self.flag.lock().unwrap_or_else(|p| p.into_inner());
            if *g == Some(self.run_id) {
                *g = None;
                emit_dictation_state(self.app, "idle");
            }
        }
    }
    let _run_guard = RunGuard {
        flag: &state.dictation_current_run,
        run_id,
        app,
    };

    emit_dictation_state(app, "processing");

    let settings = match DictationSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
    {
        Ok(s) => s,
        Err(e) => return Err(e.to_string()),
    };

    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let presets = PresetRepo::new(state.config_pool.clone());
    let overrides = PromptOverrideRepo::new(state.config_pool.clone());
    let textio = new_text_io();
    let mut replay = ReplayAudioIo::new(pcm);
    let conversations = ConversationRepo::new(state.data_pool.clone());
    let storage = if store_conversations_enabled(&state.config_pool).await {
        ContentStorageOpts::enabled(&conversations)
    } else {
        ContentStorageOpts::default()
    };

    let result = run_dictation_with_storage(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &overrides,
        &mut replay,
        textio.as_ref(),
        &settings,
        storage,
    )
    .await;

    // Flag clearing + the terminal "idle" emit are handled by _run_guard's
    // Drop (which also covers panics and the "newer run" case).
    result
}

#[tauri::command]
pub fn list_engines(state: State<'_, Arc<AppState>>) -> Vec<String> {
    engine_ids(&state.engines)
}

#[tauri::command]
pub fn list_llm_engines(state: State<'_, Arc<AppState>>) -> Vec<EngineInfoDto> {
    engine_infos(&state.engines)
}

#[tauri::command]
pub fn list_features(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state.features.list_ids()
}

#[tauri::command]
pub async fn get_setting(
    state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    SettingsRepo::new(state.config_pool.clone())
        .get(&key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_setting(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    SettingsRepo::new(state.config_pool.clone())
        .set(&key, &value)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_binding(
    state: State<'_, Arc<AppState>>,
    feature: String,
    slot: String,
) -> Result<Option<BindingDto>, String> {
    BindingRepo::new(state.config_pool.clone())
        .get(&feature, &slot)
        .await
        .map(|b| {
            b.map(|b| BindingDto {
                engine_id: b.engine_id,
                model: b.model,
                provider_ref: b.provider_ref,
            })
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_binding(
    state: State<'_, Arc<AppState>>,
    feature: String,
    slot: String,
    engine: String,
    model: Option<String>,
    provider_ref: Option<String>,
) -> Result<(), String> {
    // Validate engine id exists in the registry before persisting.
    let known = state.engines.llm(&engine).is_some()
        || state.engines.stt(&engine).is_some()
        || state.engines.tts(&engine).is_some();
    if !known {
        return Err(format!("unknown engine id: {engine}"));
    }
    BindingRepo::new(state.config_pool.clone())
        .set(
            &feature,
            &slot,
            Binding {
                engine_id: engine,
                model,
                provider_ref,
            },
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_binding(
    state: State<'_, Arc<AppState>>,
    feature: String,
    slot: String,
) -> Result<(), String> {
    BindingRepo::new(state.config_pool.clone())
        .delete(&feature, &slot)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_provider_config(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
) -> Result<Option<ProviderConfig>, String> {
    ProviderConfigRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get(&provider_ref)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_provider_config(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    config: ProviderConfig,
) -> Result<(), String> {
    ProviderConfigRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .set(&provider_ref, &config)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_credential(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    secret: String,
) -> Result<(), String> {
    state
        .credentials
        .set(&provider_ref, &secret)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_credential(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
) -> Result<(), String> {
    state
        .credentials
        .delete(&provider_ref)
        .await
        .map_err(|e| e.to_string())
}

/// Whether a key is saved for the provider. Never returns the secret itself.
#[tauri::command]
pub async fn has_credential(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
) -> Result<bool, String> {
    credential_exists(state.credentials.as_ref(), &provider_ref).await
}

/// Probes `GET {base_url}/models` with the saved key (when present) and maps
/// the outcome to a human-readable result.
#[tauri::command]
pub async fn test_provider(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
) -> Result<ProviderTestResult, String> {
    let config = ProviderConfigRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get(&provider_ref)
        .await
        .map_err(|e| e.to_string())?;
    let base_url = config
        .map(|c| c.base_url)
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| "https://api.openai.com/v1".into());
    let api_key = state
        .credentials
        .get(&provider_ref)
        .await
        .map_err(|e| e.to_string())?;

    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    Ok(match request.send().await {
        Ok(response) => provider_test_result_for_status(response.status().as_u16()),
        Err(_) => ProviderTestResult {
            ok: false,
            message: "Server unreachable".into(),
        },
    })
}

#[tauri::command]
pub async fn list_providers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ProviderEntry>, String> {
    let settings = SettingsRepo::new(state.config_pool.clone());
    let custom = load_custom_providers(&settings).await?;
    Ok(provider_entries(&custom))
}

#[tauri::command]
pub async fn add_custom_provider(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    name: String,
) -> Result<(), String> {
    let settings = SettingsRepo::new(state.config_pool.clone());
    let mut custom = load_custom_providers(&settings).await?;
    validate_new_provider(&provider_ref, &name, &custom)?;
    custom.push(CustomProvider { provider_ref, name });
    save_custom_providers(&settings, &custom).await
}

#[tauri::command]
pub async fn update_custom_provider(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    name: String,
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Provider name can't be empty".into());
    }
    let settings = SettingsRepo::new(state.config_pool.clone());
    let mut custom = load_custom_providers(&settings).await?;
    let entry = custom
        .iter_mut()
        .find(|p| p.provider_ref == provider_ref)
        .ok_or_else(|| format!("No custom provider \"{provider_ref}\""))?;
    entry.name = name;
    save_custom_providers(&settings, &custom).await
}

/// Removes a custom provider from the list. Its saved config and key are
/// left untouched, so re-adding the same ref restores them.
#[tauri::command]
pub async fn remove_custom_provider(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
) -> Result<(), String> {
    if BUILT_IN_PROVIDERS.iter().any(|(r, _)| *r == provider_ref) {
        return Err("Built-in providers can't be removed".into());
    }
    let settings = SettingsRepo::new(state.config_pool.clone());
    let mut custom = load_custom_providers(&settings).await?;
    let before = custom.len();
    custom.retain(|p| p.provider_ref != provider_ref);
    if custom.len() == before {
        return Err(format!("No custom provider \"{provider_ref}\""));
    }
    save_custom_providers(&settings, &custom).await
}

#[tauri::command]
pub async fn list_presets(state: State<'_, Arc<AppState>>) -> Result<Vec<RewritePreset>, String> {
    PresetRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upsert_preset(
    state: State<'_, Arc<AppState>>,
    preset: RewritePreset,
) -> Result<(), String> {
    PresetRepo::new(state.config_pool.clone())
        .upsert(&preset)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_preset(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    PresetRepo::new(state.config_pool.clone())
        .delete(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_prompt_override(
    state: State<'_, Arc<AppState>>,
    mode: RewriteMode,
) -> Result<Option<String>, String> {
    PromptOverrideRepo::new(state.config_pool.clone())
        .get(mode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_prompt_override(
    state: State<'_, Arc<AppState>>,
    mode: RewriteMode,
    prompt: String,
) -> Result<(), String> {
    PromptOverrideRepo::new(state.config_pool.clone())
        .set(mode, &prompt)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_hotkey(
    state: State<'_, Arc<AppState>>,
    feature: String,
    command: String,
) -> Result<Option<String>, String> {
    HotkeyBindingRepo::new(state.config_pool.clone())
        .get(&feature, &command)
        .await
        .map(|row| row.map(|r| r.accelerator))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_effective_hotkey(
    state: State<'_, Arc<AppState>>,
    feature: String,
    command: String,
) -> Result<Option<EffectiveHotkey>, String> {
    let db_accel = HotkeyBindingRepo::new(state.config_pool.clone())
        .get(&feature, &command)
        .await
        .map_err(|e| e.to_string())?
        .map(|r| r.accelerator);
    Ok(effective_hotkey(&feature, &command, db_accel))
}

#[tauri::command]
pub fn get_hotkey_registration_status(
    state: State<'_, Arc<AppState>>,
) -> Vec<HotkeyRegStatus> {
    let guard = state
        .hotkey_reg_status
        .lock()
        .expect("hotkey_reg_status lock");
    guard.values().cloned().collect()
}

/// Validate an accelerator string via [`parse_accelerator`], returning a
/// user-presentable error on failure. Pure helper kept separate from the
/// Tauri command so it is unit-testable.
pub fn validate_accelerator(accelerator: &str) -> Result<(), String> {
    parse_accelerator(accelerator)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Compare accelerators by parsed identity, not raw string: alias spellings
/// ("Cmd+Shift+R" vs "CommandOrControl+Shift+R") map to the same HotKey id on
/// macOS, so treating them as different would unregister the key we just
/// (re-)registered. Falls back to string equality when either side fails to parse.
fn same_accelerator(a: &str, b: &str) -> bool {
    match (parse_accelerator(a), parse_accelerator(b)) {
        (Ok(a), Ok(b)) => a.id() == b.id(),
        _ => a == b,
    }
}

/// Compiled-in default accelerator for the known global-hotkey pairs, if any.
fn compiled_default_accelerator(feature: &str, command: &str) -> Option<&'static str> {
    match (feature, command) {
        (REWRITE_FEATURE_ID, REWRITE_COMMAND_ID) => Some(default_rewrite_accelerator()),
        (DICTATION_FEATURE_ID, DICTATION_COMMAND_ID) => Some(default_dictation_accelerator()),
        (TTS_FEATURE_ID, TTS_COMMAND_ID) => Some(default_tts_accelerator()),
        (MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID) => Some(default_meeting_accelerator()),
        _ => None,
    }
}

/// Check for cross-feature accelerator collisions: return `Some("feature/command")`
/// if `accelerator` is already bound (via a DB row or compiled default) by a
/// different known global-hotkey pair.
fn check_hotkey_collision(
    feature: &str,
    command: &str,
    accelerator: &str,
    bindings: &[HotkeyBindingRow],
) -> Option<String> {
    for &(other_feature, other_command) in &[
        (REWRITE_FEATURE_ID, REWRITE_COMMAND_ID),
        (DICTATION_FEATURE_ID, DICTATION_COMMAND_ID),
        (TTS_FEATURE_ID, TTS_COMMAND_ID),
        (MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID),
    ] {
        if other_feature == feature && other_command == command {
            continue;
        }
        let other_effective = bindings
            .iter()
            .find(|b| b.feature_id == other_feature && b.command == other_command)
            .map(|b| b.accelerator.as_str())
            .or_else(|| compiled_default_accelerator(other_feature, other_command));

        if let Some(other_accel) = other_effective {
            if same_accelerator(accelerator, other_accel) {
                return Some(format!("{other_feature}/{other_command}"));
            }
        }
    }
    None
}

/// Cleanup for the previously-live accelerator after a rebind. Decided before
/// taking the hotkeys lock; applied only once the new binding registered.
#[derive(Debug, PartialEq, Eq)]
enum OldHotkeyAction {
    None,
    Unregister(String),
    /// Re-register the accelerator to the (feature, command) that still owns it.
    Reassign {
        accelerator: String,
        feature_id: String,
        command: String,
    },
}

/// Decide what to do with the old accelerator, given the persisted bindings.
///
/// MacHotkeys keys registrations by accelerator string alone, and `register`
/// steals ownership: the live entry for `old_accel` may fire THIS feature's
/// action even when another row still maps to it in the DB. So when a known
/// global-hotkey pair still owns the old accelerator we re-register it to that
/// pair's action (restoring correct ownership) instead of skipping; otherwise
/// nothing legitimate is listening and we unregister it.
fn old_hotkey_action(
    feature: &str,
    command: &str,
    old_accel: Option<String>,
    new_accel: &str,
    bindings: &[HotkeyBindingRow],
) -> OldHotkeyAction {
    let Some(old_accel) = old_accel else {
        return OldHotkeyAction::None;
    };
    if same_accelerator(&old_accel, new_accel) {
        return OldHotkeyAction::None;
    }
    let other_owner = bindings.iter().find(|b| {
        same_accelerator(&b.accelerator, &old_accel)
            && (b.feature_id != feature || b.command != command)
            && compiled_default_accelerator(&b.feature_id, &b.command).is_some()
    });
    match other_owner {
        Some(owner) => OldHotkeyAction::Reassign {
            accelerator: old_accel,
            feature_id: owner.feature_id.clone(),
            command: owner.command.clone(),
        },
        None => OldHotkeyAction::Unregister(old_accel),
    }
}

#[tauri::command]
pub async fn set_hotkey(
    state: State<'_, Arc<AppState>>,
    feature: String,
    command: String,
    accelerator: String,
) -> Result<(), String> {
    validate_accelerator(&accelerator)?;

    let binding_repo = HotkeyBindingRepo::new(state.config_pool.clone());

    let old_row = binding_repo
        .get(&feature, &command)
        .await
        .map_err(|e| e.to_string())?;

    // Startup registers each feature's compiled-in default accelerator when no
    // DB row exists, so on a first save the default is the live binding to
    // clean up even though old_row is None.
    let old_accel = match &old_row {
        Some(old) => Some(old.accelerator.clone()),
        None => compiled_default_accelerator(&feature, &command).map(str::to_string),
    };

    // --- collision detection: prevent two features from sharing an accelerator ---
    let all_bindings = binding_repo
        .list()
        .await
        .map_err(|e| e.to_string())?;

    if let Some(owner) =
        check_hotkey_collision(&feature, &command, &accelerator, &all_bindings)
    {
        return Err(format!(
            "accelerator '{accelerator}' is already used by {owner}"
        ));
    }

    // Decide the cleanup BEFORE taking the hotkeys lock and before the DB is
    // updated, so the list reflects the current (pre-change) ownership.
    let old_action = if old_accel
        .as_deref()
        .is_some_and(|old| !same_accelerator(old, &accelerator))
    {
        old_hotkey_action(&feature, &command, old_accel, &accelerator, &all_bindings)
    } else {
        OldHotkeyAction::None
    };

    // Register the new binding first; only when that succeeds do we persist
    // the DB row, so a failed registration never leaves stale data. The lock
    // is scoped: the std Mutex guard must not live across the persist await.
    let registered = {
        let mut hotkeys = state.hotkeys.lock().map_err(|e| e.to_string())?;
        match (feature.as_str(), command.as_str()) {
            (REWRITE_FEATURE_ID, REWRITE_COMMAND_ID) => {
                register_rewrite_hotkey(&mut hotkeys, &accelerator)?;
                true
            }
            (DICTATION_FEATURE_ID, DICTATION_COMMAND_ID) => {
                register_dictation_hotkey(&mut hotkeys, &accelerator)?;
                true
            }
            (TTS_FEATURE_ID, TTS_COMMAND_ID) => {
                register_tts_hotkey(&mut hotkeys, &accelerator)?;
                true
            }
            (MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID) => {
                register_meeting_hotkey(&mut hotkeys, &accelerator)?;
                true
            }
            _ => {
                tracing::debug!(feature = %feature, command = %command,
                    "hotkey persisted but is not a global-hotkey feature");
                false
            }
        }
    };

    if let Err(err) = binding_repo.set(&feature, &command, &accelerator).await {
        if registered {
            // Roll back: unregister the hotkey we just registered since we
            // can't persist its accelerator. This is best-effort.
            let mut hotkeys = state.hotkeys.lock().map_err(|e| e.to_string())?;
            let _ = hotkeys.unregister(&HotkeyBinding {
                accelerator: accelerator.clone(),
            });
        }
        return Err(err.to_string());
    }

    if registered {
        let mut hotkeys = state.hotkeys.lock().map_err(|e| e.to_string())?;

        match old_action {
            OldHotkeyAction::None => {}
            OldHotkeyAction::Unregister(old_accel) => {
                if let Err(err) = hotkeys.unregister(&HotkeyBinding {
                    accelerator: old_accel.clone(),
                }) {
                    tracing::warn!(
                        feature = %feature, command = %command,
                        old_accel = %old_accel, %err,
                        "unregister old hotkey failed (non-fatal)"
                    );
                }
            }
            OldHotkeyAction::Reassign {
                accelerator: old_accel,
                feature_id: owner_feature,
                command: owner_command,
            } => {
                // MacHotkeys::register replaces the existing by_accel/by_id
                // entry, so this hands the old accelerator back to its owner.
                let result = match (owner_feature.as_str(), owner_command.as_str()) {
                    (REWRITE_FEATURE_ID, REWRITE_COMMAND_ID) => {
                        register_rewrite_hotkey(&mut hotkeys, &old_accel)
                    }
                    (DICTATION_FEATURE_ID, DICTATION_COMMAND_ID) => {
                        register_dictation_hotkey(&mut hotkeys, &old_accel)
                    }
                    (TTS_FEATURE_ID, TTS_COMMAND_ID) => {
                        register_tts_hotkey(&mut hotkeys, &old_accel)
                    }
                    (MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID) => {
                        register_meeting_hotkey(&mut hotkeys, &old_accel)
                    }
                    // Unreachable: Reassign only targets known pairs.
                    _ => Ok(()),
                };
                if let Err(err) = result {
                    tracing::warn!(
                        feature = %owner_feature, command = %owner_command,
                        old_accel = %old_accel, %err,
                        "re-register old hotkey for its owner failed (non-fatal)"
                    );
                }
            }
        }
    }

    // Clear any startup registration failure record for this action.
    {
        let mut statuses = state
            .hotkey_reg_status
            .lock()
            .map_err(|e| e.to_string())?;
        clear_hotkey_reg_status(&mut statuses, &feature, &command);
    }

    Ok(())
}

#[tauri::command]
pub async fn trigger_rewrite(
    state: State<'_, Arc<AppState>>,
    mode: RewriteMode,
    preset_id: Option<String>,
    custom_instruction: Option<String>,
) -> Result<String, String> {
    execute_rewrite(
        &state,
        RewriteInput {
            source_text: String::new(),
            mode,
            preset_id,
            custom_instruction,
        },
    )
    .await
}

#[tauri::command]
pub async fn run_demo(state: State<'_, Arc<AppState>>, prompt: String) -> Result<String, String> {
    let bindings = BindingRepo::new(state.config_pool.clone());
    let resolver = SlotResolver::new(&state.engines, &bindings);
    let engine_id = match resolver
        .resolve_llm("demo", "llm")
        .await
        .map_err(|e| e.to_string())?
    {
        Resolution::Bound(id) => id,
        other => return Err(resolution_error(other).unwrap_or_else(|| "resolution failed".into())),
    };
    run_ping(&state.engines, &engine_id, &prompt).await
}

#[tauri::command]
pub fn list_stt_engines(state: State<'_, Arc<AppState>>) -> Vec<EngineInfoDto> {
    stt_engine_infos(&state.engines)
}

#[tauri::command]
pub fn list_whisper_models() -> Vec<kea_infer::WhisperModelEntry> {
    ModelRegistry::whisper_catalog()
}

#[tauri::command]
pub fn list_installed_whisper_models(state: State<'_, Arc<AppState>>) -> Vec<String> {
    state.model_storage.installed_models()
}

#[tauri::command]
pub async fn download_whisper_model(
    state: State<'_, Arc<AppState>>,
    model_id: String,
    app: AppHandle,
) -> Result<(), String> {
    let key = format!("whisper:{model_id}");
    {
        let mut guard = state.active_downloads.lock().map_err(|e| e.to_string())?;
        if !guard.insert(key.clone()) {
            return Err(format!("download of '{model_id}' already in progress"));
        }
    }
    let downloader = new_model_downloader(ModelStorage::new(state.model_storage.root.clone()));
    let app_handle = app.clone();
    let state_for_cleanup = state.inner().clone();
    let mid = model_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = downloader
            .download_whisper(&model_id, |progress| {
                emit_model_download_progress(&app_handle, &progress);
            })
            .await;
        {
            let mut guard = state_for_cleanup.active_downloads.lock().unwrap();
            guard.remove(&key);
        }
        match result {
            Ok(_) => emit_model_download_complete(&app_handle, &mid),
            Err(error) => {
                emit_model_download_error(&app_handle, &mid, &error.to_string());
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn get_dictation_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<DictationSettings, String> {
    DictationSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_dictation_settings(
    state: State<'_, Arc<AppState>>,
    settings: DictationSettings,
) -> Result<(), String> {
    DictationSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .set(&settings)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_dictation_stt_binding(
    state: State<'_, Arc<AppState>>,
    engine: String,
    model: Option<String>,
    provider_ref: Option<String>,
) -> Result<(), String> {
    BindingRepo::new(state.config_pool.clone())
        .set(
            DICTATION_FEATURE_ID,
            "stt",
            Binding {
                engine_id: engine,
                model,
                provider_ref,
            },
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_dictation(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    start_dictation_inner(&state, &app).await
}

#[tauri::command]
pub async fn stop_dictation(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<String, String> {
    stop_dictation_inner(&state, &app).await
}

#[tauri::command]
pub async fn get_meeting_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<MeetingSettings, String> {
    MeetingSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_meeting_settings(
    state: State<'_, Arc<AppState>>,
    settings: MeetingSettings,
) -> Result<(), String> {
    MeetingSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .set(&settings)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_system_audio_capability(
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let audio = state.audio.lock().await;
    Ok(system_audio_capability_dto(audio.system_audio_capability()))
}

#[tauri::command]
pub fn get_permission_status(state: State<'_, Arc<AppState>>, kind: String) -> Result<PermStatus, String> {
    if kind == "accessibility" {
        return Ok(accessibility_status());
    }
    let kind = parse_perm_kind(&kind)?;
    Ok(state.permissions.status(kind))
}

#[tauri::command]
pub async fn request_permission(
    state: State<'_, Arc<AppState>>,
    kind: String,
) -> Result<PermStatus, String> {
    if kind == "accessibility" {
        // Accessibility must be granted manually in System Settings (macOS); the
        // system prompt offers to open it directly when not yet trusted.
        #[cfg(target_os = "macos")]
        {
            let _ = kea_platform::textio::macos_ax::prompt_ax_trust();
        }
        return Ok(accessibility_status());
    }
    let kind = parse_perm_kind(&kind)?;
    state
        .permissions
        .request(kind)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_all_permission_statuses(
    state: State<'_, Arc<AppState>>,
) -> Vec<PermissionStatusItem> {
    all_permission_statuses(state.permissions.as_ref())
}

#[tauri::command]
pub async fn list_meetings(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
) -> Result<Vec<Meeting>, String> {
    state
        .meeting_repo
        .list(limit.unwrap_or(50))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_meeting(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<MeetingDetail, String> {
    state
        .meeting_repo
        .get(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {id} not found"))
}

#[tauri::command]
pub async fn delete_meeting(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state
        .meeting_repo
        .delete(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_meeting(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<String, String> {
    start_meeting_inner(&state, &app).await
}

#[tauri::command]
pub async fn stop_meeting(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<MeetingDetail, String> {
    stop_meeting_inner(&state, &app).await
}

#[tauri::command]
pub async fn get_dictation_state(
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let audio = state.audio.lock().await;
    let audio_state = audio.state();
    // Listening wins; idle with in-flight processing is reported as "processing"
    if audio_state == DictationState::Listening {
        return Ok("listening".into());
    }
    let in_flight = state
        .dictation_current_run
        .lock()
        .map_err(|e| e.to_string())?
        .is_some();
    if in_flight {
        return Ok("processing".into());
    }
    Ok(dictation_state_str(audio_state))
}

fn dictation_state_str(s: DictationState) -> String {
    match s {
        DictationState::Idle => "idle",
        DictationState::Listening => "listening",
        DictationState::Processing => "processing",
    }
    .into()
}

#[derive(serde::Serialize)]
pub struct MeetingStatePayload {
    pub state: String,
    pub active_meeting_id: Option<String>,
}

#[tauri::command]
pub async fn get_meeting_state(
    state: State<'_, Arc<AppState>>,
) -> Result<MeetingStatePayload, String> {
    let active_meeting_id = state
        .active_meeting
        .lock()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|s| s.session.meeting_id.clone());
    // Recording (active_meeting present) wins; otherwise report the
    // post-capture synthesis window as "processing" so a remounted page
    // doesn't show a lying Idle while notes are still generating.
    let state_str = if active_meeting_id.is_some() {
        "recording".to_string()
    } else if state.meeting_processing.load(Ordering::SeqCst) {
        "processing".to_string()
    } else {
        let audio = state.audio.lock().await;
        meeting_state_str(audio.meeting_state())
    };
    Ok(MeetingStatePayload {
        state: state_str,
        active_meeting_id,
    })
}

fn meeting_state_str(s: MeetingState) -> String {
    match s {
        MeetingState::Idle => "idle",
        MeetingState::Recording => "recording",
        MeetingState::Processing => "processing",
    }
    .into()
}

#[tauri::command]
pub async fn list_actions(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<ActionRow>, String> {
    let repo = ActionRepo::new(state.data_pool.clone());
    let limit = limit.unwrap_or(50);
    if let Some(q) = query.filter(|s| !s.trim().is_empty()) {
        repo.search(&q, limit).await.map_err(|e| e.to_string())
    } else {
        repo.recent(limit).await.map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn get_action(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<Option<ActionDetail>, String> {
    ActionRepo::new(state.data_pool.clone())
        .get(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_conversations(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
) -> Result<Vec<ConversationSummary>, String> {
    ConversationRepo::new(state.data_pool.clone())
        .list_recent(limit.unwrap_or(50))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_messages(
    state: State<'_, Arc<AppState>>,
    conversation_id: i64,
) -> Result<Vec<Message>, String> {
    ConversationRepo::new(state.data_pool.clone())
        .list_messages(conversation_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_conversation(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<(), String> {
    ConversationRepo::new(state.data_pool.clone())
        .delete_conversation(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn tail_logs(state: State<'_, Arc<AppState>>, max_bytes: Option<usize>) -> Result<String, String> {
    let path = current_log_path(&state.log_dir);
    if !path.exists() {
        return Ok(String::new());
    }
    let max_bytes = max_bytes.unwrap_or(64 * 1024);
    tail_log_file(&path, max_bytes).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_log_folder(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    std::fs::create_dir_all(&state.log_dir).map_err(|e| e.to_string())?;
    open_path_in_file_manager(&state.log_dir)
}

#[tauri::command]
pub fn list_tts_engines(state: State<'_, Arc<AppState>>) -> Vec<EngineInfoDto> {
    tts_engine_infos(&state.engines)
}

#[tauri::command]
pub async fn set_tts_binding(
    state: State<'_, Arc<AppState>>,
    engine: String,
    model: Option<String>,
    provider_ref: Option<String>,
) -> Result<(), String> {
    BindingRepo::new(state.config_pool.clone())
        .set(
            TTS_FEATURE_ID,
            "tts",
            Binding {
                engine_id: engine,
                model,
                provider_ref,
            },
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_tts_settings(state: State<'_, Arc<AppState>>) -> Result<TtsSettings, String> {
    TtsSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_tts_settings(
    state: State<'_, Arc<AppState>>,
    settings: TtsSettings,
) -> Result<(), String> {
    TtsSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .set(&settings)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_read_aloud(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

#[tauri::command]
pub async fn trigger_tts(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

#[tauri::command]
pub async fn read_selection(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

/// Synthesizes a short fixed sentence with the named TTS engine and plays it.
/// Bypasses selection capture and action history — this is a settings preview,
/// not a read-aloud run.
#[tauri::command]
pub async fn preview_voice(
    state: State<'_, Arc<AppState>>,
    engine: String,
    model: Option<String>,
    voice: Option<String>,
) -> Result<(), String> {
    const PREVIEW_SENTENCE: &str = "Hi! This is how this voice sounds when reading aloud.";
    let tts = state
        .engines
        .tts(&engine)
        .ok_or_else(|| format!("no tts engine '{engine}'"))?;
    let pcm = tts
        .synthesize(
            PREVIEW_SENTENCE,
            TtsOpts {
                model,
                voice,
                format: None,
                provider_ref: None,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let frame = PcmFrame {
        samples: pcm.samples,
        sample_rate_hz: pcm.sample_rate_hz,
    };
    tokio::task::spawn_blocking(move || {
        kea_platform::audio::playback::play_pcm_blocking(&frame)
    })
    .await
    .map_err(|e| format!("playback failed: {e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_onnx_models(kind: String) -> Result<Vec<OnnxModelEntry>, String> {
    onnx_catalog_for_kind(&kind)
}

#[tauri::command]
pub fn list_installed_onnx_models(
    state: State<'_, Arc<AppState>>,
    kind: String,
) -> Result<Vec<String>, String> {
    let storage = onnx_storage_for(&state, &kind)?;
    let catalog = onnx_catalog_for_kind(&kind)?;
    Ok(installed_onnx_model_ids(storage, &catalog))
}

#[tauri::command]
pub async fn download_onnx_model(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model_id: String,
    app: AppHandle,
) -> Result<(), String> {
    let storage = ModelStorage::new(onnx_storage_for(&state, &kind)?.root.clone());
    let entry = onnx_catalog_for_kind(&kind)?
        .into_iter()
        .find(|e| e.id == model_id)
        .ok_or_else(|| format!("unknown {kind} model: {model_id}"))?;

    let key = format!("onnx:{kind}:{model_id}");
    {
        let mut guard = state.active_downloads.lock().map_err(|e| e.to_string())?;
        if !guard.insert(key.clone()) {
            return Err(format!("download of '{model_id}' ({kind}) already in progress"));
        }
    }
    let downloader = new_model_downloader(storage);
    let app_handle = app.clone();
    let state_for_cleanup = state.inner().clone();
    let mid = model_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = downloader
            .download_onnx(&entry, |progress| {
                emit_model_download_progress(&app_handle, &progress);
            })
            .await;
        {
            let mut guard = state_for_cleanup.active_downloads.lock().unwrap();
            guard.remove(&key);
        }
        match result {
            Ok(_) => emit_model_download_complete(&app_handle, &mid),
            Err(error) => {
                emit_model_download_error(&app_handle, &mid, &error.to_string());
            }
        }
    });
    Ok(())
}

/// Removes an installed model's files (whisper .gguf file or onnx dir). When a
/// capability default referenced the removed model, that binding is dropped
/// too — the UI confirms with the user before calling this.
#[tauri::command]
pub async fn delete_model(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model_id: String,
) -> Result<(), String> {
    let default_slot = match kind.as_str() {
        "whisper" | "parakeet" => "stt",
        "tts" => "tts",
        _ => {
            return Err(format!(
                "unknown model kind: {kind} (expected whisper, parakeet, or tts)"
            ))
        }
    };
    match kind.as_str() {
        "whisper" => state.model_storage.remove_model(&model_id),
        _ => onnx_storage_for(&state, &kind)?.remove_onnx(&model_id),
    }
    .map_err(|e| format!("failed to remove model files: {e}"))?;

    let bindings = BindingRepo::new(state.config_pool.clone());
    clear_default_binding_for_model(&bindings, default_slot, &model_id).await?;
    Ok(())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())
    } else {
        manager.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn show_notification(app: AppHandle, title: String, body: String) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub status: String,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[cfg(feature = "updater")]
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<UpdateStatus, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(update) => Ok(UpdateStatus {
            status: "available".into(),
            version: Some(update.version.clone()),
            error: None,
        }),
        None => Ok(UpdateStatus {
            status: "up-to-date".into(),
            version: None,
            error: None,
        }),
    }
}

#[cfg(not(feature = "updater"))]
#[tauri::command]
pub fn check_update() -> Result<UpdateStatus, String> {
    Ok(UpdateStatus {
        status: "disabled".into(),
        version: None,
        error: Some("Update checking is not enabled in this build. Rebuild with --features updater after adding the public key.".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_core::rewrite::CredentialSourceAdapter;
    use kea_core::secrets::{CredentialStore, InMemoryCredentialStore};
    use kea_core::store::db::{open_pool, run_config_migrations};
    use kea_core::store::settings::SettingsRepo;
    use kea_features::{DictationFeature, Feature, MeetingFeature, RewriteFeature, TtsFeature};
    use kea_engines::{
        noop::NoopLlmEngine, register_phase1_engines, register_phase2_stt_engines,
        register_phase4_tts_engines, ReqwestHttpClient, EngineRegistry,
    };
    use kea_platform::{DictationState, SystemAudioCapability};
    use std::sync::Arc;

    async fn phase1_registry() -> EngineRegistry {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let providers = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(pool)));
        let creds = Arc::new(CredentialSourceAdapter::new(Arc::new(
            InMemoryCredentialStore::default(),
        )));
        let mut reg = EngineRegistry::default();
        register_phase1_engines(
            &mut reg,
            Arc::new(ReqwestHttpClient::new()),
            creds,
            providers,
        );
        reg
    }

    #[test]
    fn engine_ids_listing_is_pure() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        assert_eq!(engine_ids(&reg), vec!["noop".to_string()]);
    }

    #[tokio::test]
    async fn phase1_engine_ids_include_openai() {
        let reg = phase1_registry().await;
        let ids = engine_ids(&reg);
        assert!(ids.contains(&"openai".to_string()));
        assert!(ids.contains(&"openai-compatible".to_string()));
    }

    #[tokio::test]
    async fn engine_infos_maps_capabilities() {
        let reg = phase1_registry().await;
        let infos = engine_infos(&reg);
        let openai = infos.iter().find(|e| e.id == "openai").expect("openai");
        assert!(!openai.models.is_empty());
    }

    #[test]
    fn resolution_error_maps_outcomes() {
        assert!(resolution_error(Resolution::Bound("openai".into())).is_none());
        assert_eq!(
            resolution_error(Resolution::Unresolvable),
            Some("no llm engine available".into())
        );
        assert!(
            resolution_error(Resolution::NeedsChoice(vec!["a".into(), "b".into()]))
                .unwrap()
                .contains("a")
        );
    }

    #[tokio::test]
    async fn credential_exists_probes_without_exposing_secret() {
        // InMemoryCredentialStore stands in for the keyring.
        let store = InMemoryCredentialStore::default();
        assert!(!credential_exists(&store, "openai").await.unwrap());
        store.set("openai", "sk-test").await.unwrap();
        assert!(credential_exists(&store, "openai").await.unwrap());
        store.delete("openai").await.unwrap();
        assert!(!credential_exists(&store, "openai").await.unwrap());
    }

    #[test]
    fn provider_test_status_maps_to_human_messages() {
        let ok = provider_test_result_for_status(200);
        assert!(ok.ok);
        assert_eq!(ok.message, "Connected");
        let auth = provider_test_result_for_status(401);
        assert!(!auth.ok);
        assert_eq!(auth.message, "Invalid key");
        assert_eq!(provider_test_result_for_status(403).message, "Invalid key");
        let other = provider_test_result_for_status(500);
        assert!(!other.ok);
        assert!(other.message.contains("500"));
    }

    #[test]
    fn provider_entries_lists_built_ins_first_then_custom() {
        let custom = vec![
            CustomProvider {
                provider_ref: "groq".into(),
                name: "Groq".into(),
            },
            // shadowed by a built-in ref: dropped
            CustomProvider {
                provider_ref: "openai".into(),
                name: "Shadow".into(),
            },
        ];
        let entries = provider_entries(&custom);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].provider_ref, "openai");
        assert_eq!(entries[0].name, "OpenAI");
        assert!(entries[0].built_in);
        assert_eq!(entries[1].provider_ref, "local-llm");
        assert!(entries[1].built_in);
        assert_eq!(entries[2].provider_ref, "groq");
        assert!(!entries[2].built_in);
    }

    #[test]
    fn validate_new_provider_rejects_bad_input() {
        let existing = vec![CustomProvider {
            provider_ref: "groq".into(),
            name: "Groq".into(),
        }];
        assert!(validate_new_provider("", "Name", &existing).is_err());
        assert!(validate_new_provider("mistral", "  ", &existing).is_err());
        assert!(validate_new_provider("openai", "Name", &existing).is_err());
        assert!(validate_new_provider("groq", "Name", &existing).is_err());
        assert!(validate_new_provider("mistral", "Mistral", &existing).is_ok());
    }

    #[tokio::test]
    async fn custom_providers_roundtrip_under_single_settings_key() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool);
        assert!(load_custom_providers(&settings).await.unwrap().is_empty());
        let list = vec![CustomProvider {
            provider_ref: "groq".into(),
            name: "Groq".into(),
        }];
        save_custom_providers(&settings, &list).await.unwrap();
        assert_eq!(load_custom_providers(&settings).await.unwrap(), list);
    }

    #[tokio::test]
    async fn clear_default_binding_only_when_model_matches() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let bindings = kea_core::store::bindings::BindingRepo::new(pool);
        bindings
            .set(
                DEFAULT_FEATURE_ID,
                "tts",
                Binding {
                    engine_id: "sherpa-tts".into(),
                    model: Some("vits-piper-en-us-lessac-medium".into()),
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        // A different model leaves the default in place.
        assert!(
            !clear_default_binding_for_model(&bindings, "tts", "vits-piper-en-us-amy-low")
                .await
                .unwrap()
        );
        assert!(bindings.get(DEFAULT_FEATURE_ID, "tts").await.unwrap().is_some());

        // The referenced model clears it.
        assert!(
            clear_default_binding_for_model(&bindings, "tts", "vits-piper-en-us-lessac-medium")
                .await
                .unwrap()
        );
        assert!(bindings.get(DEFAULT_FEATURE_ID, "tts").await.unwrap().is_none());
    }

    #[test]
    fn validate_accelerator_accepts_valid() {
        assert!(validate_accelerator("Cmd+Shift+R").is_ok());
        assert!(validate_accelerator("CommandOrControl+Shift+D").is_ok());
        assert!(validate_accelerator("Ctrl+Alt+Delete").is_ok());
    }

    #[test]
    fn validate_accelerator_rejects_empty() {
        let err = validate_accelerator("").unwrap_err();
        assert!(
            err.contains("invalid accelerator"),
            "expected 'invalid accelerator' in error, got: {err}"
        );
    }

    #[test]
    fn validate_accelerator_rejects_multiple_main_keys() {
        assert!(validate_accelerator("Shift+R+A").is_err());
    }

    #[tokio::test]
    async fn validate_before_write_invalid_does_not_persist() {
        // Simulates set_hotkey's validation-before-write flow:
        // validate_accelerator fails → DB row is unchanged.
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();

        // Write a known-good row first.
        HotkeyBindingRepo::new(pool.clone())
            .set(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K")
            .await
            .unwrap();

        // Invalid accelerator should be rejected by validate_accelerator before
        // any DB write happens.
        assert!(validate_accelerator("").is_err());

        // DB still has the original value.
        let row = HotkeyBindingRepo::new(pool.clone())
            .get(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.accelerator, "Alt+K");
    }

    #[test]
    fn rewrite_feature_default_hotkey_is_non_empty() {
        let f = RewriteFeature;
        let cmd = &f.commands()[0];
        assert_eq!(cmd.id, REWRITE_COMMAND_ID);
        assert!(cmd.default_accelerator.is_some());
    }

    #[tokio::test]
    async fn resolve_rewrite_accelerator_falls_back_to_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        assert_eq!(
            resolve_rewrite_accelerator(&pool).await,
            default_rewrite_accelerator()
        );
    }

    #[tokio::test]
    async fn resolve_rewrite_accelerator_reads_db() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        HotkeyBindingRepo::new(pool.clone())
            .set(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K")
            .await
            .unwrap();
        assert_eq!(resolve_rewrite_accelerator(&pool).await, "Alt+K");
    }

    fn binding_row(feature_id: &str, command: &str, accelerator: &str) -> HotkeyBindingRow {
        HotkeyBindingRow {
            feature_id: feature_id.into(),
            command: command.into(),
            accelerator: accelerator.into(),
        }
    }

    #[test]
    fn old_hotkey_action_unregisters_unowned_accelerator() {
        // No other row maps to the old accelerator: plain unregister. An
        // unknown (feature, command) owner never had a live registration, so
        // it also resolves to unregister.
        let bindings = vec![
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K"),
            binding_row("unknown", "unknown_cmd", "Cmd+Shift+R"),
        ];
        assert_eq!(
            old_hotkey_action(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                Some("Cmd+Shift+R".into()),
                "Alt+K",
                &bindings,
            ),
            OldHotkeyAction::Unregister("Cmd+Shift+R".into())
        );
    }

    #[test]
    fn old_hotkey_action_reassigns_to_known_owner() {
        // Dictation still owns the old accelerator in the DB, so it must be
        // re-registered to dictation's action rather than unregistered.
        let bindings = vec![
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K"),
            binding_row(DICTATION_FEATURE_ID, DICTATION_COMMAND_ID, "Cmd+Shift+R"),
        ];
        assert_eq!(
            old_hotkey_action(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                Some("Cmd+Shift+R".into()),
                "Alt+K",
                &bindings,
            ),
            OldHotkeyAction::Reassign {
                accelerator: "Cmd+Shift+R".into(),
                feature_id: DICTATION_FEATURE_ID.into(),
                command: DICTATION_COMMAND_ID.into(),
            }
        );
    }

    #[test]
    fn old_hotkey_action_noop_when_unchanged_or_absent() {
        let bindings = vec![binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K")];
        assert_eq!(
            old_hotkey_action(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                Some("Alt+K".into()),
                "Alt+K",
                &bindings,
            ),
            OldHotkeyAction::None
        );
        assert_eq!(
            old_hotkey_action("unknown", "unknown_cmd", None, "Alt+K", &bindings),
            OldHotkeyAction::None
        );
    }

    #[test]
    fn old_hotkey_action_noop_for_alias_spellings() {
        // "Cmd" and "Super" parse to the same modifier on every platform;
        // unregistering the old spelling would kill the just-registered key.
        let bindings = vec![binding_row(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            "Super+Shift+R",
        )];
        assert_eq!(
            old_hotkey_action(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                Some("Cmd+Shift+R".into()),
                "Super+Shift+R",
                &bindings,
            ),
            OldHotkeyAction::None
        );
        // "CommandOrControl" aliases Cmd only on macOS (it means Ctrl elsewhere,
        // a genuinely different key, so treating it as a rebind is correct there).
        #[cfg(target_os = "macos")]
        assert_eq!(
            old_hotkey_action(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                Some("Cmd+Shift+R".into()),
                "CommandOrControl+Shift+R",
                &bindings,
            ),
            OldHotkeyAction::None
        );
    }

    #[test]
    fn collision_detects_other_features_custom_row() {
        // Rewrite has Cmd+Shift+D in DB → dictation can't also use Cmd+Shift+D.
        let bindings = vec![
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Cmd+Shift+D"),
        ];
        let hit = check_hotkey_collision(
            DICTATION_FEATURE_ID,
            DICTATION_COMMAND_ID,
            "Cmd+Shift+D",
            &bindings,
        );
        assert_eq!(hit, Some(format!("{REWRITE_FEATURE_ID}/{REWRITE_COMMAND_ID}")));
    }

    #[test]
    fn collision_detects_other_features_default_accelerator() {
        // No DB row for meetings → its compiled default is Cmd+Shift+M.
        // Dictation can't claim Cmd+Shift+M even if meetings has never been customized.
        let bindings: Vec<HotkeyBindingRow> = vec![];
        let hit = check_hotkey_collision(
            DICTATION_FEATURE_ID,
            DICTATION_COMMAND_ID,
            compiled_default_accelerator(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID).unwrap(),
            &bindings,
        );
        assert_eq!(
            hit,
            Some(format!("{MEETINGS_FEATURE_ID}/{MEETINGS_COMMAND_ID}"))
        );
    }

    #[test]
    fn collision_allows_self_rebind() {
        // Rewrite moves from Cmd+Shift+R to Alt+K — both its DB row and the
        // new value belong to rewrite, so no collision with itself.
        let bindings = vec![
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Cmd+Shift+R"),
        ];
        let hit = check_hotkey_collision(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            "Alt+K",
            &bindings,
        );
        assert_eq!(hit, None);
    }

    #[test]
    fn collision_alias_spelling_matches_other_feature() {
        // Dictation has Cmd+Shift+R in DB; rewrite tries "CommandOrControl+Shift+R"
        // which parses to the same id on macOS → collision.
        #[cfg(target_os = "macos")]
        {
            let bindings = vec![
                binding_row(DICTATION_FEATURE_ID, DICTATION_COMMAND_ID, "Cmd+Shift+R"),
            ];
            let hit = check_hotkey_collision(
                REWRITE_FEATURE_ID,
                REWRITE_COMMAND_ID,
                "CommandOrControl+Shift+R",
                &bindings,
            );
            assert_eq!(
                hit,
                Some(format!("{DICTATION_FEATURE_ID}/{DICTATION_COMMAND_ID}"))
            );
        }
    }

    async fn phase2_registry() -> EngineRegistry {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let providers = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(pool)));
        let creds = Arc::new(CredentialSourceAdapter::new(Arc::new(
            InMemoryCredentialStore::default(),
        )));
        let http = Arc::new(ReqwestHttpClient::new());
        let mut reg = EngineRegistry::default();
        register_phase2_stt_engines(&mut reg, http, creds, providers);
        reg
    }

    #[tokio::test]
    async fn phase2_engine_ids_include_openai_stt() {
        let reg = phase2_registry().await;
        let ids = reg.list_stt_ids();
        assert!(ids.contains(&"openai-stt".to_string()));
        #[cfg(not(feature = "whisper"))]
        assert!(!ids.contains(&"whisper".to_string()));
    }

    #[tokio::test]
    async fn stt_engine_infos_maps_capabilities() {
        let reg = phase2_registry().await;
        let infos = stt_engine_infos(&reg);
        let openai = infos
            .iter()
            .find(|e| e.id == "openai-stt")
            .expect("openai-stt");
        assert!(!openai.models.is_empty());
    }

    #[test]
    fn dictation_hotkey_action_toggles_listen_state() {
        assert_eq!(
            dictation_hotkey_action(DictationState::Idle, false, false),
            DictationHotkeyAction::Start
        );
        assert_eq!(
            dictation_hotkey_action(DictationState::Listening, false, false),
            DictationHotkeyAction::Stop
        );
        assert_eq!(
            dictation_hotkey_action(DictationState::Processing, false, false),
            DictationHotkeyAction::Ignore
        );
    }

    #[test]
    fn dictation_hotkey_action_ignores_during_meeting() {
        assert_eq!(
            dictation_hotkey_action(DictationState::Idle, true, false),
            DictationHotkeyAction::Ignore
        );
        assert_eq!(
            dictation_hotkey_action(DictationState::Listening, true, false),
            DictationHotkeyAction::Ignore
        );
        assert_eq!(
            dictation_hotkey_action(DictationState::Processing, true, false),
            DictationHotkeyAction::Ignore
        );
    }

    #[test]
    fn dictation_hotkey_action_ignores_when_in_flight() {
        // Audio is Idle but a dictation run is still processing → Ignore
        assert_eq!(
            dictation_hotkey_action(DictationState::Idle, false, true),
            DictationHotkeyAction::Ignore
        );
        // Meeting active + in-flight → still Ignore
        assert_eq!(
            dictation_hotkey_action(DictationState::Idle, true, true),
            DictationHotkeyAction::Ignore
        );
    }

    #[test]
    fn meeting_hotkey_action_ignores_while_processing() {
        // A prior meeting still synthesizing: never start a concurrent
        // meeting and never toggle, regardless of recording state.
        assert_eq!(
            meeting_hotkey_action(false, true),
            MeetingHotkeyAction::Ignore
        );
        assert_eq!(
            meeting_hotkey_action(true, true),
            MeetingHotkeyAction::Ignore
        );
    }

    #[test]
    fn meeting_hotkey_action_toggles_when_not_processing() {
        assert_eq!(
            meeting_hotkey_action(false, false),
            MeetingHotkeyAction::Start
        );
        assert_eq!(
            meeting_hotkey_action(true, false),
            MeetingHotkeyAction::Stop
        );
    }

    #[test]
    fn dictation_feature_default_hotkey_is_non_empty() {
        let f = DictationFeature;
        let cmd = &f.commands()[0];
        assert_eq!(cmd.id, DICTATION_COMMAND_ID);
        assert!(cmd.default_accelerator.is_some());
    }

    #[tokio::test]
    async fn resolve_dictation_accelerator_falls_back_to_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        assert_eq!(
            resolve_dictation_accelerator(&pool).await,
            default_dictation_accelerator()
        );
    }

    #[test]
    fn system_audio_capability_dto_maps_variants() {
        assert_eq!(
            system_audio_capability_dto(SystemAudioCapability::MicOnly),
            "mic_only"
        );
        assert_eq!(
            system_audio_capability_dto(SystemAudioCapability::ScreenCaptureKit),
            "screen_capture_kit"
        );
        assert_eq!(
            system_audio_capability_dto(SystemAudioCapability::LoopbackDevice),
            "loopback_device"
        );
        assert_eq!(
            system_audio_capability_dto(SystemAudioCapability::Unavailable),
            "unavailable"
        );
    }

    #[test]
    fn meeting_feature_is_registered() {
        let mut reg = kea_features::FeatureRegistry::default();
        reg.register(Arc::new(MeetingFeature));
        assert!(reg.list_ids().contains(&MEETINGS_FEATURE_ID.to_string()));
    }

    #[test]
    fn meeting_feature_default_hotkey_is_non_empty() {
        let f = MeetingFeature;
        assert_eq!(f.id(), MEETINGS_FEATURE_ID);
        let cmd = &f.commands()[0];
        assert_eq!(cmd.id, MEETINGS_COMMAND_ID);
        assert!(cmd.default_accelerator.is_some());
    }

    #[tokio::test]
    async fn resolve_meeting_accelerator_falls_back_to_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        assert_eq!(
            resolve_meeting_accelerator(&pool).await,
            default_meeting_accelerator()
        );
    }

    #[tokio::test]
    async fn resolve_meeting_accelerator_reads_db() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        HotkeyBindingRepo::new(pool.clone())
            .set(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID, "Cmd+Shift+N")
            .await
            .unwrap();
        assert_eq!(resolve_meeting_accelerator(&pool).await, "Cmd+Shift+N");
    }

    async fn phase4_registry() -> EngineRegistry {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let providers = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(pool)));
        let creds = Arc::new(CredentialSourceAdapter::new(Arc::new(
            InMemoryCredentialStore::default(),
        )));
        let http = Arc::new(ReqwestHttpClient::new());
        let mut reg = EngineRegistry::default();
        register_phase4_tts_engines(&mut reg, http, creds, providers);
        reg
    }

    #[tokio::test]
    async fn tts_engine_infos_maps_capabilities() {
        let reg = phase4_registry().await;
        let infos = tts_engine_infos(&reg);
        let openai = infos
            .iter()
            .find(|e| e.id == "openai-tts")
            .expect("openai-tts");
        assert!(!openai.models.is_empty());
    }

    #[test]
    fn tts_feature_default_hotkey_is_non_empty() {
        let f = TtsFeature;
        assert_eq!(f.id(), TTS_FEATURE_ID);
        let cmd = &f.commands()[0];
        assert_eq!(cmd.id, TTS_COMMAND_ID);
        assert!(cmd.default_accelerator.is_some());
    }

    #[tokio::test]
    async fn resolve_tts_accelerator_falls_back_to_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        assert_eq!(
            resolve_tts_accelerator(&pool).await,
            default_tts_accelerator()
        );
    }

    #[test]
    fn onnx_catalog_for_kind_returns_parakeet_and_tts() {
        let parakeet = onnx_catalog_for_kind("parakeet").unwrap();
        assert!(!parakeet.is_empty());
        let tts = onnx_catalog_for_kind("tts").unwrap();
        assert!(!tts.is_empty());
        assert!(onnx_catalog_for_kind("unknown").is_err());
    }

    #[test]
    fn installed_onnx_model_ids_filters_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let catalog = ModelRegistry::parakeet_catalog();
        assert!(installed_onnx_model_ids(&storage, &catalog).is_empty());
        let model_dir = storage.onnx_dir_for(&catalog[0].id);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();
        let installed = installed_onnx_model_ids(&storage, &catalog);
        assert_eq!(installed, vec![catalog[0].id.clone()]);
    }

    #[test]
    fn all_permission_statuses_lists_three_kinds() {
        let permissions = kea_platform::new_permissions();
        let items = all_permission_statuses(permissions.as_ref());
        assert_eq!(items.len(), 3);
        assert!(items.iter().any(|i| i.kind == "microphone"));
        assert!(items.iter().any(|i| i.kind == "screen_recording"));
        assert!(items.iter().any(|i| i.kind == "accessibility"));
    }

    #[tokio::test]
    async fn store_conversations_enabled_defaults_to_true() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        assert!(store_conversations_enabled(&pool).await);
    }

    #[tokio::test]
    async fn store_conversations_enabled_reads_setting_false() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        SettingsRepo::new(pool.clone())
            .set("history.store_conversations", &false)
            .await
            .unwrap();
        assert!(!store_conversations_enabled(&pool).await);
    }

    #[tokio::test]
    async fn store_conversations_enabled_reads_setting_true() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        SettingsRepo::new(pool.clone())
            .set("history.store_conversations", &true)
            .await
            .unwrap();
        assert!(store_conversations_enabled(&pool).await);
    }

    #[tokio::test]
    async fn store_conversations_enabled_honors_ui_string_encoding() {
        // The real UI path goes through the generic set_setting command,
        // which stores the value as a JSON *string* ("false"), not a bool —
        // this must still disable storage (regression: it parsed as bool,
        // failed, and defaulted to true, silently ignoring the opt-out).
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = SettingsRepo::new(pool.clone());
        repo.set("history.store_conversations", &"false".to_string())
            .await
            .unwrap();
        assert!(!store_conversations_enabled(&pool).await);
        repo.set("history.store_conversations", &"true".to_string())
            .await
            .unwrap();
        assert!(store_conversations_enabled(&pool).await);
    }

    #[test]
    fn effective_hotkey_returns_custom_when_db_row_exists() {
        let result = effective_hotkey(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            Some("Alt+K".to_string()),
        );
        assert_eq!(
            result,
            Some(EffectiveHotkey {
                accelerator: "Alt+K".into(),
                source: "custom".into(),
            })
        );
    }

    #[test]
    fn effective_hotkey_returns_default_when_no_db_row() {
        let result = effective_hotkey(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            None,
        );
        assert_eq!(
            result,
            Some(EffectiveHotkey {
                accelerator: default_rewrite_accelerator().into(),
                source: "default".into(),
            })
        );
    }

    #[test]
    fn effective_hotkey_returns_none_for_unknown_pair() {
        assert_eq!(effective_hotkey("unknown", "unknown_cmd", None), None);
    }

    #[test]
    fn effective_hotkey_returns_custom_for_unknown_pair_with_db_row() {
        // Even unknown pairs get a custom result if the DB had a row.
        let result = effective_hotkey(
            "unknown",
            "unknown_cmd",
            Some("Cmd+Y".to_string()),
        );
        assert_eq!(
            result,
            Some(EffectiveHotkey {
                accelerator: "Cmd+Y".into(),
                source: "custom".into(),
            })
        );
    }

    #[test]
    fn record_hotkey_reg_status_books_success_and_failure() {
        let mut m = HashMap::new();
        record_hotkey_reg_status(&mut m, "rewrite", "rewrite_selection", Ok(()));
        assert_eq!(m.len(), 1);
        let entry = m.get("rewrite:rewrite_selection").unwrap();
        assert!(entry.ok);
        assert!(entry.error.is_none());

        record_hotkey_reg_status(
            &mut m,
            "dictation",
            "push_to_talk",
            Err("conflict".into()),
        );
        let entry = m.get("dictation:push_to_talk").unwrap();
        assert!(!entry.ok);
        assert_eq!(entry.error.as_deref(), Some("conflict"));
    }

    #[test]
    fn clear_hotkey_reg_status_removes_entry() {
        let mut m = HashMap::new();
        m.insert(
            "rewrite:rewrite_selection".into(),
            HotkeyRegStatus {
                feature: "rewrite".into(),
                command: "rewrite_selection".into(),
                ok: false,
                error: Some("fail".into()),
            },
        );
        clear_hotkey_reg_status(&mut m, "rewrite", "rewrite_selection");
        assert!(!m.contains_key("rewrite:rewrite_selection"));
    }

    #[test]
    fn download_guard_prevents_duplicate_and_allows_reinsertion() {
        use std::collections::HashSet;
        use std::sync::Mutex;

        let guard = Mutex::new(HashSet::<String>::new());

        // First insert succeeds.
        {
            let mut s = guard.lock().unwrap();
            assert!(s.insert("whisper:tiny".into()));
        }
        // Duplicate is rejected.
        {
            let mut s = guard.lock().unwrap();
            assert!(!s.insert("whisper:tiny".into()));
        }
        // After removal, re-insert succeeds.
        {
            let mut s = guard.lock().unwrap();
            s.remove("whisper:tiny");
        }
        {
            let mut s = guard.lock().unwrap();
            assert!(s.insert("whisper:tiny".into()));
        }
        // Different key doesn't conflict.
        {
            let mut s = guard.lock().unwrap();
            assert!(s.insert("whisper:base".into()));
        }
    }

    #[test]
    fn try_acquire_busy_basic() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _guard = try_acquire_busy(&flag);
            assert!(flag.load(Ordering::Acquire));
        }
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn try_acquire_busy_rejects_second() {
        let flag = Arc::new(AtomicBool::new(false));
        let _g1 = try_acquire_busy(&flag);
        assert!(_g1.is_some());
        let g2 = try_acquire_busy(&flag);
        assert!(g2.is_none());
    }

    #[test]
    fn try_acquire_busy_retry_after_drop() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _g1 = try_acquire_busy(&flag).unwrap();
            drop(_g1);
        }
        let g2 = try_acquire_busy(&flag);
        assert!(g2.is_some());
    }
}
