use std::collections::HashMap;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use kea_core::dictation::{apply_vocabulary, DictationSettings, DictationSettingsRepo};
use kea_core::log::{current_log_path, tail_log_file};
use kea_core::meetings::{MeetingSettings, MeetingSettingsRepo};
use kea_core::resolve::Resolution;
use kea_core::resolve::SlotResolver;
use kea_core::rewrite::{
    build_llm_request, PresetRepo, PromptOverrideRepo, ProviderConfig, ProviderConfigRepo,
    RewriteInput, RewriteMode, RewritePreset,
};
use kea_core::store::actions::{ActionDetail, ActionRepo, ActionRow};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::conversations::{ConversationRepo, ConversationSummary, Message};
use kea_core::store::hotkeys::{HotkeyBindingRepo, HotkeyBindingRow};
use kea_core::store::meetings::{Meeting, MeetingDetail};
use kea_core::store::settings::SettingsRepo;
use kea_core::store::vocabulary::{VocabularyEntry, VocabularyRepo};
use kea_core::tts::{TtsSettings, TtsSettingsRepo};
use kea_engines::{EngineRegistry, TtsOpts};
use kea_features::demo::{run_ping, DemoFeature};
use kea_features::run_rewrite_with_storage;
use kea_features::tts::run_tts_with_player;
use kea_features::{
    drain_and_stop_meeting, run_dictation_with_storage, run_meeting_poll_segment,
    run_meeting_start, run_meeting_stop, ActiveMeeting, CapKind, ContentStorageOpts,
    DictationFeature, FeatureRegistry, MeetingFeature, MeetingRunContext, RewriteFeature,
    TtsFeature,
};
use kea_infer::{
    temp_file_for, DownloadTransport, InferError, ModelDownloader, ModelKind, ModelRegistry,
    ModelStorage, OnnxModelEntry, StreamedFile,
};
use kea_platform::audio::InputDevice;
use kea_platform::{
    new_text_io, parse_accelerator, AudioIo, AudioIoError, Cue, DictationState, HoldAction,
    HotkeyBinding, Hotkeys, MeetingState, PcmFrame, PermKind, PermStatus, SystemAudioCapability,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::watch;

use crate::events::{
    dictation_state_wire, emit_device_fallback, emit_dictation_error, emit_dictation_level,
    emit_dictation_preview, emit_dictation_state, emit_meeting_error, emit_meeting_level,
    emit_meeting_segment, emit_meeting_state, emit_model_download_complete,
    emit_model_download_error, emit_model_download_progress, emit_tts_state, meeting_state_wire,
    MeetingSegmentPayload, TtsState,
};
use crate::{ActiveDownload, AppState};

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

/// One global hotkey: the `(feature, command)` pair the DB rows and the UI
/// address it by, plus the action id the dispatch loop matches on.
///
/// No accelerator here on purpose — the default belongs to the feature that
/// declares the command ([`kea_features::Command::default_accelerator`]) and is
/// read back through [`compiled_default_accelerator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyAction {
    pub feature: &'static str,
    pub command: &'static str,
    pub action_id: &'static str,
}

/// Every command that owns a global hotkey.
///
/// This is the one table the hotkey paths read — startup registration,
/// `set_hotkey`, its rebind cleanup, collision detection and the
/// effective-hotkey lookup — so adding a feature hotkey is a row here rather
/// than another arm in five matches.
pub const HOTKEY_ACTIONS: [HotkeyAction; 4] = [
    HotkeyAction {
        feature: REWRITE_FEATURE_ID,
        command: REWRITE_COMMAND_ID,
        action_id: REWRITE_ACTION_ID,
    },
    HotkeyAction {
        feature: DICTATION_FEATURE_ID,
        command: DICTATION_COMMAND_ID,
        action_id: DICTATION_ACTION_ID,
    },
    HotkeyAction {
        feature: TTS_FEATURE_ID,
        command: TTS_COMMAND_ID,
        action_id: TTS_ACTION_ID,
    },
    HotkeyAction {
        feature: MEETINGS_FEATURE_ID,
        command: MEETINGS_COMMAND_ID,
        action_id: MEETINGS_ACTION_ID,
    },
];

/// Escape, while — and only while — a locked recording is running.
///
/// Deliberately **not** a [`HOTKEY_ACTIONS`] row, although the doc above says
/// that table is where hotkeys are added. Every reader of that table describes
/// a shortcut the user owns: startup registration, rebinding, collision
/// detection, the effective-hotkey lookup and the dispatch table. This one is
/// registered and unregistered by [`set_dictation_lock`] around a single
/// recording, because holding Escape globally for any longer would take it
/// away from every other app on the Mac. Its press still arrives on the one
/// accelerator stream, so `hotkeys::spawn_dispatch_loop` matches it by hand.
pub const LOCK_CANCEL_ACTION_ID: &str = "dictation:cancel_lock";

/// The accelerator behind [`LOCK_CANCEL_ACTION_ID`]. Not user-rebindable:
/// "Escape cancels" is the platform convention, not a preference.
const LOCK_CANCEL_ACCELERATOR: &str = "Escape";

/// The descriptor for a `(feature, command)` pair, or `None` when the pair is
/// not a global hotkey — a binding persisted for some other command, say.
pub fn hotkey_action(feature: &str, command: &str) -> Option<HotkeyAction> {
    HOTKEY_ACTIONS
        .iter()
        .copied()
        .find(|a| a.feature == feature && a.command == command)
}

/// The registered features, built once.
///
/// Also the app's [`AppState::features`](crate::AppState) — it is immutable
/// after startup, and the hotkey defaults have to be readable from pure
/// helpers that never see the state.
pub fn feature_registry() -> &'static FeatureRegistry {
    static REGISTRY: OnceLock<FeatureRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut reg = FeatureRegistry::default();
        reg.register(Arc::new(DemoFeature));
        reg.register(Arc::new(RewriteFeature));
        reg.register(Arc::new(DictationFeature));
        reg.register(Arc::new(MeetingFeature));
        reg.register(Arc::new(TtsFeature));
        reg
    })
}

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
pub fn effective_hotkey(
    feature: &str,
    command: &str,
    db_accel: Option<String>,
) -> Option<EffectiveHotkey> {
    match db_accel {
        Some(accel) => Some(EffectiveHotkey {
            accelerator: accel,
            source: "custom".into(),
        }),
        None => compiled_default_accelerator(feature, command).map(|a| EffectiveHotkey {
            accelerator: a,
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
    statuses.insert(
        key,
        HotkeyRegStatus {
            feature: feature.to_string(),
            command: command.to_string(),
            ok,
            error,
        },
    );
}

/// Pure helper: clears a single registration-status entry when re-registration succeeds.
pub fn clear_hotkey_reg_status(
    statuses: &mut HashMap<String, HotkeyRegStatus>,
    feature: &str,
    command: &str,
) {
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

/// The permission kinds the UI can ask about, paired with the snake_case names
/// the IPC layer speaks. One table, so the parser and the status list cannot
/// drift apart.
const PERM_KINDS: [(&str, PermKind); 3] = [
    ("microphone", PermKind::Microphone),
    ("screen_recording", PermKind::ScreenRecording),
    ("accessibility", PermKind::Accessibility),
];

fn parse_perm_kind(kind: &str) -> Result<PermKind, String> {
    PERM_KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, perm)| *perm)
        .ok_or_else(|| format!("unknown permission kind: {kind}"))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionStatusItem {
    pub kind: String,
    pub status: PermStatus,
}

pub fn all_permission_statuses(
    permissions: &dyn kea_platform::Permissions,
) -> Vec<PermissionStatusItem> {
    PERM_KINDS
        .iter()
        .map(|(name, perm)| PermissionStatusItem {
            kind: (*name).into(),
            status: permissions.status(*perm),
        })
        .collect()
}

/// Parses the model kind an IPC command was handed. Every command that takes a
/// `kind: String` funnels through here, so the unknown-kind rejection lives in
/// one place and everything downstream works on the enum.
pub fn parse_model_kind(kind: &str) -> Result<ModelKind, String> {
    ModelKind::try_from(kind).map_err(|e| e.to_string())
}

pub fn onnx_catalog_for_kind(kind: ModelKind) -> Result<Vec<OnnxModelEntry>, String> {
    ModelRegistry::onnx_catalog(kind)
        .ok_or_else(|| format!("unknown onnx model kind: {kind} (expected parakeet or tts)"))
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
        if entries
            .iter()
            .any(|e| e.provider_ref == provider.provider_ref)
        {
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

/// Characters a provider ref may contain. The ref becomes part of settings
/// keys (`provider.<ref>`) and of the keychain account name, so anything
/// outside this set — whitespace, slashes, quotes, uppercase — is rejected
/// rather than silently normalized.
fn is_valid_provider_ref_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
}

/// Validates a to-be-added custom provider and returns the entry to store
/// (pure, unit-testable). Both fields are trimmed *before* validation, so a
/// ref like `" openai"` can't slip past the built-in / duplicate checks and
/// then be stored — and displayed — as `openai`.
pub fn validate_new_provider(
    provider_ref: &str,
    name: &str,
    existing: &[CustomProvider],
) -> Result<CustomProvider, String> {
    let provider_ref = provider_ref.trim();
    let name = name.trim();
    if provider_ref.is_empty() {
        return Err("Provider id can't be empty".into());
    }
    if name.is_empty() {
        return Err("Provider name can't be empty".into());
    }
    if !provider_ref.chars().all(is_valid_provider_ref_char) {
        return Err(format!(
            "Provider id \"{provider_ref}\" may only use lowercase letters, digits, dot, underscore and hyphen"
        ));
    }
    if BUILT_IN_PROVIDERS.iter().any(|(r, _)| *r == provider_ref) {
        return Err(format!("\"{provider_ref}\" is a built-in provider"));
    }
    if existing.iter().any(|p| p.provider_ref == provider_ref) {
        return Err(format!("A provider \"{provider_ref}\" already exists"));
    }
    Ok(CustomProvider {
        provider_ref: provider_ref.into(),
        name: name.into(),
    })
}

async fn load_custom_providers(settings: &SettingsRepo) -> Result<Vec<CustomProvider>, String> {
    match settings.get(CUSTOM_PROVIDERS_KEY).await {
        Ok(list) => Ok(list.unwrap_or_default()),
        // A corrupt stored value must not hide the built-in providers; the
        // next save overwrites it.
        Err(kea_core::error::KeaError::Serde(e)) => {
            tracing::warn!(%e, "corrupt providers.custom value ignored");
            Ok(Vec::new())
        }
        Err(e) => Err(e.to_string()),
    }
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

/// Trims a server error body down to something worth showing in a row. Servers
/// answer with anything from a bare string to a large HTML page, so cap it and
/// unwrap the common `{"detail": "..."}` / `{"error": {"message": "..."}}`
/// shapes rather than pasting raw JSON at the user.
pub fn server_detail(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() || body.starts_with('<') {
        return None;
    }
    let text = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("detail")
                .or_else(|| v.pointer("/error/message"))
                .or_else(|| v.get("message"))
                .and_then(|d| d.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.to_string());
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(160).collect())
}

/// Maps a `GET {base_url}/models` outcome to a human-readable test result.
///
/// `has_key` matters: a 401 with no key sent means nothing was saved for this
/// provider, which is a different problem from a key the server rejected —
/// reporting both as "Invalid key" sends people hunting for a bad key when
/// none was ever read. The server's own explanation is appended when it gives
/// one, because the status alone can't distinguish "this endpoint doesn't
/// accept API keys" from "this key is wrong".
pub fn provider_test_result_for_response(
    status: u16,
    has_key: bool,
    body: &str,
) -> ProviderTestResult {
    let detail = server_detail(body);
    let with_detail = |base: String| match &detail {
        Some(d) => format!("{base} — {d}"),
        None => base,
    };
    match status {
        200..=299 => ProviderTestResult {
            ok: true,
            message: "Connected".into(),
        },
        401 | 403 if !has_key => ProviderTestResult {
            ok: false,
            message: with_detail("No API key saved for this provider".into()),
        },
        401 | 403 => ProviderTestResult {
            ok: false,
            message: with_detail("Server rejected the key".into()),
        },
        404 => ProviderTestResult {
            ok: false,
            message: with_detail(format!(
                "No model list at this address (status {status}) — the base URL may be wrong"
            )),
        },
        s => ProviderTestResult {
            ok: false,
            message: with_detail(format!("Server responded with status {s}")),
        },
    }
}

/// Courtesy warning appended to a test result when the saved key would travel
/// over plain HTTP. Advisory only — it never changes the ok/failed verdict,
/// because a local server on `http://` is a legitimate setup.
pub const CLEARTEXT_KEY_WARNING: &str = "key sent over plain http";

/// Whether testing this provider would put the bearer key on the wire in
/// cleartext (pure, unit-testable).
pub fn sends_key_in_cleartext(base_url: &str, has_key: bool) -> bool {
    has_key
        && base_url
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("http://")
}

/// Appends the cleartext warning to a result's message, keeping `ok` as-is.
pub fn with_cleartext_warning(result: ProviderTestResult) -> ProviderTestResult {
    ProviderTestResult {
        message: format!("{} — {CLEARTEXT_KEY_WARNING}", result.message),
        ..result
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

/// Drops every binding for `slot` that names `model_id` — the capability
/// default *and* per-feature overrides (dictation/stt, meetings/stt, tts/tts).
/// Returns the feature ids whose bindings were cleared. A row left pointing at
/// deleted files would otherwise only surface as an inference failure later.
pub async fn clear_bindings_for_model(
    bindings: &BindingRepo,
    slot: &str,
    model_id: &str,
) -> Result<Vec<String>, String> {
    bindings
        .delete_by_model(slot, model_id)
        .await
        .map_err(|e| e.to_string())
}

/// Clears the settings-level fallback model (`dictation.active_model` /
/// `tts.active_model`) when it names the removed model. Those settings are
/// consulted when a binding carries no model of its own, so they dangle the
/// same way a binding does. Returns whether a setting was cleared.
pub async fn clear_active_model_for_deleted(
    config_pool: &SqlitePool,
    slot: &str,
    model_id: &str,
) -> Result<bool, String> {
    match slot {
        "stt" => {
            let repo = DictationSettingsRepo::new(SettingsRepo::new(config_pool.clone()));
            let current = repo.get().await.map_err(|e| e.to_string())?;
            if current.active_model.as_deref() != Some(model_id) {
                return Ok(false);
            }
            repo.set(&DictationSettings {
                active_model: None,
                ..current
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok(true)
        }
        "tts" => {
            let repo = TtsSettingsRepo::new(SettingsRepo::new(config_pool.clone()));
            let current = repo.get().await.map_err(|e| e.to_string())?;
            if current.active_model.as_deref() != Some(model_id) {
                return Ok(false);
            }
            repo.set(&TtsSettings {
                active_model: None,
                ..current
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The engine capability a slot name implies. Slot names are the capability
/// names everywhere in this app (`llm` / `stt` / `tts`); anything else is a
/// custom slot with no known capability.
pub fn capability_for_slot(slot: &str) -> Option<CapKind> {
    match slot {
        "llm" => Some(CapKind::Llm),
        "stt" => Some(CapKind::Stt),
        "tts" => Some(CapKind::Tts),
        _ => None,
    }
}

/// Rejects an engine that can't serve `slot`: unknown ids always, and — for
/// the three known slot kinds — engines registered under another capability.
/// A `tts` engine bound to the `llm` slot would persist a default that every
/// resolve silently skips, so the UI would show a default that isn't used.
/// Unknown/custom slots keep the previous "registered anywhere" rule.
pub fn validate_engine_for_slot(
    engines: &EngineRegistry,
    slot: &str,
    engine: &str,
) -> Result<(), String> {
    let llm = engines.llm(engine).is_some();
    let stt = engines.stt(engine).is_some();
    let tts = engines.tts(engine).is_some();
    if !(llm || stt || tts) {
        return Err(format!("unknown engine id: {engine}"));
    }
    let (ok, label) = match capability_for_slot(slot) {
        Some(CapKind::Llm) => (llm, "text"),
        Some(CapKind::Stt) => (stt, "speech-to-text"),
        Some(CapKind::Tts) => (tts, "text-to-speech"),
        None => return Ok(()),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "engine '{engine}' is not a {label} engine (slot '{slot}')"
        ))
    }
}

/// The live accelerator for one global hotkey: the user's row when there is
/// one, otherwise the feature's compiled-in default.
pub async fn resolve_accelerator(config_pool: &SqlitePool, action: &HotkeyAction) -> String {
    let repo = HotkeyBindingRepo::new(config_pool.clone());
    match repo.get(action.feature, action.command).await {
        Ok(Some(row)) => row.accelerator,
        // Every row in HOTKEY_ACTIONS names a command whose feature declares a
        // default; falling back to empty keeps this total rather than panicking
        // if one is ever dropped, and registration then fails visibly.
        _ => compiled_default_accelerator(action.feature, action.command).unwrap_or_default(),
    }
}

/// Best-effort system language as a BCP-47 primary subtag, for the one case the
/// settings row cannot cover: a fresh install where the user has pressed the
/// rewrite hotkey in Translate mode before ever opening the Rewrite page.
///
/// Best-effort really means it: a macOS app launched from Finder usually has no
/// `LANG` at all, which is why the picker seeds itself from `navigator.language`
/// in the UI and that value is what normally ends up in settings. This is the
/// floor under that, not a replacement for it.
fn system_language_tag() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .ok()
        .and_then(|v| {
            // "en_US.UTF-8" -> "en-US"; "C" and "POSIX" are not languages.
            let base = v.split('.').next().unwrap_or("").replace('_', "-");
            let primary = base.split('-').next().unwrap_or("");
            (primary.len() == 2 || primary.len() == 3).then_some(base)
        })
        .unwrap_or_else(|| "en".to_string())
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
    // Dispatch through the descriptor rather than naming a mode: `AskKea` takes
    // an instruction and `Translate` a target language, and each stores it under
    // its own settings key. A `matches!(mode, ..)` here is the shape that made
    // adding Translate mean editing four unrelated conditionals.
    let custom_instruction = if let Some(parameter) = mode.parameter() {
        settings
            .get_optional::<String>(parameter.setting_key())
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    // Translate is the one mode whose parameter is not optional — without a
    // target the prompt cannot be rendered at all — so it falls back rather
    // than failing the run.
    let custom_instruction = match (mode, custom_instruction) {
        (RewriteMode::Translate, None) => {
            let tag = system_language_tag();
            tracing::info!(
                target_language = %tag,
                "translate: no target language set, falling back to the system language"
            );
            Some(tag)
        }
        (_, other) => other,
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

/// Settings key for the dictation cue sounds toggle.
pub const SOUND_CUES_SETTING: &str = "sound.cues_enabled";

/// Reads the cue-sounds flag, defaulting ON.
///
/// Same both-shapes tolerance as `store_conversations_enabled`: the generic
/// `set_setting` command writes a JSON string, other callers a JSON bool.
pub fn cues_enabled_from_setting(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::String(s)) => s != "false",
        Some(_) | None => true,
    }
}

async fn sound_cues_enabled(config_pool: &SqlitePool) -> bool {
    match SettingsRepo::new(config_pool.clone())
        .get::<serde_json::Value>(SOUND_CUES_SETTING)
        .await
    {
        Ok(value) => cues_enabled_from_setting(value.as_ref()),
        Err(e) => {
            tracing::warn!(%e, "failed to read {SOUND_CUES_SETTING}, defaulting to on");
            true
        }
    }
}

/// Plays a dictation outcome cue, unless the user turned sounds off.
///
/// Fire-and-forget: playback pins a blocking-pool thread for the length of the
/// cue, and the caller is on the path that returns the transcript.
pub fn spawn_dictation_cue(state: &Arc<AppState>, cue: Cue) {
    let config_pool = state.config_pool.clone();
    tauri::async_runtime::spawn(async move {
        if !sound_cues_enabled(&config_pool).await {
            return;
        }
        let frame = kea_platform::cue_pcm(cue);
        let played = tokio::task::spawn_blocking(move || {
            kea_platform::audio::playback::play_pcm_blocking(&frame)
        })
        .await;
        match played {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, ?cue, "failed to play dictation cue"),
            Err(e) => tracing::warn!(error = %e, ?cue, "dictation cue playback task failed"),
        }
    });
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

/// Bind `accelerator` to one global-hotkey action.
pub fn register_hotkey(
    hotkeys: &mut Box<dyn Hotkeys>,
    action: &HotkeyAction,
    accelerator: &str,
) -> Result<(), String> {
    hotkeys
        .register(
            HotkeyBinding {
                accelerator: accelerator.to_string(),
            },
            action.action_id.into(),
        )
        .map_err(|e| e.to_string())
}

pub async fn trigger_tts_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    emit_tts_state(app, TtsState::Reading);
    // Any early failure between here and the terminal emit must still return
    // the UI to idle, otherwise the global status banner sticks on
    // "Reading selection aloud…".
    let result = trigger_tts_run(state, app).await;
    if result.is_err() {
        emit_tts_state(app, TtsState::Idle);
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

    // The action lifecycle lives in the feature; this only supplies the way
    // the app reaches the speakers. Playback goes to the free function on a
    // blocking thread rather than through `state.audio`, whose mutex guards
    // capture and must not be held for the length of the audio.
    run_tts_with_player(
        &state.engines,
        &bindings,
        &actions,
        textio.as_ref(),
        &settings,
        |pcm| async move {
            tokio::task::spawn_blocking(move || {
                kea_platform::audio::playback::play_pcm_blocking(&pcm)
            })
            .await
            .map_err(|e| format!("playback failed: {e}"))?
            .map_err(|e| e.to_string())
        },
    )
    .await?;

    emit_tts_state(app, TtsState::Idle);
    Ok(())
}

/// Reads the three inputs [`dictation_hotkey_action`] and
/// [`hold_dictation_action`] gate on, in the order that keeps the answer
/// right: the meeting flags come first because during meeting synthesis the
/// audio lock is free but starting dictation would still be wrong, and reading
/// them first avoids any park-and-replay behind that lock.
///
/// This is only the hotkey gate. `start_dictation_run` and
/// `get_dictation_state` answer different questions with deliberately
/// different precedence and do not share it.
pub async fn dictation_gate(state: &Arc<AppState>) -> (bool, bool, DictationState) {
    let meeting_active = state
        .active_meeting
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
        || state.meeting_processing.load(Ordering::SeqCst);
    let in_flight = state
        .dictation_current_run
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false);
    let current = state.audio.lock().await.state();
    // The capture device cannot tell a locked run from a held one — both are
    // an open microphone — so the lock is read from the app's own flag here,
    // where the gating rules can see it.
    let current = match (current, state.dictation_locked.load(Ordering::SeqCst)) {
        (DictationState::Listening, true) => DictationState::Locked,
        (current, _) => current,
    };
    (meeting_active, in_flight, current)
}

/// Toggle push-to-talk: global-hotkey currently delivers press events only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationHotkeyAction {
    Start,
    /// Start a run that outlives the keys, ended by the next tap, by Escape or
    /// by the hard cap. Only the hold path ever asks for this.
    StartLocked,
    Stop,
    /// Stop without transcribing, throwing the audio away.
    Cancel,
    Ignore,
}

pub fn dictation_hotkey_action(
    current: DictationState,
    meeting_active: bool,
    in_flight: bool,
) -> DictationHotkeyAction {
    if meeting_active {
        return DictationHotkeyAction::Ignore;
    }
    match current {
        DictationState::Listening => DictationHotkeyAction::Stop,
        // A locked run is stopped by the same toggle as a held one: the
        // accelerator means "I am done talking" either way, and leaving it
        // inert would make the lock the one state with no way out but the
        // chord.
        DictationState::Locked => DictationHotkeyAction::Stop,
        DictationState::Idle if in_flight => DictationHotkeyAction::Ignore,
        DictationState::Idle => DictationHotkeyAction::Start,
        DictationState::Processing => DictationHotkeyAction::Ignore,
    }
}

/// Maps a hold-to-talk edge onto the dictation state machine.
///
/// Hold-to-talk is directional where the accelerator is a toggle: the chord
/// going down can only ever start, and coming up can only ever stop. Routing it
/// through [`dictation_hotkey_action`] keeps one set of rules about when
/// dictation may run at all (meetings, a run still finishing), and this narrows
/// that answer to the direction the edge asked for — so a hold that begins
/// while a recording is already running cannot restart it, and a release that
/// arrives after the run ended some other way cannot stop the next one.
/// Matched on `event` without a wildcard on purpose: a new [`HoldAction`]
/// variant must not be able to compile clean into a silent `Ignore`.
pub fn hold_dictation_action(
    event: HoldAction,
    current: DictationState,
    meeting_active: bool,
    in_flight: bool,
) -> DictationHotkeyAction {
    let toggle = dictation_hotkey_action(current, meeting_active, in_flight);
    let narrow = |wanted, then| {
        if toggle == wanted {
            then
        } else {
            DictationHotkeyAction::Ignore
        }
    };

    match event {
        // Arming is about the microphone, never about the run: an armed
        // stream records nothing until a `Start` follows, and the dictation
        // toggle must not see either edge.
        HoldAction::Nothing | HoldAction::Arm | HoldAction::Disarm => DictationHotkeyAction::Ignore,
        HoldAction::Start => narrow(DictationHotkeyAction::Start, DictationHotkeyAction::Start),
        HoldAction::StartLocked => narrow(
            DictationHotkeyAction::Start,
            DictationHotkeyAction::StartLocked,
        ),
        HoldAction::Stop | HoldAction::StopLocked => {
            narrow(DictationHotkeyAction::Stop, DictationHotkeyAction::Stop)
        }
        HoldAction::CancelLocked => {
            narrow(DictationHotkeyAction::Stop, DictationHotkeyAction::Cancel)
        }
    }
}

/// Carry out a decision from either hotkey path.
///
/// Shared so the accelerator and the hold chord cannot drift: they answer
/// different questions (a toggle versus a directed edge) but they run the same
/// five transitions, and a lock started by one has to be stoppable by the
/// other.
pub async fn run_dictation_action(
    action: DictationHotkeyAction,
    state: &Arc<AppState>,
    app: &AppHandle,
) {
    let outcome = match action {
        DictationHotkeyAction::Start => start_dictation_inner(state, app).await,
        DictationHotkeyAction::StartLocked => start_locked_dictation_inner(state, app).await,
        DictationHotkeyAction::Stop => stop_dictation_inner(state, app).await.map(|_| ()),
        DictationHotkeyAction::Cancel => cancel_dictation_inner(state, app).await,
        DictationHotkeyAction::Ignore => Ok(()),
    };
    if let Err(error) = outcome {
        emit_dictation_error(app, &error);
    }
}

/// Brings the ⌥⇧ hold-to-talk listener in line with the setting.
///
/// Installing is one-way (the platform listener outlives any disable), so this
/// is safe to call on every settings write and at startup; turning the mode off
/// only clears the flag the listener consults.
///
/// A failure to install is surfaced as a dictation error rather than swallowed:
/// the realistic cause is missing Accessibility permission, and a hold-to-talk
/// that silently does nothing is the exact failure this release set out to
/// remove. `installed` stays false in that case so granting the permission and
/// toggling the setting again retries.
pub fn sync_hold_to_talk(state: &Arc<AppState>, app: &AppHandle, enabled: bool) {
    state.hold_to_talk_enabled.store(enabled, Ordering::SeqCst);
    if !enabled {
        return;
    }

    {
        let mut installed = match state.hold_to_talk_installed.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *installed {
            return;
        }

        let (control, events) =
            match kea_platform::spawn_hold_to_talk(state.hold_to_talk_enabled.clone()) {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(%error, "hold-to-talk listener could not start");
                    emit_dictation_error(app, &error.to_string());
                    return;
                }
            };
        *installed = true;
        // Kept so a lock can be ended by something other than the keyboard
        // tap — Escape, or a run that finished by another route.
        *state.hold_control.lock().unwrap_or_else(|p| p.into_inner()) = Some(control);
        spawn_hold_to_talk_dispatch(state, app, events);
    }
}

fn spawn_hold_to_talk_dispatch(
    state: &Arc<AppState>,
    app: &AppHandle,
    mut events: tokio::sync::mpsc::UnboundedReceiver<HoldAction>,
) {
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = events.recv().await {
            // Arming opens and closes the microphone without starting a run,
            // so it is handled before the busy flag: a modifier press must not
            // contend with a recording for the flag that serialises runs.
            if matches!(event, HoldAction::Arm | HoldAction::Disarm) {
                apply_preroll_edge(&state, event).await;
                continue;
            }

            // Same busy flag as the accelerator path, so a chord and a
            // Cmd+Shift+D landing together cannot both start a run.
            let Some(_busy) = try_acquire_busy(&state.dictation_busy) else {
                tracing::debug!(?event, "hold-to-talk ignored: dictation handler in flight");
                continue;
            };

            let (meeting_active, in_flight, current) = dictation_gate(&state).await;
            let action = hold_dictation_action(event, current, meeting_active, in_flight);
            run_dictation_action(action, &state, &app).await;
        }
    });
}

/// Open or close the preroll capture for a modifier edge.
///
/// Silently does nothing when the setting is off, which is the whole of the
/// switch: with no armed stream the recording opens its own and simply starts
/// ~150ms later, which is what every release before this one did.
async fn apply_preroll_edge(state: &Arc<AppState>, event: HoldAction) {
    let mut audio = state.audio.lock().await;
    match event {
        // Only the opening edge is gated by the setting.
        HoldAction::Arm if state.preroll_enabled.load(Ordering::SeqCst) => audio.arm_capture(),
        // The closing edge never is: a stream armed before the user switched
        // the setting off still has to be closed, or it would hold the
        // microphone open until KEA quits.
        HoldAction::Disarm => audio.disarm_capture(),
        _ => {}
    }
}

/// Turn locked mode on or off.
///
/// One function owns both halves — the flag the state emits read from, and the
/// Escape accelerator that only exists while a lock is running — because they
/// must never disagree: a stale flag reports a run that ended, and a stale
/// Escape registration swallows the key from every other app.
fn set_dictation_lock(state: &Arc<AppState>, locked: bool) {
    let was_locked = state.dictation_locked.swap(locked, Ordering::SeqCst);
    if was_locked == locked {
        return;
    }

    let mut hotkeys = state.hotkeys.lock().unwrap_or_else(|p| p.into_inner());
    let binding = HotkeyBinding {
        accelerator: LOCK_CANCEL_ACCELERATOR.to_string(),
    };
    let result = if locked {
        hotkeys.register(binding, LOCK_CANCEL_ACTION_ID.into())
    } else {
        hotkeys.unregister(&binding)
    };
    if let Err(error) = result {
        // Not fatal either way: without it Escape does not cancel, and the
        // tap, the accelerator and the hard cap all still end the recording.
        tracing::warn!(%error, locked, "could not update the lock-cancel Escape binding");
    }
}

/// End a locked recording from outside the keyboard tap, so the hold machine
/// does not keep a lock that no longer has a run behind it.
fn release_hold_lock(state: &Arc<AppState>) {
    let control = state
        .hold_control
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(control) = control {
        control.reset();
    }
}

/// What Escape should do, asked of the hold machine rather than assumed: the
/// key is registered around a lock, and a race could still deliver the press
/// after the recording ended some other way.
pub fn lock_cancel_action(state: &Arc<AppState>) -> HoldAction {
    let control = state
        .hold_control
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    match control {
        Some(control) => control.cancel_lock(),
        None => HoldAction::Nothing,
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
    async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
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

/// How long to wait for the connection to come up before giving up.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a transfer may go without delivering *any* bytes before it counts
/// as dead. This is per-read, not per-transfer: a model is hundreds of
/// megabytes and a slow link can legitimately take many minutes, so an overall
/// deadline would cancel healthy downloads. What must never happen is the
/// no-timeout case — a socket that stops delivering leaves the download task
/// parked forever, so it emits neither completion nor error and the UI waits
/// on a transfer that will never end.
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);

struct ReqwestDownloadTransport {
    connect_timeout: Duration,
    stall_timeout: Duration,
}

impl ReqwestDownloadTransport {
    fn new() -> Self {
        Self::with_timeouts(DOWNLOAD_CONNECT_TIMEOUT, DOWNLOAD_STALL_TIMEOUT)
    }

    fn with_timeouts(connect_timeout: Duration, stall_timeout: Duration) -> Self {
        Self {
            connect_timeout,
            stall_timeout,
        }
    }
}

#[async_trait]
impl DownloadTransport for ReqwestDownloadTransport {
    async fn fetch_to_file(
        &self,
        url: &str,
        dest: &std::path::Path,
        on_chunk: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<StreamedFile, InferError> {
        use futures_util::StreamExt;
        use sha2::{Digest, Sha256};
        use std::io::Write;

        let client = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .read_timeout(self.stall_timeout)
            .build()
            .map_err(|e| InferError::Other(e.to_string()))?;
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
        let total = response.content_length().unwrap_or(0);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::io::BufWriter::new(std::fs::File::create(dest)?);
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| InferError::Other(e.to_string()))?;
            hasher.update(&chunk);
            file.write_all(&chunk)?;
            received += chunk.len() as u64;
            on_chunk(received, total);
        }
        file.flush()?;

        Ok(StreamedFile {
            bytes: received,
            sha256: format!("{:x}", hasher.finalize()),
        })
    }
}

pub fn new_model_downloader(storage: ModelStorage) -> ModelDownloader {
    ModelDownloader::new(Arc::new(ReqwestDownloadTransport::new()), storage)
}

fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Runs a download to completion and reduces every way it can end into one
/// outcome the caller turns into exactly one event.
///
/// The panic arm is the point. A spawned task that unwinds is dropped
/// silently: no completion, no error, nothing in the log — and the picker,
/// which refuses to re-issue while a request is pending, sits on "starting
/// download…" until the app is restarted. Catching the unwind here turns an
/// invisible death into a message the UI can show and recover from.
async fn run_download_task<F>(model_id: &str, download: F) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), InferError>>,
{
    use futures_util::FutureExt;

    let outcome = std::panic::AssertUnwindSafe(download).catch_unwind().await;
    match outcome {
        Ok(Ok(())) => {
            tracing::info!(model = %model_id, "model download finished");
            Ok(())
        }
        Ok(Err(error)) => {
            let message = error.to_string();
            tracing::warn!(model = %model_id, error = %message, "model download failed");
            Err(message)
        }
        Err(payload) => {
            let message = format!("download crashed: {}", panic_reason(payload.as_ref()));
            tracing::error!(model = %model_id, error = %message, "model download panicked");
            Err(message)
        }
    }
}

/// What registering a download needs, whichever family it belongs to. The key
/// is deliberately not a field: [`start_download`] derives it from
/// `(kind, model_id)` so the start side has exactly one producer and the key
/// [`cancel_model_download`] re-derives cannot drift from it.
struct DownloadRequest<'a> {
    kind: ModelKind,
    model_id: &'a str,
    /// Where the transfer stages bytes, so a cancel can clear the partial file.
    temp_path: PathBuf,
    /// How a second start for the same model is refused; the two families word
    /// it differently.
    busy_message: String,
}

/// Registers one transfer and owns the whole contract `cancel_model_download`
/// depends on: the key it re-derives, the temp path it deletes, and the single
/// terminal event the UI waits on.
///
/// The commands above it keep only what is family-specific — storage, catalog
/// entry, the transfer closure. That split is the point: the cancellation
/// contract used to be written out once per kind, so a fix to one copy left
/// cancel broken for the other.
fn start_download<Fut>(
    state: &Arc<AppState>,
    app: &AppHandle,
    request: DownloadRequest<'_>,
    run: impl FnOnce(AppHandle) -> Fut + Send + 'static,
) -> Result<(), String>
where
    Fut: std::future::Future<Output = Result<(), InferError>> + Send + 'static,
{
    let DownloadRequest {
        kind,
        model_id,
        temp_path,
        busy_message,
    } = request;
    let key = kind.download_key(model_id);

    let mut guard = state.active_downloads.lock().map_err(|e| e.to_string())?;
    if guard.contains_key(&key) {
        return Err(busy_message);
    }

    let app_handle = app.clone();
    let state_for_cleanup = state.clone();
    let mid = model_id.to_string();
    let cleanup_key = key.clone();
    // The handle is registered while the lock is held, so a task that finishes
    // instantly cannot have its entry removed before it was ever inserted.
    let task = tauri::async_runtime::spawn(async move {
        let outcome = run_download_task(&mid, run(app_handle.clone())).await;
        {
            let mut guard = state_for_cleanup.active_downloads.lock().unwrap();
            guard.remove(&cleanup_key);
        }
        match outcome {
            Ok(()) => emit_model_download_complete(&app_handle, &mid),
            Err(message) => emit_model_download_error(&app_handle, &mid, &message),
        }
    });
    guard.insert(
        key,
        ActiveDownload {
            model_id: model_id.to_string(),
            temp_path,
            task,
        },
    );
    Ok(())
}

fn onnx_storage_for(state: &AppState, kind: ModelKind) -> Result<&ModelStorage, String> {
    match kind {
        ModelKind::Parakeet => Ok(&state.parakeet_storage),
        ModelKind::Tts => Ok(&state.tts_storage),
        ModelKind::Whisper => Err(format!("unknown onnx model kind: {kind}")),
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

/// The cancel half of a running poll loop, parked on [`AppState`] so the next
/// spawn can stop the previous one.
type PollCancel = Mutex<Option<watch::Sender<bool>>>;

/// How often the level polls sample the meter.
const LEVEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn stop_poll(slot: &PollCancel) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(true);
        }
    }
}

/// Runs `tick` every `every` on the async runtime until the loop is cancelled
/// through `slot` or `tick` asks to stop, replacing whatever loop `slot` was
/// holding.
fn spawn_cancellable_poll<F, Fut>(slot: &PollCancel, every: Duration, tick: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = ControlFlow<()>> + Send + 'static,
{
    stop_poll(slot);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(cancel_tx);
    }

    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(every);
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    if tick().await.is_break() {
                        break;
                    }
                }
            }
        }
    });
}

fn stop_level_poll(state: &AppState) {
    stop_poll(&state.level_poll_cancel);
}

fn stop_segment_poll(state: &AppState) {
    stop_poll(&state.segment_poll_cancel);
}

/// Publishes the input meter on `emit` until the level poll is cancelled.
/// Dictation and meetings share the one slot: only one of them records at a
/// time, so starting either stops the other's meter.
fn spawn_audio_level_poll(state: &Arc<AppState>, app: &AppHandle, emit: fn(&AppHandle, f32)) {
    let poll_state = state.clone();
    let poll_app = app.clone();
    spawn_cancellable_poll(&state.level_poll_cancel, LEVEL_POLL_INTERVAL, move || {
        let state = poll_state.clone();
        let app = poll_app.clone();
        async move {
            let level = state.audio.lock().await.current_level();
            emit(&app, level);
            ControlFlow::Continue(())
        }
    });
}

fn spawn_level_poll(state: &Arc<AppState>, app: &AppHandle) {
    spawn_audio_level_poll(state, app, emit_dictation_level);
}

fn spawn_meeting_level_poll(state: &Arc<AppState>, app: &AppHandle) {
    spawn_audio_level_poll(state, app, emit_meeting_level);
}

/// How often the loop asks whether the buffer has reached a cut point. The
/// segment *length* is no longer set by this interval — the cut is decided
/// from the audio (a pause, or the configured maximum), so this only bounds
/// how promptly a pause is noticed.
const SEGMENT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// `vocabulary` is read once by the caller when the meeting starts and shared
/// across every tick, rather than re-read here per segment: a transcript whose
/// spelling changed halfway through because the user edited their vocabulary
/// mid-recording would be worse than either answer taken consistently.
fn spawn_segment_poll(
    state: &Arc<AppState>,
    app: &AppHandle,
    interval_secs: u32,
    vocabulary: Arc<Vec<VocabularyEntry>>,
) {
    // The configured length is passed to the cut logic as the hard cap, not
    // used as the tick rate.
    let _ = interval_secs;
    let vocabulary_for_poll = vocabulary;
    let state_for_poll = state.clone();
    let app_for_poll = app.clone();
    spawn_cancellable_poll(
        &state.segment_poll_cancel,
        SEGMENT_POLL_INTERVAL,
        move || {
            let state = state_for_poll.clone();
            let app = app_for_poll.clone();
            let vocabulary = vocabulary_for_poll.clone();
            async move {
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
                    return ControlFlow::Break(());
                };

                let settings =
                    match MeetingSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
                        .get()
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            emit_meeting_error(&app, &e.to_string());
                            return ControlFlow::Continue(());
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
                        vocabulary: &vocabulary,
                    };
                    run_meeting_poll_segment(&mut ctx, &meeting_id, &mut sequence, &mut elapsed_ms)
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
                ControlFlow::Continue(())
            }
        },
    );
}

pub async fn start_meeting_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<String, String> {
    // Reject before the audio lock while a prior meeting is still finishing:
    // during synthesis active_meeting is None and audio is Idle, so without
    // this a hotkey press would start a concurrent, UI-unstoppable meeting.
    if state.meeting_processing.load(Ordering::SeqCst) {
        return Err("a meeting is finishing; wait for it to complete".into());
    }

    {
        let guard = state.active_meeting.lock().map_err(|e| e.to_string())?;
        if guard.is_some() {
            return Err("a meeting is already recording".into());
        }
    }

    // As for dictation: a meeting takes the device off the preview, and this
    // is the half of that the frontend hears about.
    stop_input_preview_inner(state, app).await;

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

    // One read, shared by the start context, the segment poll and the stop.
    let vocabulary = Arc::new(load_vocabulary(&state.config_pool).await);

    let session = {
        let mut audio = state.audio.lock().await;
        let mut ctx = MeetingRunContext {
            engines: &state.engines,
            bindings: &bindings,
            actions: &actions,
            meetings,
            audio: audio.as_mut(),
            settings: &settings,
            vocabulary: &vocabulary,
        };
        let session = run_meeting_start(&mut ctx).await?;
        report_device_fallback(app, audio.as_mut());
        session
    };

    let meeting_id = session.meeting_id.clone();
    {
        let mut guard = state.active_meeting.lock().map_err(|e| e.to_string())?;
        *guard = Some(ActiveMeetingSession {
            session,
            sequence: 0,
            elapsed_ms: 0,
        });
    }

    emit_meeting_state(app, MeetingState::Recording);
    spawn_meeting_level_poll(state, app);
    spawn_segment_poll(
        state,
        app,
        settings.segment_duration_secs,
        vocabulary.clone(),
    );

    Ok(meeting_id)
}

pub async fn stop_meeting_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
) -> Result<MeetingDetail, String> {
    stop_segment_poll(state);
    stop_level_poll(state);

    let session = {
        let mut guard = state.active_meeting.lock().map_err(|e| e.to_string())?;
        guard
            .take()
            .ok_or_else(|| "no meeting is recording".to_string())?
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

    emit_meeting_state(app, MeetingState::Processing);

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

    let vocabulary = load_vocabulary(&state.config_pool).await;

    let detail = run_meeting_stop(
        &state.engines,
        &bindings,
        &actions,
        meetings,
        &session.session,
        drain_result,
        &vocabulary,
    )
    .await;

    match &detail {
        Ok(_) => emit_meeting_state(app, MeetingState::Idle),
        Err(e) => {
            emit_meeting_error(app, e);
            emit_meeting_state(app, MeetingState::Idle);
        }
    }

    detail
}

pub async fn start_dictation_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    let result = start_dictation_run(state, app).await;
    if result.is_err() {
        spawn_dictation_cue(state, Cue::Error);
    }
    result
}

/// Start a run that keeps recording with no keys held.
///
/// The lock is set before the microphone opens so the very first
/// `dictation:state` already says `locked`: the HUD would otherwise flash
/// "Listening" and correct itself, which reads as a glitch in exactly the mode
/// whose whole job is to say "yes, this is still recording".
pub async fn start_locked_dictation_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
) -> Result<(), String> {
    set_dictation_lock(state, true);
    let result = start_dictation_inner(state, app).await;
    if result.is_err() {
        set_dictation_lock(state, false);
        release_hold_lock(state);
    }
    result
}

/// The state a listening run publishes: the microphone is open either way, and
/// only the app knows whether a key is holding it there.
fn listening_state(state: &Arc<AppState>) -> DictationState {
    if state.dictation_locked.load(Ordering::SeqCst) {
        DictationState::Locked
    } else {
        DictationState::Listening
    }
}

/// Report a microphone that was not the one the user picked.
fn report_device_fallback(app: &AppHandle, audio: &mut dyn AudioIo) {
    if let Some(fallback) = audio.take_device_fallback() {
        emit_device_fallback(app, &fallback);
    }
}

async fn start_dictation_run(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    // The preview loses to a recording. `start_mic` cancels it too, but this
    // is the half that tells the frontend, so the "Test microphone" toggle
    // does not stay lit over a recording it is not part of.
    stop_input_preview_inner(state, app).await;

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
            emit_dictation_state(app, DictationState::Idle);
            return Err(e.to_string());
        }
        report_device_fallback(app, audio.as_mut());
    }

    emit_dictation_state(app, listening_state(state));
    spawn_level_poll(state, app);
    Ok(())
}

/// Stop a run and throw its audio away. Reached only from Escape during a
/// locked recording — every other route through the dictation state machine
/// transcribes what it captured.
pub async fn cancel_dictation_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    stop_level_poll(state);
    set_dictation_lock(state, false);
    release_hold_lock(state);

    let mut audio = state.audio.lock().await;
    if audio.state() != DictationState::Listening {
        // Re-sync only from idle: the HUD may still be showing a lock whose
        // run ended some other way, but a run that is mid-transcription owns
        // the state and must not be reported as finished.
        let stale = audio.state() == DictationState::Idle;
        drop(audio);
        if stale {
            emit_dictation_state(app, DictationState::Idle);
        }
        return Err("dictation is not listening".into());
    }
    let discarded = audio.stop_mic().await.map_err(|e| e.to_string());
    drop(audio);

    emit_dictation_state(app, DictationState::Idle);
    spawn_dictation_cue(state, Cue::Cancel);
    discarded.map(|_| ())
}

pub async fn stop_dictation_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
) -> Result<String, String> {
    let result = stop_dictation_run(state, app).await;
    spawn_dictation_cue(state, cue_for_dictation_outcome(&result));
    result
}

/// Which cue a finished dictation run earns.
///
/// There is no abort/cancel path in dictation — a hotkey press either starts,
/// stops, or is ignored — so the neutral blip goes to the closest thing there
/// is: a run that completed but produced nothing to insert.
pub fn cue_for_dictation_outcome(result: &Result<String, String>) -> Cue {
    match result {
        Ok(text) if text.trim().is_empty() => Cue::Cancel,
        Ok(_) => Cue::Success,
        Err(_) => Cue::Error,
    }
}

async fn stop_dictation_run(state: &Arc<AppState>, app: &AppHandle) -> Result<String, String> {
    stop_level_poll(state);
    // Whatever ended the run — the chord, the accelerator, the hard cap — the
    // lock goes with it, and the hold machine is told so the next chord is
    // judged fresh rather than read as the tap that stops a lock.
    set_dictation_lock(state, false);
    release_hold_lock(state);

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
                    emit_dictation_state(app, DictationState::Idle);
                }
            }
            return Err("dictation is not listening".into());
        }
        match audio.stop_mic().await {
            Ok(pcm) => pcm,
            Err(e) => {
                emit_dictation_state(app, DictationState::Idle);
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
                emit_dictation_state(self.app, DictationState::Idle);
            }
        }
    }
    let _run_guard = RunGuard {
        flag: &state.dictation_current_run,
        run_id,
        app,
    };

    emit_dictation_state(app, DictationState::Processing);

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

    let vocabulary = load_vocabulary(&state.config_pool).await;

    let result = run_dictation_with_storage(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &overrides,
        &mut replay,
        textio.as_ref(),
        &settings,
        &vocabulary,
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
    // Validate the engine against the slot's capability before persisting: an
    // engine of the wrong kind would be skipped by every resolve, leaving the
    // UI showing a default that is never used.
    validate_engine_for_slot(&state.engines, &slot, &engine)?;
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
    let has_key = api_key.is_some();
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let result = match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            // Read the body before mapping: the status alone can't tell a
            // rejected key from an endpoint that doesn't honour API keys.
            let body = response.text().await.unwrap_or_default();
            provider_test_result_for_response(status, has_key, &body)
        }
        Err(e) => ProviderTestResult {
            ok: false,
            message: if e.is_timeout() {
                "Server did not respond within 10 seconds".into()
            } else {
                "Server unreachable".into()
            },
        },
    };
    Ok(if sends_key_in_cleartext(&base_url, has_key) {
        with_cleartext_warning(result)
    } else {
        result
    })
}

#[tauri::command]
pub async fn list_providers(state: State<'_, Arc<AppState>>) -> Result<Vec<ProviderEntry>, String> {
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
    // Store the normalized entry, not the raw input.
    custom.push(validate_new_provider(&provider_ref, &name, &custom)?);
    save_custom_providers(&settings, &custom).await
}

#[tauri::command]
pub async fn update_custom_provider(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    name: String,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
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

/// Reads the enabled vocabulary, failing open.
///
/// Deliberately returns an empty list rather than an error on a read failure:
/// vocabulary is an accuracy aid, and a transcript with the wrong spelling of a
/// product name is enormously better than a dictation run that refuses to
/// insert anything because a settings table could not be read.
async fn load_vocabulary(config_pool: &SqlitePool) -> Vec<VocabularyEntry> {
    match VocabularyRepo::new(config_pool.clone())
        .list_enabled()
        .await
    {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("vocabulary unavailable, continuing without it: {e}");
            Vec::new()
        }
    }
}

#[tauri::command]
pub async fn list_vocabulary(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VocabularyEntry>, String> {
    VocabularyRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upsert_vocabulary_entry(
    state: State<'_, Arc<AppState>>,
    entry: VocabularyEntry,
) -> Result<(), String> {
    VocabularyRepo::new(state.config_pool.clone())
        .upsert(&entry)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_vocabulary_entry(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    VocabularyRepo::new(state.config_pool.clone())
        .delete(&id)
        .await
        .map_err(|e| e.to_string())
}

/// Runs the replacement pass over `text` for the settings page's live preview.
///
/// Goes through the backend rather than reimplementing the rules in TypeScript
/// so the box cannot drift from what dictation actually does — a preview that
/// disagrees with the feature is worse than no preview.
#[tauri::command]
pub async fn preview_vocabulary(
    state: State<'_, Arc<AppState>>,
    text: String,
) -> Result<String, String> {
    let entries = VocabularyRepo::new(state.config_pool.clone())
        .list_enabled()
        .await
        .map_err(|e| e.to_string())?;
    Ok(apply_vocabulary(&text, &entries))
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
pub async fn delete_preset(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
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
pub fn get_hotkey_registration_status(state: State<'_, Arc<AppState>>) -> Vec<HotkeyRegStatus> {
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
///
/// Read off the feature's own `Command`, so the shell can never offer a default
/// the feature does not declare.
fn compiled_default_accelerator(feature: &str, command: &str) -> Option<String> {
    let action = hotkey_action(feature, command)?;
    feature_registry()
        .find_command(action.feature, action.command)
        .and_then(|c| c.default_accelerator)
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
    for other in HOTKEY_ACTIONS {
        if other.feature == feature && other.command == command {
            continue;
        }
        let other_effective = bindings
            .iter()
            .find(|b| b.feature_id == other.feature && b.command == other.command)
            .map(|b| b.accelerator.clone())
            .or_else(|| compiled_default_accelerator(other.feature, other.command));

        if let Some(other_accel) = other_effective {
            if same_accelerator(accelerator, &other_accel) {
                return Some(format!("{}/{}", other.feature, other.command));
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
            && hotkey_action(&b.feature_id, &b.command).is_some()
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
        None => compiled_default_accelerator(&feature, &command),
    };

    // --- collision detection: prevent two features from sharing an accelerator ---
    let all_bindings = binding_repo.list().await.map_err(|e| e.to_string())?;

    if let Some(owner) = check_hotkey_collision(&feature, &command, &accelerator, &all_bindings) {
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
        match hotkey_action(&feature, &command) {
            Some(action) => {
                register_hotkey(&mut hotkeys, &action, &accelerator)?;
                true
            }
            None => {
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
                // `old_hotkey_action` only reassigns to pairs in HOTKEY_ACTIONS,
                // so the lookup is the same invariant, structurally.
                let result = match hotkey_action(&owner_feature, &owner_command) {
                    Some(action) => register_hotkey(&mut hotkeys, &action, &old_accel),
                    None => Ok(()),
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
        let mut statuses = state.hotkey_reg_status.lock().map_err(|e| e.to_string())?;
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

/// Runs the configured rewrite prompt over `text` with the Rewrite feature's
/// own LLM binding and returns the result.
///
/// Unlike [`trigger_rewrite`] this never captures the selection and never
/// pastes anything back — it exists for the "Try it" card on the Rewrite page,
/// where the user types the sample text and expects to read the result in the
/// window rather than have it inserted into whatever app is frontmost.
///
/// It also deliberately skips the `ActionRepo` record/finish pair and the
/// conversation storage that [`execute_rewrite`] performs, so a try-it run
/// leaves no trace in History or Logs. Experimenting in the settings window is
/// not something the user did to their own text, and padding their history
/// with it would bury the runs that were.
#[tauri::command]
pub async fn preview_rewrite(
    state: State<'_, Arc<AppState>>,
    text: String,
    mode: RewriteMode,
    preset_id: Option<String>,
    custom_instruction: Option<String>,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("type some text to rewrite first".into());
    }

    let bindings = BindingRepo::new(state.config_pool.clone());
    let presets = PresetRepo::new(state.config_pool.clone());
    let overrides = PromptOverrideRepo::new(state.config_pool.clone());

    let resolver = SlotResolver::new(&state.engines, &bindings);
    let binding = match resolver
        .resolve(REWRITE_FEATURE_ID, CapKind::Llm)
        .await
        .map_err(|e| e.to_string())?
    {
        Resolution::Bound(b) => b,
        other => return Err(resolution_error(other).unwrap_or_else(|| "resolution failed".into())),
    };

    let mut req = build_llm_request(
        &RewriteInput {
            source_text: text,
            mode,
            preset_id,
            custom_instruction,
        },
        &presets,
        &overrides,
    )
    .await
    .map_err(|e| e.to_string())?;
    req.model = binding.model.clone();
    // Try-it must hit the same provider the real rewrite would, or it would
    // report a key problem the user does not have (or hide one they do).
    req.provider_ref = binding.provider_ref.clone();

    let engine = state
        .engines
        .llm(&binding.engine_id)
        .ok_or_else(|| format!("no llm engine '{}'", binding.engine_id))?;

    engine
        .complete(req)
        .await
        .map(|resp| resp.text)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn run_demo(state: State<'_, Arc<AppState>>, prompt: String) -> Result<String, String> {
    let bindings = BindingRepo::new(state.config_pool.clone());
    let resolver = SlotResolver::new(&state.engines, &bindings);
    let binding = match resolver
        .resolve("demo", CapKind::Llm)
        .await
        .map_err(|e| e.to_string())?
    {
        Resolution::Bound(b) => b,
        other => return Err(resolution_error(other).unwrap_or_else(|| "resolution failed".into())),
    };
    run_ping(
        &state.engines,
        &binding.engine_id,
        binding.provider_ref.clone(),
        &prompt,
    )
    .await
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
    let storage = ModelStorage::new(state.model_storage.root.clone());
    let temp_path = temp_file_for(&storage.path_for(&model_id));
    let downloader = new_model_downloader(storage);
    let mid = model_id.clone();

    start_download(
        state.inner(),
        &app,
        DownloadRequest {
            kind: ModelKind::Whisper,
            model_id: &model_id,
            temp_path,
            busy_message: format!("download of '{model_id}' already in progress"),
        },
        move |app| async move {
            downloader
                .download_whisper(&mid, |progress| {
                    emit_model_download_progress(&app, &progress);
                })
                .await
                .map(|_| ())
        },
    )?;
    tracing::info!(model = %model_id, kind = "whisper", "model download started");
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
    app: AppHandle,
    settings: DictationSettings,
) -> Result<(), String> {
    DictationSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .set(&settings)
        .await
        .map_err(|e| e.to_string())?;
    // Only after the write succeeds: a listener armed against a setting that
    // did not persist would come back disarmed on the next launch.
    apply_dictation_settings(&state, &app, &settings).await;
    Ok(())
}

/// Push the settings that live outside the database into the running app: the
/// hold listener, the preroll flag the hold dispatch reads, and the capture
/// device.
///
/// Called on every write and once at startup, so "what is saved" and "what the
/// next recording does" cannot drift apart.
pub async fn apply_dictation_settings(
    state: &Arc<AppState>,
    app: &AppHandle,
    settings: &DictationSettings,
) {
    state
        .preroll_enabled
        .store(settings.preroll, Ordering::SeqCst);
    state
        .audio
        .lock()
        .await
        .set_input_device(settings.input_device.clone());
    sync_hold_to_talk(state, app, settings.hold_to_talk);
}

#[tauri::command]
pub async fn set_dictation_stt_binding(
    state: State<'_, Arc<AppState>>,
    engine: String,
    model: Option<String>,
    provider_ref: Option<String>,
) -> Result<(), String> {
    validate_engine_for_slot(&state.engines, "stt", &engine)?;
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

/// How long the input preview may hold the microphone before stopping itself.
///
/// Checking a microphone takes seconds; a preview left running holds the
/// device and keeps the macOS orange indicator lit for as long as the window
/// is open, which looks exactly like the app recording behind the user's back.
const INPUT_PREVIEW_MAX: Duration = Duration::from_secs(30);

#[tauri::command]
pub async fn list_input_devices(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<InputDevice>, String> {
    Ok(state.audio.lock().await.list_input_devices())
}

#[tauri::command]
pub async fn start_input_preview(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    start_input_preview_inner(&state, &app).await
}

#[tauri::command]
pub async fn stop_input_preview(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    stop_input_preview_inner(&state, &app).await;
    Ok(())
}

async fn start_input_preview_inner(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    {
        let mut audio = state.audio.lock().await;
        audio
            .start_input_preview()
            .await
            .map_err(|e| e.to_string())?;
        report_device_fallback(app, audio.as_mut());
    }
    emit_dictation_preview(app, true);
    // The same meter loop dictation and meetings use: only one of the three
    // can hold the device, so they can share the one slot.
    spawn_level_poll(state, app);
    spawn_preview_deadline(state, app);
    Ok(())
}

/// Stop the preview if one is running, and tell the frontend either way it
/// matters. Safe to call blind — the timer, window blur, a dictation start and
/// the toggle itself all come through here.
pub async fn stop_input_preview_inner(state: &Arc<AppState>, app: &AppHandle) {
    {
        let mut audio = state.audio.lock().await;
        if !audio.preview_active() {
            return;
        }
        if let Err(error) = audio.stop_input_preview().await {
            tracing::warn!(%error, "input preview did not stop cleanly");
        }
    }
    // Invalidates any deadline still counting down for the preview just
    // stopped, so a later one is not cut short by an older timer.
    state.preview_generation.fetch_add(1, Ordering::SeqCst);
    stop_level_poll(state);
    emit_dictation_level(app, 0.0);
    emit_dictation_preview(app, false);
}

fn spawn_preview_deadline(state: &Arc<AppState>, app: &AppHandle) {
    let generation = state.preview_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INPUT_PREVIEW_MAX).await;
        // A newer preview (or any stop) bumped the counter, so this timer is
        // about a preview that is already over.
        if state.preview_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        stop_input_preview_inner(&state, &app).await;
    });
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
pub fn get_permission_status(
    state: State<'_, Arc<AppState>>,
    kind: String,
) -> Result<PermStatus, String> {
    let kind = parse_perm_kind(&kind)?;
    Ok(state.permissions.status(kind))
}

#[tauri::command]
pub async fn request_permission(
    state: State<'_, Arc<AppState>>,
    kind: String,
) -> Result<PermStatus, String> {
    let kind = parse_perm_kind(&kind)?;
    state
        .permissions
        .request(kind)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_all_permission_statuses(state: State<'_, Arc<AppState>>) -> Vec<PermissionStatusItem> {
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
pub async fn get_dictation_state(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let audio = state.audio.lock().await;
    let audio_state = audio.state();
    // Listening wins; idle with in-flight processing is reported as "processing"
    if audio_state == DictationState::Listening {
        // A page that loads mid-lock has to be told it is a lock, or its badge
        // would read "Listening" for a recording with no key holding it.
        return Ok(dictation_state_wire(listening_state(&state)).into());
    }
    let in_flight = state
        .dictation_current_run
        .lock()
        .map_err(|e| e.to_string())?
        .is_some();
    if in_flight {
        return Ok(dictation_state_wire(DictationState::Processing).into());
    }
    Ok(dictation_state_wire(audio_state).into())
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
        meeting_state_wire(MeetingState::Recording).to_string()
    } else if state.meeting_processing.load(Ordering::SeqCst) {
        meeting_state_wire(MeetingState::Processing).to_string()
    } else {
        let audio = state.audio.lock().await;
        meeting_state_wire(audio.meeting_state()).to_string()
    };
    Ok(MeetingStatePayload {
        state: state_str,
        active_meeting_id,
    })
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
pub async fn delete_conversation(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    ConversationRepo::new(state.data_pool.clone())
        .delete_conversation(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn tail_logs(
    state: State<'_, Arc<AppState>>,
    max_bytes: Option<usize>,
) -> Result<String, String> {
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
    validate_engine_for_slot(&state.engines, "tts", &engine)?;
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
pub async fn run_read_aloud(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

#[tauri::command]
pub async fn trigger_tts(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

#[tauri::command]
pub async fn read_selection(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    trigger_tts_inner(&state, &app).await
}

/// Holds the "a preview is running" flag for as long as it lives, clearing it
/// on every exit path (including `?` returns, cancellation and panics).
pub struct PreviewGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> PreviewGuard<'a> {
    /// Claims the preview slot, or `None` when one is already running.
    pub fn try_acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self { flag })
    }
}

impl Drop for PreviewGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Synthesizes a short fixed sentence with the named TTS engine and plays it.
/// Bypasses selection capture and action history — this is a settings preview,
/// not a read-aloud run.
///
/// One preview at a time: playback blocks a thread from the blocking pool and
/// mixes with whatever is already playing, so rapid clicks would overlay
/// voices and pile up threads. A second request is refused with a message
/// rather than silently overlapping or cutting the first one off.
#[tauri::command]
pub async fn preview_voice(
    state: State<'_, Arc<AppState>>,
    engine: String,
    model: Option<String>,
    voice: Option<String>,
    provider_ref: Option<String>,
) -> Result<(), String> {
    const PREVIEW_SENTENCE: &str = "Hi! This is how this voice sounds when reading aloud.";
    let _guard = PreviewGuard::try_acquire(&state.preview_playing)
        .ok_or_else(|| "A voice preview is already playing — wait for it to finish.".to_string())?;
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
                // Carry the caller's provider through: the cloud TTS engine is
                // registered once against the built-in "openai" ref, so a
                // preview of a voice bound to a user-added provider would
                // otherwise read the wrong key and fail as "missing api key"
                // while the real read-aloud run succeeded.
                provider_ref,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let frame = PcmFrame {
        samples: pcm.samples,
        sample_rate_hz: pcm.sample_rate_hz,
    };
    tokio::task::spawn_blocking(move || kea_platform::audio::playback::play_pcm_blocking(&frame))
        .await
        .map_err(|e| format!("playback failed: {e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_onnx_models(kind: String) -> Result<Vec<OnnxModelEntry>, String> {
    onnx_catalog_for_kind(parse_model_kind(&kind)?)
}

#[tauri::command]
pub fn list_installed_onnx_models(
    state: State<'_, Arc<AppState>>,
    kind: String,
) -> Result<Vec<String>, String> {
    let kind = parse_model_kind(&kind)?;
    let storage = onnx_storage_for(&state, kind)?;
    let catalog = onnx_catalog_for_kind(kind)?;
    Ok(installed_onnx_model_ids(storage, &catalog))
}

#[tauri::command]
pub async fn download_onnx_model(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model_id: String,
    app: AppHandle,
) -> Result<(), String> {
    let kind = parse_model_kind(&kind)?;
    let storage = ModelStorage::new(onnx_storage_for(&state, kind)?.root.clone());
    let entry = onnx_catalog_for_kind(kind)?
        .into_iter()
        .find(|e| e.id == model_id)
        .ok_or_else(|| format!("unknown {kind} model: {model_id}"))?;

    let temp_path = temp_file_for(&storage.onnx_dir_for(&model_id));
    let downloader = new_model_downloader(storage);
    let task_entry = entry.clone();

    start_download(
        state.inner(),
        &app,
        DownloadRequest {
            kind,
            model_id: &model_id,
            temp_path,
            busy_message: format!("download of '{model_id}' ({kind}) already in progress"),
        },
        move |app| async move {
            downloader
                .download_onnx(&task_entry, |progress| {
                    emit_model_download_progress(&app, &progress);
                })
                .await
        },
    )?;
    tracing::info!(
        model = %model_id,
        kind = %kind,
        bytes = entry.size_bytes,
        url = %entry.url,
        "model download started"
    );
    Ok(())
}

/// Stops an in-flight download and clears the partial file.
///
/// This is the user's way out of a transfer they no longer want — a slow one,
/// or one the UI is still showing as pending. It is deliberately forgiving:
/// cancelling something that is already gone still emits the terminal event,
/// because a UI stuck on a download the backend has forgotten is exactly the
/// state that needs unsticking.
#[tauri::command]
pub async fn cancel_model_download(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model_id: String,
    app: AppHandle,
) -> Result<(), String> {
    let kind = parse_model_kind(&kind)?;
    let key = kind.download_key(&model_id);
    let active = {
        let mut guard = state.active_downloads.lock().map_err(|e| e.to_string())?;
        guard.remove(&key)
    };

    match active {
        Some(active) => {
            active.task.abort();
            // The aborted task is dropped mid-write, so nothing else will
            // remove what it had already streamed.
            let _ = std::fs::remove_file(&active.temp_path);
            tracing::info!(model = %active.model_id, kind = %kind, "model download cancelled");
        }
        None => {
            tracing::info!(
                model = %model_id,
                kind = %kind,
                "cancel for a download that was no longer running"
            );
        }
    }

    emit_model_download_error(&app, &model_id, "download cancelled");
    Ok(())
}

/// Validates an IPC-supplied model id against the kind's catalog before any
/// filesystem call — the id becomes a path component, so an unknown id
/// (including any traversal attempt) must be rejected (pure, unit-testable).
pub fn validate_model_id_for_delete(kind: ModelKind, model_id: &str) -> Result<(), String> {
    if ModelRegistry::find(kind, model_id).is_some() {
        Ok(())
    } else {
        Err(format!("unknown {kind} model: {model_id}"))
    }
}

/// Removes an installed model's files (whisper .gguf file or onnx dir). Every
/// binding that referenced the removed model — the capability default and any
/// per-feature override — is dropped too, along with the settings-level
/// fallback model, so nothing keeps pointing at deleted files. The UI confirms
/// with the user before calling this.
#[tauri::command]
pub async fn delete_model(
    state: State<'_, Arc<AppState>>,
    kind: String,
    model_id: String,
) -> Result<(), String> {
    let kind = parse_model_kind(&kind)?;
    let default_slot = kind.default_slot();
    validate_model_id_for_delete(kind, &model_id)?;
    match kind {
        ModelKind::Whisper => state.model_storage.remove_model(&model_id),
        _ => onnx_storage_for(&state, kind)?.remove_onnx(&model_id),
    }
    .map_err(|e| format!("failed to remove model files: {e}"))?;

    let bindings = BindingRepo::new(state.config_pool.clone());
    let cleared = clear_bindings_for_model(&bindings, default_slot, &model_id).await?;
    if !cleared.is_empty() {
        tracing::info!(
            model = %model_id,
            slot = %default_slot,
            features = %cleared.join(", "),
            "cleared bindings referencing the deleted model"
        );
    }
    if clear_active_model_for_deleted(&state.config_pool, default_slot, &model_id).await? {
        tracing::info!(model = %model_id, slot = %default_slot, "cleared active_model setting");
    }
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
    use kea_core::resolve::DEFAULT_FEATURE_ID;
    use kea_core::rewrite::CredentialSourceAdapter;
    use kea_core::secrets::{CredentialStore, InMemoryCredentialStore};
    use kea_core::store::db::{open_pool, run_config_migrations};
    use kea_core::store::settings::SettingsRepo;
    use kea_engines::{
        noop::NoopLlmEngine, register_phase1_engines, register_phase2_stt_engines,
        register_phase4_tts_engines, EngineRegistry, ReqwestHttpClient,
    };
    use kea_features::{DictationFeature, Feature, MeetingFeature, RewriteFeature, TtsFeature};
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
        assert!(resolution_error(Resolution::Bound(Binding {
            engine_id: "openai".into(),
            model: None,
            provider_ref: None,
        }))
        .is_none());
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

    #[test]
    fn validate_model_id_for_delete_accepts_catalog_ids_only() {
        let whisper_id = ModelRegistry::whisper_catalog()[0].id.clone();
        assert!(validate_model_id_for_delete(ModelKind::Whisper, &whisper_id).is_ok());
        let tts_id = ModelRegistry::tts_catalog()[0].id.clone();
        assert!(validate_model_id_for_delete(ModelKind::Tts, &tts_id).is_ok());

        // unknown ids are rejected before any filesystem call
        assert!(validate_model_id_for_delete(ModelKind::Whisper, "nonexistent").is_err());
        assert!(validate_model_id_for_delete(ModelKind::Parakeet, "nonexistent").is_err());

        // traversal attempts are never catalog ids
        assert!(validate_model_id_for_delete(ModelKind::Whisper, "../../etc/passwd").is_err());
        assert!(validate_model_id_for_delete(ModelKind::Tts, "/etc/passwd").is_err());

        // unknown kinds never reach here — they are rejected at the IPC boundary
        assert!(parse_model_kind("bogus").is_err());
    }

    #[tokio::test]
    async fn load_custom_providers_survives_corrupt_value() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool);
        settings
            .set(CUSTOM_PROVIDERS_KEY, &"not a provider list")
            .await
            .unwrap();

        let got = load_custom_providers(&settings).await.unwrap();
        assert!(
            got.is_empty(),
            "corrupt value should yield no custom providers"
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
        let ok = provider_test_result_for_response(200, true, "");
        assert!(ok.ok);
        assert_eq!(ok.message, "Connected");

        // A 401 with no key read is a missing key, not a bad one — the two
        // send the user looking in completely different places.
        let missing = provider_test_result_for_response(401, false, "");
        assert!(!missing.ok);
        assert!(missing.message.contains("No API key saved"));

        let rejected = provider_test_result_for_response(403, true, "");
        assert!(!rejected.ok);
        assert!(rejected.message.contains("Server rejected the key"));

        let other = provider_test_result_for_response(500, true, "");
        assert!(!other.ok);
        assert!(other.message.contains("500"));
    }

    #[test]
    fn provider_test_surfaces_the_server_explanation() {
        // Real OpenWebUI bodies: the status is identical, the cause is not.
        let no_token =
            provider_test_result_for_response(401, false, r#"{"detail":"Not authenticated"}"#);
        assert!(no_token.message.contains("Not authenticated"));

        let bad_token = provider_test_result_for_response(
            401,
            true,
            r#"{"detail":"Your session has expired or the token is invalid."}"#,
        );
        assert!(bad_token.message.contains("token is invalid"));

        // OpenAI-shaped errors unwrap too.
        let openai = provider_test_result_for_response(
            401,
            true,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        assert!(openai.message.contains("Incorrect API key provided"));

        // A wrong base URL usually means a 404, and should say so.
        let missing_route = provider_test_result_for_response(404, true, "");
        assert!(missing_route.message.contains("base URL"));
    }

    #[test]
    fn server_detail_ignores_html_and_caps_length() {
        assert_eq!(server_detail("<!DOCTYPE html><html>…"), None);
        assert_eq!(server_detail("   "), None);
        assert_eq!(
            server_detail("plain text reason").as_deref(),
            Some("plain text reason")
        );
        let long = "x".repeat(500);
        assert_eq!(server_detail(&long).unwrap().chars().count(), 160);
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

    #[test]
    fn validate_new_provider_trims_before_checking_and_storing() {
        let existing = vec![CustomProvider {
            provider_ref: "groq".into(),
            name: "Groq".into(),
        }];
        // Padding must not smuggle a ref past the built-in / duplicate checks.
        assert!(validate_new_provider(" openai", "Name", &existing).is_err());
        assert!(validate_new_provider("groq ", "Name", &existing).is_err());

        let stored = validate_new_provider("  mistral  ", "  Mistral  ", &existing).unwrap();
        assert_eq!(stored.provider_ref, "mistral");
        assert_eq!(stored.name, "Mistral");
    }

    #[test]
    fn validate_new_provider_restricts_ref_charset() {
        // The ref feeds settings keys and keychain account names.
        for bad in [
            "My Provider",
            "OpenAI",
            "prov/ider",
            "prov:ider",
            "../escape",
            "prov\"ider",
            "prövider",
        ] {
            assert!(
                validate_new_provider(bad, "Name", &[]).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
        for good in ["groq", "my-server", "my_server", "v1.2", "llama3"] {
            assert!(
                validate_new_provider(good, "Name", &[]).is_ok(),
                "expected {good:?} to be accepted"
            );
        }
    }

    #[test]
    fn cleartext_key_warning_only_for_http_with_a_key() {
        assert!(sends_key_in_cleartext("http://localhost:1234/v1", true));
        assert!(sends_key_in_cleartext("HTTP://Localhost/v1", true));
        // No key on the wire, or TLS: nothing to warn about.
        assert!(!sends_key_in_cleartext("http://localhost:1234/v1", false));
        assert!(!sends_key_in_cleartext("https://api.openai.com/v1", true));
        // "https" must not be matched by a sloppy prefix check.
        assert!(!sends_key_in_cleartext("https://http.example.com/v1", true));
    }

    #[test]
    fn cleartext_warning_keeps_the_ok_verdict() {
        let warned = with_cleartext_warning(provider_test_result_for_response(200, true, ""));
        assert!(warned.ok, "a warning must not fail an otherwise good test");
        assert!(warned.message.starts_with("Connected"));
        assert!(warned.message.contains(CLEARTEXT_KEY_WARNING));

        let failed = with_cleartext_warning(provider_test_result_for_response(401, true, ""));
        assert!(!failed.ok);
        assert!(failed.message.contains("Server rejected the key"));
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
    async fn clear_bindings_only_when_model_matches() {
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
            clear_bindings_for_model(&bindings, "tts", "vits-piper-en-us-amy-low")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(bindings
            .get(DEFAULT_FEATURE_ID, "tts")
            .await
            .unwrap()
            .is_some());

        // The referenced model clears it.
        assert_eq!(
            clear_bindings_for_model(&bindings, "tts", "vits-piper-en-us-lessac-medium")
                .await
                .unwrap(),
            vec![DEFAULT_FEATURE_ID.to_string()]
        );
        assert!(bindings
            .get(DEFAULT_FEATURE_ID, "tts")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn clear_bindings_covers_feature_overrides_not_just_the_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let bindings = kea_core::store::bindings::BindingRepo::new(pool);
        for feature in [
            DEFAULT_FEATURE_ID,
            DICTATION_FEATURE_ID,
            MEETINGS_FEATURE_ID,
        ] {
            bindings
                .set(
                    feature,
                    "stt",
                    Binding {
                        engine_id: "whisper".into(),
                        model: Some("ggml-base.en".into()),
                        provider_ref: None,
                    },
                )
                .await
                .unwrap();
        }
        // An override on a model that stays installed must survive.
        bindings
            .set(
                TTS_FEATURE_ID,
                "tts",
                Binding {
                    engine_id: "sherpa-tts".into(),
                    model: Some("ggml-base.en".into()),
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        let cleared = clear_bindings_for_model(&bindings, "stt", "ggml-base.en")
            .await
            .unwrap();

        assert_eq!(
            cleared,
            vec![
                DEFAULT_FEATURE_ID.to_string(),
                DICTATION_FEATURE_ID.to_string(),
                MEETINGS_FEATURE_ID.to_string()
            ]
        );
        for feature in [
            DEFAULT_FEATURE_ID,
            DICTATION_FEATURE_ID,
            MEETINGS_FEATURE_ID,
        ] {
            assert!(
                bindings.get(feature, "stt").await.unwrap().is_none(),
                "{feature}/stt should have been cleared"
            );
        }
        // Same model id, different capability slot: untouched.
        assert!(bindings.get(TTS_FEATURE_ID, "tts").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn clear_active_model_drops_only_the_deleted_model() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let dictation = DictationSettingsRepo::new(SettingsRepo::new(pool.clone()));
        dictation
            .set(&DictationSettings {
                post_process: true,
                active_model: Some("ggml-base.en".into()),
                hold_to_talk: false,
                input_device: None,
                preroll: true,
            })
            .await
            .unwrap();
        let tts = TtsSettingsRepo::new(SettingsRepo::new(pool.clone()));
        tts.set(&TtsSettings {
            active_voice: Some("alloy".into()),
            active_model: Some("vits-piper-en-us-amy-low".into()),
        })
        .await
        .unwrap();

        // A model that isn't the active one leaves the setting alone.
        assert!(!clear_active_model_for_deleted(&pool, "stt", "ggml-small")
            .await
            .unwrap());
        assert_eq!(
            dictation.get().await.unwrap().active_model.as_deref(),
            Some("ggml-base.en")
        );

        // The active one is cleared, and unrelated fields survive.
        assert!(clear_active_model_for_deleted(&pool, "stt", "ggml-base.en")
            .await
            .unwrap());
        let after = dictation.get().await.unwrap();
        assert_eq!(after.active_model, None);
        assert!(after.post_process, "post_process must not be disturbed");

        assert!(
            clear_active_model_for_deleted(&pool, "tts", "vits-piper-en-us-amy-low")
                .await
                .unwrap()
        );
        let after_tts = tts.get().await.unwrap();
        assert_eq!(after_tts.active_model, None);
        assert_eq!(after_tts.active_voice.as_deref(), Some("alloy"));

        // Unknown slots are a no-op rather than an error.
        assert!(!clear_active_model_for_deleted(&pool, "llm", "gpt-4o")
            .await
            .unwrap());
    }

    #[test]
    fn preview_guard_admits_one_at_a_time_and_releases_on_drop() {
        let flag = AtomicBool::new(false);
        let first = PreviewGuard::try_acquire(&flag).expect("first preview should start");
        assert!(
            PreviewGuard::try_acquire(&flag).is_none(),
            "a second preview must be refused while one is playing"
        );
        drop(first);
        assert!(
            PreviewGuard::try_acquire(&flag).is_some(),
            "the slot must be free again once the preview finishes"
        );
    }

    fn capability_registry() -> EngineRegistry {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_stt(Arc::new(kea_engines::noop::NoopSttEngine));
        reg.register_tts(Arc::new(kea_engines::noop::NoopTtsEngine));
        reg
    }

    #[test]
    fn slot_capability_is_derived_from_the_slot_name() {
        assert_eq!(capability_for_slot("llm"), Some(CapKind::Llm));
        assert_eq!(capability_for_slot("stt"), Some(CapKind::Stt));
        assert_eq!(capability_for_slot("tts"), Some(CapKind::Tts));
        assert_eq!(capability_for_slot("summarize"), None);
    }

    #[test]
    fn engine_validation_is_scoped_to_the_slot_capability() {
        let reg = capability_registry();
        assert!(validate_engine_for_slot(&reg, "llm", "noop").is_ok());
        assert!(validate_engine_for_slot(&reg, "stt", "noop-stt").is_ok());
        assert!(validate_engine_for_slot(&reg, "tts", "noop-tts").is_ok());

        // A tts engine on the llm slot resolves to nothing: reject it.
        let err = validate_engine_for_slot(&reg, "llm", "noop-tts").unwrap_err();
        assert!(err.contains("noop-tts"), "got: {err}");
        assert!(err.contains("text"), "got: {err}");
        assert!(validate_engine_for_slot(&reg, "stt", "noop").is_err());
        assert!(validate_engine_for_slot(&reg, "tts", "noop-stt").is_err());
    }

    #[test]
    fn engine_validation_rejects_unknown_ids_and_keeps_custom_slots_working() {
        let reg = capability_registry();
        assert_eq!(
            validate_engine_for_slot(&reg, "stt", "ghost").unwrap_err(),
            "unknown engine id: ghost"
        );
        assert_eq!(
            validate_engine_for_slot(&reg, "whatever", "ghost").unwrap_err(),
            "unknown engine id: ghost"
        );
        // Unknown slots keep the old "registered anywhere" rule.
        assert!(validate_engine_for_slot(&reg, "whatever", "noop-tts").is_ok());
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

    /// The descriptor for a pair that must be in the table.
    fn action_for(feature: &str, command: &str) -> HotkeyAction {
        hotkey_action(feature, command).expect("a known global-hotkey pair")
    }

    /// Asserts the feature itself declares `expected` for `command`, and that
    /// the shell reads back exactly that. The two used to be separate copies of
    /// the same `cfg` block, and nothing bound them together.
    fn assert_default_accelerator(f: &dyn Feature, command: &str, expected: &str) {
        let cmd = f
            .commands()
            .into_iter()
            .find(|c| c.id == command)
            .expect("declared command");
        assert_eq!(cmd.default_accelerator.as_deref(), Some(expected));
        assert_eq!(
            compiled_default_accelerator(f.id(), command).as_deref(),
            Some(expected)
        );
    }

    #[cfg(target_os = "macos")]
    const EXPECTED_MODIFIERS: &str = "Cmd+Shift";
    #[cfg(not(target_os = "macos"))]
    const EXPECTED_MODIFIERS: &str = "CommandOrControl+Shift";

    #[test]
    fn rewrite_feature_default_hotkey_matches_the_shell() {
        assert_default_accelerator(
            &RewriteFeature,
            REWRITE_COMMAND_ID,
            &format!("{EXPECTED_MODIFIERS}+R"),
        );
    }

    #[tokio::test]
    async fn resolve_accelerator_falls_back_to_the_features_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        for action in HOTKEY_ACTIONS {
            assert_eq!(
                resolve_accelerator(&pool, &action).await,
                compiled_default_accelerator(action.feature, action.command).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn resolve_accelerator_reads_db() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        HotkeyBindingRepo::new(pool.clone())
            .set(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K")
            .await
            .unwrap();
        let action = action_for(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID);
        assert_eq!(resolve_accelerator(&pool, &action).await, "Alt+K");
    }

    #[tokio::test]
    async fn translate_falls_back_to_a_target_when_none_is_saved() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        SettingsRepo::new(pool.clone())
            .set("rewrite.active_mode", &"translate".to_string())
            .await
            .unwrap();

        let input = default_rewrite_input(&pool).await;

        assert_eq!(input.mode, RewriteMode::Translate);
        assert!(
            input.custom_instruction.is_some(),
            "translate cannot render its prompt without a target, so the default \
             must supply one rather than leaving the run to fail"
        );
    }

    /// The descriptor is what makes a mode's parameter reach the prompt. Ask
    /// and Translate read different settings keys, and reading the wrong one
    /// is silent: the prompt renders with an empty parameter.
    #[tokio::test]
    async fn each_mode_reads_its_own_parameter_key() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool.clone());
        settings
            .set("rewrite.custom_instruction", &"make it rhyme".to_string())
            .await
            .unwrap();
        settings
            .set("rewrite.translate.target", &"fr".to_string())
            .await
            .unwrap();

        settings
            .set("rewrite.active_mode", &"ask_kea".to_string())
            .await
            .unwrap();
        assert_eq!(
            default_rewrite_input(&pool)
                .await
                .custom_instruction
                .as_deref(),
            Some("make it rhyme")
        );

        settings
            .set("rewrite.active_mode", &"translate".to_string())
            .await
            .unwrap();
        assert_eq!(
            default_rewrite_input(&pool)
                .await
                .custom_instruction
                .as_deref(),
            Some("fr")
        );

        settings
            .set("rewrite.active_mode", &"improve".to_string())
            .await
            .unwrap();
        assert_eq!(
            default_rewrite_input(&pool).await.custom_instruction,
            None,
            "a mode with no parameter must not inherit another mode's"
        );
    }

    #[test]
    fn hotkey_actions_cover_every_declaring_feature_exactly_once() {
        // The table and the features are the same four rows: each descriptor
        // names a command its feature actually declares, and no pair repeats.
        for action in HOTKEY_ACTIONS {
            assert!(
                feature_registry()
                    .find_command(action.feature, action.command)
                    .is_some(),
                "{}/{} is not declared by its feature",
                action.feature,
                action.command
            );
            assert_eq!(
                action.action_id,
                format!("{}:{}", action.feature, action.command)
            );
        }
        let mut pairs: Vec<_> = HOTKEY_ACTIONS
            .iter()
            .map(|a| (a.feature, a.command))
            .collect();
        pairs.sort();
        pairs.dedup();
        assert_eq!(pairs.len(), HOTKEY_ACTIONS.len());
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
        let bindings = vec![binding_row(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            "Cmd+Shift+D",
        )];
        let hit = check_hotkey_collision(
            DICTATION_FEATURE_ID,
            DICTATION_COMMAND_ID,
            "Cmd+Shift+D",
            &bindings,
        );
        assert_eq!(
            hit,
            Some(format!("{REWRITE_FEATURE_ID}/{REWRITE_COMMAND_ID}"))
        );
    }

    #[test]
    fn collision_detects_other_features_default_accelerator() {
        // No DB row for meetings → its compiled default is Cmd+Shift+M.
        // Dictation can't claim Cmd+Shift+M even if meetings has never been customized.
        let bindings: Vec<HotkeyBindingRow> = vec![];
        let hit = check_hotkey_collision(
            DICTATION_FEATURE_ID,
            DICTATION_COMMAND_ID,
            &compiled_default_accelerator(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID).unwrap(),
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
        let bindings = vec![binding_row(
            REWRITE_FEATURE_ID,
            REWRITE_COMMAND_ID,
            "Cmd+Shift+R",
        )];
        let hit =
            check_hotkey_collision(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K", &bindings);
        assert_eq!(hit, None);
    }

    #[test]
    fn collision_alias_spelling_matches_other_feature() {
        // Dictation has Cmd+Shift+R in DB; rewrite tries "CommandOrControl+Shift+R"
        // which parses to the same id on macOS → collision.
        #[cfg(target_os = "macos")]
        {
            let bindings = vec![binding_row(
                DICTATION_FEATURE_ID,
                DICTATION_COMMAND_ID,
                "Cmd+Shift+R",
            )];
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
    fn a_hold_starts_only_from_idle_and_a_release_stops_only_while_listening() {
        assert_eq!(
            hold_dictation_action(HoldAction::Start, DictationState::Idle, false, false),
            DictationHotkeyAction::Start
        );
        assert_eq!(
            hold_dictation_action(HoldAction::Stop, DictationState::Listening, false, false),
            DictationHotkeyAction::Stop
        );
    }

    #[test]
    fn a_hold_edge_never_toggles_the_other_way() {
        // The chord going down while a recording is already running must not
        // stop it, and coming up while idle must not start one. The accelerator
        // is a toggle; this is not, and routing it through the same rules would
        // otherwise invert on a missed edge.
        assert_eq!(
            hold_dictation_action(HoldAction::Start, DictationState::Listening, false, false),
            DictationHotkeyAction::Ignore
        );
        assert_eq!(
            hold_dictation_action(HoldAction::Stop, DictationState::Idle, false, false),
            DictationHotkeyAction::Ignore
        );
    }

    #[test]
    fn a_hold_defers_to_meetings_and_to_a_run_still_finishing() {
        assert_eq!(
            hold_dictation_action(HoldAction::Start, DictationState::Idle, true, false),
            DictationHotkeyAction::Ignore,
            "a meeting owns the microphone"
        );
        assert_eq!(
            hold_dictation_action(HoldAction::Start, DictationState::Idle, false, true),
            DictationHotkeyAction::Ignore,
            "the previous transcript is still being inserted"
        );
        assert_eq!(
            hold_dictation_action(HoldAction::Stop, DictationState::Processing, false, false),
            DictationHotkeyAction::Ignore
        );
    }

    #[test]
    fn arming_never_reaches_the_dictation_toggle() {
        // The microphone opens and closes on these, but no run starts or
        // stops — and the `_ => Ignore` shape of this mapping is exactly the
        // kind that would silently swallow a mistake here.
        for event in [HoldAction::Arm, HoldAction::Disarm] {
            for current in [
                DictationState::Idle,
                DictationState::Listening,
                DictationState::Locked,
                DictationState::Processing,
            ] {
                assert_eq!(
                    hold_dictation_action(event, current, false, false),
                    DictationHotkeyAction::Ignore,
                    "{event:?} in {current:?} must not touch the run"
                );
            }
        }
    }

    #[test]
    fn a_double_tap_starts_a_locked_run() {
        assert_eq!(
            hold_dictation_action(HoldAction::StartLocked, DictationState::Idle, false, false),
            DictationHotkeyAction::StartLocked
        );
    }

    #[test]
    fn locking_inherits_the_rules_about_when_dictation_may_run() {
        assert_eq!(
            hold_dictation_action(HoldAction::StartLocked, DictationState::Idle, true, false),
            DictationHotkeyAction::Ignore,
            "a meeting owns the microphone; a double tap cannot take it"
        );
        assert_eq!(
            hold_dictation_action(HoldAction::StartLocked, DictationState::Idle, false, true),
            DictationHotkeyAction::Ignore,
            "the previous transcript is still being inserted"
        );
        assert_eq!(
            hold_dictation_action(
                HoldAction::StartLocked,
                DictationState::Listening,
                false,
                false
            ),
            DictationHotkeyAction::Ignore,
            "a hold is already recording; locking on top of it would start a second run"
        );
    }

    #[test]
    fn a_locked_run_stops_and_cancels_only_while_it_is_running() {
        assert_eq!(
            hold_dictation_action(HoldAction::StopLocked, DictationState::Locked, false, false),
            DictationHotkeyAction::Stop,
            "the tap that ends a lock transcribes what it captured"
        );
        assert_eq!(
            hold_dictation_action(
                HoldAction::CancelLocked,
                DictationState::Locked,
                false,
                false
            ),
            DictationHotkeyAction::Cancel,
            "Escape throws the audio away"
        );
        // The hard cap and Escape can both arrive after the run ended some
        // other way; neither may reach into the next one.
        for event in [HoldAction::StopLocked, HoldAction::CancelLocked] {
            assert_eq!(
                hold_dictation_action(event, DictationState::Idle, false, false),
                DictationHotkeyAction::Ignore,
                "{event:?} with nothing running"
            );
        }
    }

    #[test]
    fn the_accelerator_can_stop_a_locked_run() {
        // Otherwise the lock would be the one state with no way out but the
        // chord that started it.
        assert_eq!(
            dictation_hotkey_action(DictationState::Locked, false, false),
            DictationHotkeyAction::Stop
        );
        assert_eq!(
            dictation_hotkey_action(DictationState::Locked, true, false),
            DictationHotkeyAction::Ignore,
            "a meeting still silences the dictation hotkey"
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
    fn dictation_feature_default_hotkey_matches_the_shell() {
        assert_default_accelerator(
            &DictationFeature,
            DICTATION_COMMAND_ID,
            &format!("{EXPECTED_MODIFIERS}+D"),
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
    fn meeting_feature_default_hotkey_matches_the_shell() {
        assert_eq!(MeetingFeature.id(), MEETINGS_FEATURE_ID);
        assert_default_accelerator(
            &MeetingFeature,
            MEETINGS_COMMAND_ID,
            &format!("{EXPECTED_MODIFIERS}+M"),
        );
    }

    #[tokio::test]
    async fn resolve_accelerator_reads_db_for_meetings() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        HotkeyBindingRepo::new(pool.clone())
            .set(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID, "Cmd+Shift+N")
            .await
            .unwrap();
        let action = action_for(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID);
        assert_eq!(resolve_accelerator(&pool, &action).await, "Cmd+Shift+N");
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
    fn tts_feature_default_hotkey_matches_the_shell() {
        assert_eq!(TtsFeature.id(), TTS_FEATURE_ID);
        assert_default_accelerator(
            &TtsFeature,
            TTS_COMMAND_ID,
            &format!("{EXPECTED_MODIFIERS}+T"),
        );
    }

    #[test]
    fn onnx_catalog_for_kind_returns_parakeet_and_tts() {
        let parakeet = onnx_catalog_for_kind(ModelKind::Parakeet).unwrap();
        assert!(!parakeet.is_empty());
        let tts = onnx_catalog_for_kind(ModelKind::Tts).unwrap();
        assert!(!tts.is_empty());
        // Whisper is a single ggml file, not an ONNX bundle, and unknown kind
        // strings never get this far — they are rejected at the IPC boundary.
        assert!(onnx_catalog_for_kind(ModelKind::Whisper).is_err());
        assert!(parse_model_kind("unknown").is_err());
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
    fn parse_perm_kind_accepts_every_ui_kind() {
        assert_eq!(parse_perm_kind("microphone"), Ok(PermKind::Microphone));
        assert_eq!(
            parse_perm_kind("screen_recording"),
            Ok(PermKind::ScreenRecording)
        );
        assert_eq!(
            parse_perm_kind("accessibility"),
            Ok(PermKind::Accessibility)
        );
        assert!(parse_perm_kind("camera").is_err());
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
        let result = effective_hotkey(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, None);
        assert_eq!(
            result,
            Some(EffectiveHotkey {
                accelerator: compiled_default_accelerator(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID)
                    .unwrap(),
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
        let result = effective_hotkey("unknown", "unknown_cmd", Some("Cmd+Y".to_string()));
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

        record_hotkey_reg_status(&mut m, "dictation", "push_to_talk", Err("conflict".into()));
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

    #[test]
    fn cues_are_enabled_by_default() {
        assert!(cues_enabled_from_setting(None));
    }

    #[test]
    fn cues_read_both_the_string_and_the_bool_shape() {
        assert!(!cues_enabled_from_setting(Some(&serde_json::json!(
            "false"
        ))));
        assert!(!cues_enabled_from_setting(Some(&serde_json::json!(false))));
        assert!(cues_enabled_from_setting(Some(&serde_json::json!("true"))));
        assert!(cues_enabled_from_setting(Some(&serde_json::json!(true))));
    }

    #[test]
    fn an_unexpected_cue_setting_value_leaves_cues_on() {
        assert!(cues_enabled_from_setting(Some(&serde_json::json!(7))));
        assert!(cues_enabled_from_setting(Some(&serde_json::Value::Null)));
    }

    #[test]
    fn a_completed_dictation_run_earns_the_success_cue() {
        assert_eq!(
            cue_for_dictation_outcome(&Ok("hello there".into())),
            Cue::Success
        );
    }

    #[test]
    fn a_failed_dictation_run_earns_the_error_cue() {
        assert_eq!(
            cue_for_dictation_outcome(&Err("no stt engine".into())),
            Cue::Error
        );
    }

    #[test]
    fn a_run_that_produced_nothing_earns_the_neutral_cue() {
        assert_eq!(cue_for_dictation_outcome(&Ok(String::new())), Cue::Cancel);
        assert_eq!(cue_for_dictation_outcome(&Ok("  \n ".into())), Cue::Cancel);
    }

    #[test]
    fn cancel_and_start_agree_on_the_download_key() {
        // A cancel that derives a different key than the start silently misses:
        // the task keeps running and the UI never escapes its pending state.
        assert_eq!(
            ModelKind::Whisper.download_key("ggml-base.en"),
            "whisper:ggml-base.en"
        );
        assert_eq!(
            ModelKind::Parakeet.download_key("parakeet-tdt-0.6b-v3"),
            "onnx:parakeet:parakeet-tdt-0.6b-v3"
        );
        assert_eq!(
            ModelKind::Tts.download_key("vits-piper"),
            "onnx:tts:vits-piper"
        );
        // Same id under two kinds must not collide.
        assert_ne!(
            ModelKind::Parakeet.download_key("shared-id"),
            ModelKind::Tts.download_key("shared-id")
        );
    }

    #[test]
    fn a_bogus_kind_is_refused_at_the_boundary_rather_than_keyed() {
        // The old string match failed open: a bogus kind still produced a key,
        // so the download started under a name no cancel could ever re-derive.
        // Both start and cancel now parse before they can key anything.
        let error = parse_model_kind("bogus").expect_err("bogus kind must not parse");
        assert!(
            error.contains("unknown model kind: bogus"),
            "unexpected message: {error}"
        );
        // Every kind that does parse keys identically on both sides.
        for kind in ["whisper", "parakeet", "tts"] {
            let parsed = parse_model_kind(kind).expect("catalog kind");
            assert_eq!(
                parsed.download_key("m"),
                parse_model_kind(kind).unwrap().download_key("m")
            );
        }
    }

    #[tokio::test]
    async fn a_panicking_download_still_reports_a_terminal_outcome() {
        // A download task that ends without reporting is what stranded the
        // picker on "starting download…": the UI got no completion and no
        // error, and it refuses to re-issue while a request is pending. Every
        // exit from the task has to map to an event the UI can act on.
        let outcome = run_download_task("parakeet-tdt-0.6b-v3", async {
            panic!("boom in the unpack");
        })
        .await;

        let message = outcome.expect_err("a panicking download must report an error");
        assert!(
            message.contains("boom in the unpack"),
            "the panic reason should survive into the message: {message}"
        );
    }

    #[tokio::test]
    async fn a_failing_download_reports_the_reason() {
        let outcome = run_download_task("parakeet-tdt-0.6b-v3", async {
            Err(InferError::Other("connection reset".into()))
        })
        .await;

        assert!(
            outcome.unwrap_err().contains("connection reset"),
            "the failure reason has to reach the UI verbatim"
        );
    }

    #[tokio::test]
    async fn a_successful_download_reports_success() {
        let outcome = run_download_task("parakeet-tdt-0.6b-v3", async { Ok(()) }).await;
        assert!(outcome.is_ok(), "got {outcome:?}");
    }

    /// Serves one request: real 200 headers, a few body bytes, then silence
    /// with the socket held open — the shape of a transfer that dies without
    /// the peer ever closing the connection.
    async fn stalling_server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial")
                .await
                .unwrap();
            // Hold the connection open forever without sending the rest.
            std::future::pending::<()>().await;
        });
        format!("http://{addr}/model.tar.bz2")
    }

    #[tokio::test]
    async fn a_stalled_transfer_fails_instead_of_hanging_forever() {
        // The bug this guards: with no read timeout a transfer that stops
        // mid-body never ends, so the download task never emits a terminal
        // event and the picker sits on "starting download…" until the app is
        // restarted. A stall has to become an error the UI can act on.
        let url = stalling_server().await;
        let dir = tempfile::tempdir().unwrap();
        let transport = ReqwestDownloadTransport::with_timeouts(
            Duration::from_millis(500),
            Duration::from_millis(200),
        );

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            transport.fetch_to_file(&url, &dir.path().join("model.tmp"), &|_, _| {}),
        )
        .await;

        let result = outcome.expect("fetch_to_file hung past its read timeout");
        assert!(
            result.is_err(),
            "a stalled transfer must surface as an error, got {result:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_instead_of_hanging_forever() {
        // Same contract on the other end of the transfer: a connect that
        // never completes must give up rather than pin the download task.
        let dir = tempfile::tempdir().unwrap();
        let transport = ReqwestDownloadTransport::with_timeouts(
            Duration::from_millis(200),
            Duration::from_millis(200),
        );

        // 203.0.113.0/24 is TEST-NET-3: reserved for documentation, so
        // nothing answers and the SYN goes unacknowledged.
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            transport.fetch_to_file(
                "http://203.0.113.1/model.tar.bz2",
                &dir.path().join("model.tmp"),
                &|_, _| {},
            ),
        )
        .await;

        assert!(
            outcome
                .expect("fetch_to_file hung past its connect timeout")
                .is_err(),
            "an unreachable host must surface as an error"
        );
    }
}
