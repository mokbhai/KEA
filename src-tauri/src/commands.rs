use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use kea_core::app_context::{resolve_profile, AppProfile, ProfileQuery};
use kea_core::dictation::{apply_vocabulary, hint_terms, DictationSettings, DictationSettingsRepo};
use kea_core::log::{current_log_path, tail_log_file};
use kea_core::meetings::notion::{parse_page_id, NotionError};
use kea_core::meetings::{MeetingSettings, MeetingSettingsRepo};
use kea_core::resolve::Resolution;
use kea_core::resolve::SlotResolver;
use kea_core::rewrite::language::is_well_formed_tag;
use kea_core::rewrite::{
    build_llm_request, PaletteHistoryRepo, PresetRepo, PromptOverrideRepo, ProviderConfig,
    ProviderConfigRepo, RewriteInput, RewriteMode, RewritePreset, STORE_HISTORY_SETTING,
};
use kea_core::secrets::NOTION_TOKEN_REF;
use kea_core::store::actions::{ActionDetail, ActionRepo, ActionRow, NewAction};
use kea_core::store::app_profiles::AppProfileRepo;
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::conversations::{ConversationRepo, ConversationSummary, Message};
use kea_core::store::hotkeys::{HotkeyBindingRepo, HotkeyBindingRow};
use kea_core::store::meetings::{
    ActionItemStatus, Meeting, MeetingDetail, NewActionItem, TitleSource,
};
use kea_core::store::rates::{priced, LlmRate, RateRepo, UsageSpend};
use kea_core::store::settings::SettingsRepo;
use kea_core::store::usage::{UsageDay, UsageRepo};
use kea_core::store::vocabulary::{VocabularyEntry, VocabularyRepo};
use kea_core::transcript::{
    assign_speakers, plan_chunks, segments_from_rows, transcribe_chunks, NewTranscript,
    SubtitleFormat, SubtitleOpts, TranscribeSink, TranscriptDetail, TranscriptRepo, TranscriptRow,
    TranscriptStatus, DEFAULT_CHUNK_SECS,
};
use kea_core::tts::{TtsSettings, TtsSettingsRepo};
use kea_engines::traits::SttOpts;
use kea_engines::{EngineRegistry, TtsOpts};
use kea_features::demo::{run_ping, DemoFeature};
use kea_features::dictation::{run_dictation_with_commands, spawn_partials, DictationRunOpts};
use kea_features::meeting::{
    run_interim_notes_pass, run_meeting_stop_with, InterimSchedule, MeetingStopOptions,
};
use kea_features::rewrite::{complete_rewrite, OCR_COMMAND, PALETTE_COMMAND, UNDO_COMMAND};
use kea_features::run_rewrite_with_storage;
use kea_features::tts::run_tts_with_player;
use kea_features::ProfileOverrides;
use kea_features::{
    drain_and_stop_meeting, run_meeting_poll_segment, run_meeting_start, ActionGuard,
    ActiveMeeting, CapKind, ContentStorageOpts, DictationFeature, FeatureRegistry, MeetingFeature,
    MeetingRunContext, RewriteFeature, TranscribeFeature, TtsFeature,
};
use kea_infer::{
    temp_file_for, DownloadTransport, InferError, ModelDownloader, ModelKind, ModelRegistry,
    ModelStorage, OnnxModelEntry, StreamedFile,
};
use kea_platform::audio::{cut_points, decode_file, InputDevice};
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

use crate::palette::{
    palette_event, DeliveryOptions, PaletteDelivery, PaletteEvent, PaletteOrigin, PaletteReaction,
    PaletteState,
};

use crate::events::{
    dictation_state_wire, emit_device_fallback, emit_dictation_error, emit_dictation_level,
    emit_dictation_partial, emit_dictation_preview, emit_dictation_state, emit_meeting_error,
    emit_meeting_level, emit_meeting_notes, emit_meeting_notes_error, emit_meeting_segment,
    emit_meeting_state, emit_model_download_complete, emit_model_download_error,
    emit_model_download_progress, emit_palette_close, emit_palette_open,
    emit_transcribe_file_complete, emit_transcribe_file_error, emit_transcribe_file_progress,
    emit_transcribe_file_segment, emit_tts_state, meeting_state_wire, MeetingSegmentPayload,
    PartialThrottle, TranscribeFileProgressPayload, TranscribeFileSegmentPayload, TtsState,
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

/// The prompt palette and the screen-capture shortcut are commands of the
/// **rewrite** feature, not features of their own: all three are a rewrite
/// with an instruction, they share one `llm` slot binding, and they contend
/// for the same synthetic-keystroke path. See `RewriteFeature::commands`.
pub const PALETTE_ACTION_ID: &str = "rewrite:prompt_palette";
pub const OCR_ACTION_ID: &str = "rewrite:ocr_capture";
pub const UNDO_ACTION_ID: &str = "rewrite:undo_rewrite";

pub const MEETINGS_ACTION_ID: &str = "meetings:toggle_meeting";
pub const MEETINGS_FEATURE_ID: &str = "meetings";
pub const MEETINGS_COMMAND_ID: &str = "toggle_meeting";

/// One global hotkey: the `(feature, command)` pair the DB rows and the UI
/// address it by.
///
/// No accelerator here on purpose — the default belongs to the feature that
/// declares the command ([`kea_features::Command::default_accelerator`]) and is
/// read back through [`compiled_default_accelerator`].
///
/// `command` is a [`Cow`] for exactly one reason: the per-language translate
/// shortcuts (`translate.fr`, `translate.pt-BR`) are a command *family* whose
/// members exist only once a user has picked a language, so they cannot be
/// `&'static str`. `feature` stays static because a hotkey always belongs to a
/// compiled-in feature, and the action id is derived rather than stored — see
/// [`HotkeyAction::action_id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyAction {
    pub feature: &'static str,
    pub command: Cow<'static, str>,
}

impl HotkeyAction {
    /// A row of the fixed table.
    const fn fixed(feature: &'static str, command: &'static str) -> Self {
        Self {
            feature,
            command: Cow::Borrowed(command),
        }
    }

    /// The id the platform layer routes a press by.
    ///
    /// Derived, not stored. As a third field it was a second spelling of a
    /// fact the other two already carry — one a row could get wrong, and one a
    /// synthesized row would have had to invent. Every id the table ever had
    /// was `feature:command`; now that is a rule rather than a coincidence.
    pub fn action_id(&self) -> String {
        format!("{}:{}", self.feature, self.command)
    }

    /// The BCP-47 tag this shortcut translates into, when it is one of the
    /// per-language translate rows.
    pub fn translate_target(&self) -> Option<&str> {
        self.command.strip_prefix(TRANSLATE_COMMAND_PREFIX)
    }
}

/// Every command that owns a global hotkey.
///
/// This is the one table the hotkey paths read — startup registration,
/// `set_hotkey`, its rebind cleanup, collision detection and the
/// effective-hotkey lookup — so adding a feature hotkey is a row here rather
/// than another arm in five matches.
pub const HOTKEY_ACTIONS: [HotkeyAction; 7] = [
    HotkeyAction::fixed(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID),
    HotkeyAction::fixed(DICTATION_FEATURE_ID, DICTATION_COMMAND_ID),
    HotkeyAction::fixed(TTS_FEATURE_ID, TTS_COMMAND_ID),
    HotkeyAction::fixed(MEETINGS_FEATURE_ID, MEETINGS_COMMAND_ID),
    HotkeyAction::fixed(REWRITE_FEATURE_ID, PALETTE_COMMAND),
    HotkeyAction::fixed(REWRITE_FEATURE_ID, OCR_COMMAND),
    HotkeyAction::fixed(REWRITE_FEATURE_ID, UNDO_COMMAND),
];

/// The command-id prefix of the per-language translate shortcuts.
///
/// A hotkey row is keyed `(feature_id, command)`, so `("rewrite",
/// "translate.fr")` is a storable, listable binding with no schema change —
/// and the tag after the prefix is the shortcut's whole configuration.
///
/// The ids are *built* only in the UI (`translateCommand` in `ui/src/api.ts`),
/// which is what decides a language has a shortcut at all; everything here
/// reads them.
pub const TRANSLATE_COMMAND_PREFIX: &str = "translate.";

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
    // The fixed table is consulted first, always: a synthesized row must never
    // be able to shadow a real one, whatever a future command is named.
    if let Some(found) = HOTKEY_ACTIONS
        .iter()
        .find(|a| a.feature == feature && a.command == command)
    {
        return Some(found.clone());
    }
    translate_hotkey_action(feature, command)
}

/// The synthesized descriptor for one per-language translate shortcut.
///
/// Translate is the one open-ended hotkey family — a shortcut per language the
/// user enabled — so [`HOTKEY_ACTIONS`] cannot list its members. Admitting
/// them *here*, inside the one lookup, is what makes startup registration,
/// `set_hotkey`, its rebind cleanup, collision detection and the
/// effective-hotkey lookup all work for them without a second branch each.
///
/// The tag is validated rather than merely prefix-matched: the command id is
/// what reaches the Translate prompt as its target language, so
/// `translate.<a sentence>` must not be a bindable hotkey.
fn translate_hotkey_action(feature: &str, command: &str) -> Option<HotkeyAction> {
    if feature != REWRITE_FEATURE_ID {
        return None;
    }
    let tag = command.strip_prefix(TRANSLATE_COMMAND_PREFIX)?;
    is_well_formed_tag(tag).then(|| HotkeyAction {
        feature: REWRITE_FEATURE_ID,
        command: Cow::Owned(command.to_string()),
    })
}

/// The descriptor behind a dispatched action id.
///
/// The press stream speaks action ids while the table is keyed by
/// `(feature, command)`; this is the one place that turns one into the other,
/// so the dispatch loop admits exactly what every other hotkey path does.
pub fn hotkey_action_for_id(action_id: &str) -> Option<HotkeyAction> {
    let (feature, command) = action_id.split_once(':')?;
    hotkey_action(feature, command)
}

/// Every `(feature, command)` that owns a global hotkey right now: the fixed
/// table plus the persisted rows it cannot list — today, the per-language
/// translate shortcuts.
///
/// Startup registration and collision detection both need "all of them", and
/// they have to agree: a check that only saw the fixed table would happily
/// hand French a combo Spanish already holds.
pub fn hotkey_owners(bindings: &[HotkeyBindingRow]) -> Vec<HotkeyAction> {
    let mut owners: Vec<HotkeyAction> = HOTKEY_ACTIONS.to_vec();
    for row in bindings {
        let Some(action) = hotkey_action(&row.feature_id, &row.command) else {
            continue;
        };
        if !owners.contains(&action) {
            owners.push(action);
        }
    }
    owners
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
        reg.register(Arc::new(TranscribeFeature));
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
    /// When the next interim notes pass is due, and how many have run.
    ///
    /// Beside the poll cursor rather than on `AppState`, because every counter
    /// in it is about *this* meeting: taking the session out of the slot at
    /// stop is what retires the schedule, with nothing to reset by hand.
    pub interim: InterimSchedule,
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
const PERM_KINDS: [(&str, PermKind); 5] = [
    ("microphone", PermKind::Microphone),
    ("screen_recording", PermKind::ScreenRecording),
    ("accessibility", PermKind::Accessibility),
    ("calendar", PermKind::Calendar),
    ("speech", PermKind::Speech),
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
    ModelRegistry::onnx_catalog(kind).ok_or_else(|| {
        format!(
            "unknown onnx model kind: {kind} (expected parakeet, tts, streaming or diarization)"
        )
    })
}

pub fn installed_onnx_model_ids(storage: &ModelStorage, catalog: &[OnnxModelEntry]) -> Vec<String> {
    catalog
        .iter()
        // Through the entry's own bundle shape, not a hardcoded `tokens.txt`:
        // the diarization models have no vocabulary file and would otherwise
        // install correctly and then report themselves missing forever.
        .filter(|entry| storage.is_onnx_entry_installed(entry))
        .map(|entry| entry.id.clone())
        .collect()
}

/// Providers that ship with the app and can never be removed.
///
/// Every entry resolves its base URL and default model from
/// [`kea_engines::WELL_KNOWN_PROVIDERS`] with nothing stored, so connecting one
/// is entering a key and nothing else. A provider that needed a URL typed in
/// belongs in the custom list, not here — `local-llm` is the exception that
/// proves it, and it is keyless.
pub const BUILT_IN_PROVIDERS: [(&str, &str); 6] = [
    ("openai", "OpenAI"),
    ("anthropic", "Anthropic"),
    // Groq has no engine of its own: it is an OpenAI-shaped server, so
    // `openai-stt` and `openai-compatible` reach it through this ref.
    ("groq", "Groq"),
    ("deepgram", "Deepgram"),
    ("elevenlabs", "ElevenLabs"),
    ("local-llm", "Local server"),
];

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
    match repo.get(action.feature, &action.command).await {
        Ok(Some(row)) => row.accelerator,
        // Every row in HOTKEY_ACTIONS names a command whose feature declares a
        // default; falling back to empty keeps this total rather than panicking
        // if one is ever dropped, and registration then fails visibly.
        _ => compiled_default_accelerator(action.feature, &action.command).unwrap_or_default(),
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

/// The value of whatever parameter `mode` takes, read from that mode's own key.
///
/// Dispatches through the descriptor rather than naming a mode: `AskKea` takes
/// an instruction and `Translate` a target language, and each stores it under
/// its own settings key. A `matches!(mode, ..)` here is the shape that made
/// adding Translate mean editing four unrelated conditionals.
async fn mode_parameter_value(settings: &SettingsRepo, mode: RewriteMode) -> Option<String> {
    let parameter = mode.parameter()?;
    settings
        .get_optional::<String>(parameter.setting_key())
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Probes the frontmost app right now, honouring the opt-in capture flags.
///
/// Returns `None` when nothing could be identified, which is the same thing as
/// "no profile applies" to every caller.
pub async fn capture_app_context_now(state: &AppState) -> Option<kea_platform::AppContext> {
    let opts = capture_opts(&state.config_pool).await;
    let ctx = kea_platform::new_app_context_probe().capture(opts);
    ctx.bundle_id.is_some().then_some(ctx)
}

/// Force `input` onto `mode`, re-deriving the parameter that mode reads.
///
/// The re-derive is the part worth being careful about: a mode's parameter
/// comes from a key chosen BY that mode, so anything that forces Translate
/// must also pick up `rewrite.translate.target`. Applying the mode without
/// re-reading the parameter would hand Translate the Ask instruction, or
/// nothing at all.
///
/// Shared by the app-profile path and the `kea://`/HTTP override so the two
/// cannot answer that differently.
pub async fn set_rewrite_mode(
    input: &mut RewriteInput,
    mode: RewriteMode,
    config_pool: &SqlitePool,
) {
    // Naming a mode means asking for that mode's template, and a preset
    // replaces the template outright (`build_llm_request`). Leaving the saved
    // preset in place would quietly run it instead — which for a translate
    // shortcut means the key does not translate at all.
    input.preset_id = None;
    if mode == input.mode {
        return;
    }
    input.mode = mode;
    input.custom_instruction =
        mode_parameter_value(&SettingsRepo::new(config_pool.clone()), mode).await;
}

/// The default rewrite input with `profile`'s mode and preset applied.
pub async fn rewrite_input_for_profile(
    config_pool: &SqlitePool,
    profile: Option<&AppProfile>,
) -> RewriteInput {
    let mut input = default_rewrite_input(config_pool).await;
    let Some(profile) = profile else {
        return input;
    };
    if let Some(mode) = profile.mode() {
        set_rewrite_mode(&mut input, mode, config_pool).await;
    }
    if profile.preset_id.is_some() {
        input.preset_id = profile.preset_id.clone();
    }
    input
}

/// What a caller outside the app asked for, on top of the saved settings and
/// whatever app profile applies.
///
/// `None` everywhere means "no opinion", which is exactly what the hotkey
/// passes — so the shortcut and the two scripting surfaces run the same
/// function instead of the shortcut keeping its own copy of it.
#[derive(Debug, Clone, Default)]
pub struct RewriteOverride {
    pub mode: Option<RewriteMode>,
    pub preset_id: Option<String>,
    pub instruction: Option<String>,
}

impl RewriteOverride {
    pub async fn apply(&self, input: &mut RewriteInput, config_pool: &SqlitePool) {
        if let Some(mode) = self.mode {
            set_rewrite_mode(input, mode, config_pool).await;
        }
        if self.preset_id.is_some() {
            input.preset_id = self.preset_id.clone();
        }
        // After the mode, not before: `set_rewrite_mode` re-derives the
        // parameter from settings, and an instruction the caller spelled out
        // has to survive that.
        if self.instruction.is_some() {
            input.custom_instruction = self.instruction.clone();
        }
    }
}

/// Capture the frontmost app's selection, rewrite it, and write it back.
///
/// The body of the rewrite shortcut, shared with `kea://rewrite` and
/// `POST /v1/rewrite`. The caller owns the busy guard (`selection_busy`) and
/// whatever progress it reports; this owns the *order*, which is the part that
/// matters — the app context is probed FIRST, before anything that could
/// change which app is frontmost, because by the time the rewrite returns the
/// user may well have switched away.
pub async fn run_selection_rewrite(
    state: &Arc<AppState>,
    over: &RewriteOverride,
) -> Result<String, String> {
    let ctx = capture_app_context_now(state).await;
    // Read with the context, for the same reason: the app that owns the text
    // is the one that is frontmost *now*, and undo may be pressed from
    // somewhere else entirely.
    let target_pid = crate::macfocus::frontmost_pid();
    let profile = profile_for(&state.config_pool, ctx.as_ref()).await;
    let mut input = rewrite_input_for_profile(&state.config_pool, profile.as_ref()).await;
    over.apply(&mut input, &state.config_pool).await;
    let outcome = execute_rewrite(
        state,
        input,
        &ProfileOverrides::from_profile(profile.as_ref()),
    )
    .await?;
    offer_undo(state, &outcome, target_pid);
    Ok(outcome.text)
}

// ===========================================================================
// Undoing the last rewrite
// ===========================================================================

/// How long a rewrite stays undoable.
///
/// An undo is a reaction to *reading* the result: the user looks at what came
/// back, decides they preferred their own sentence, and reaches for the key.
/// That takes tens of seconds, so anything much shorter would expire while
/// they were still reading. Two minutes covers it with room.
///
/// It does not run longer because of what the undo actually does. The
/// selection is gone, so the only handle left is the text KEA wrote, found by
/// searching the focused field for it (see `TextIo::swap_in_focused`). That
/// search is a good guard for a minute or two and a weak one after an
/// afternoon of editing, by which time the user has typed around it, the
/// surrounding text has changed, and a stale offer firing is an edit nobody
/// asked for. The offer is also single-use and replaced by the next rewrite:
/// there is one step of undo, not a stack.
pub const UNDO_WINDOW: Duration = Duration::from_secs(120);

/// The last rewrite, while it can still be taken back.
///
/// Both halves of the swap are kept because neither can be recovered later:
/// `original` was a selection that no longer exists, and `inserted` is what
/// has to be found again in a document the user may have gone on editing.
pub struct UndoOffer {
    /// What KEA wrote into the document.
    pub inserted: String,
    /// What it replaced, and what goes back.
    pub original: String,
    /// The app to bring forward before writing. `None` when nothing could be
    /// identified, which the undo treats as "do not type anywhere".
    pub target_pid: Option<i32>,
    /// When the rewrite landed. Compared against [`UNDO_WINDOW`].
    pub made_at: std::time::Instant,
}

impl UndoOffer {
    fn is_live(&self) -> bool {
        self.made_at.elapsed() < UNDO_WINDOW
    }
}

/// Hand-written rather than derived: both strings are the user's own document
/// text, and a derive would put a sentence they wrote into any log line or
/// test failure that happened to print the offer. Lengths answer every
/// question a debug view is actually asked.
impl std::fmt::Debug for UndoOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UndoOffer")
            .field("inserted_chars", &self.inserted.chars().count())
            .field("original_chars", &self.original.chars().count())
            .field("target_pid", &self.target_pid)
            .field("live", &self.is_live())
            .finish()
    }
}

/// Records a finished rewrite as undoable, replacing any earlier offer.
///
/// See [`is_undoable`] for the rewrites that are deliberately not offered.
fn offer_undo(state: &AppState, outcome: &kea_features::RewriteOutcome, target_pid: Option<i32>) {
    if !is_undoable(outcome) {
        return;
    }
    match state.last_rewrite.lock() {
        Ok(mut slot) => {
            *slot = Some(UndoOffer {
                inserted: outcome.text.clone(),
                original: outcome.source_text.clone(),
                target_pid,
                made_at: std::time::Instant::now(),
            })
        }
        // A poisoned slot means some other thread panicked holding it; losing
        // the undo offer is the mildest possible consequence and is not worth
        // taking a rewrite down for.
        Err(e) => tracing::warn!(error = %e, "the undo slot is poisoned; not offering an undo"),
    }
}

/// Whether a finished rewrite is worth offering an undo for.
///
/// Two rewrites are not:
///
/// * one that replaced nothing — no selection, so the text went in at the
///   caret. "Undo" would have to mean *deleting* the insertion, and the swap
///   this feature is built on can only exchange one string for another; asking
///   it to delete would leave the surrounding text to guesswork. Not offering
///   beats offering something that refuses when pressed.
/// * one that changed nothing. The provider handed back exactly what it was
///   given, so there is nothing to put back, and the swap would be a no-op
///   that looked like a success.
fn is_undoable(outcome: &kea_features::RewriteOutcome) -> bool {
    !outcome.source_text.is_empty() && outcome.source_text != outcome.text
}

/// The offer a press of the undo key should act on, or why there is none.
///
/// Split out from the slot so the rule — "there has to be one, and it has to
/// still be live" — is testable without an `AppState`, and so both refusals
/// carry a message the user can act on.
fn undo_target(offer: Option<&UndoOffer>) -> Result<&UndoOffer, String> {
    let offer = offer.ok_or_else(|| "there is no recent rewrite to undo".to_string())?;
    if !offer.is_live() {
        return Err("that rewrite is too old to undo safely".into());
    }
    Ok(offer)
}

/// The live offer, cloned rather than taken.
///
/// Taken only on success (see [`undo_last_rewrite`]): a failed attempt — the
/// app would not come back, the element would not give its text up — must
/// leave the offer where it was, or one unlucky press would silently spend
/// the user's only chance to get their sentence back.
fn peek_undo_offer(state: &AppState) -> Result<(String, String, Option<i32>), String> {
    let slot = state
        .last_rewrite
        .lock()
        .map_err(|_| "KEA lost track of the last rewrite".to_string())?;
    let offer = undo_target(slot.as_ref())?;
    Ok((
        offer.inserted.clone(),
        offer.original.clone(),
        offer.target_pid,
    ))
}

fn clear_undo_offer(state: &AppState) {
    if let Ok(mut slot) = state.last_rewrite.lock() {
        *slot = None;
    }
}

/// Puts the text the last rewrite replaced back where it was.
///
/// The order matters and is the same one the palette's delivery uses: bring
/// the app forward and **wait for the activation to land** before writing
/// anything, because a write into whatever happens to be frontmost is the one
/// outcome worse than not undoing at all. Then verify — the swap refuses
/// unless the text KEA wrote is still there, exactly once — and only then
/// spend the offer.
pub async fn undo_last_rewrite(state: &Arc<AppState>) -> Result<String, String> {
    let (inserted, original, target_pid) = peek_undo_offer(state)?;

    let reactivation =
        tokio::task::spawn_blocking(move || crate::macfocus::restore_focus(target_pid))
            .await
            .map_err(|e| e.to_string())?;
    if !reactivation.can_deliver() {
        return Err(
            "KEA could not bring that app back to the front, so nothing was changed".into(),
        );
    }

    new_text_io()
        .swap_in_focused(&inserted, &original)
        .await
        .map_err(|e| e.to_string())?;
    clear_undo_offer(state);
    Ok(original)
}

/// The frontmost app's selection, as text.
///
/// A synthetic ⌘C, so every caller must already hold `selection_busy`.
pub async fn capture_selection_text() -> Result<String, String> {
    let text = new_text_io()
        .capture_selection()
        .await
        .map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("nothing is selected".into());
    }
    Ok(text)
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
    let custom_instruction = mode_parameter_value(&settings, mode).await;
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

/// Reads a boolean setting that may have been written in either encoding.
///
/// The generic `set_setting` command takes a `String` and JSON-encodes it, so
/// the UI's toggles land as the JSON string `"true"`/`"false"`, while typed
/// callers write a JSON bool. A reader that assumes one shape does not fail
/// loudly — it fails to deserialize, falls back to its default, and the toggle
/// is silently inert. That is exactly what happened to the two app-context
/// capture flags, so this is the one place that knows about both shapes.
fn bool_setting(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "true" => true,
            "false" => false,
            _ => default,
        },
        Some(other) => {
            tracing::warn!(value = %other, "unexpected boolean setting shape, using the default");
            default
        }
        None => default,
    }
}

/// [`bool_setting`] against the store, defaulting on any read error too.
pub async fn read_bool_setting(config_pool: &SqlitePool, key: &str, default: bool) -> bool {
    match SettingsRepo::new(config_pool.clone())
        .get::<serde_json::Value>(key)
        .await
    {
        Ok(value) => bool_setting(value.as_ref(), default),
        Err(e) => {
            tracing::warn!(%e, key, "failed to read a boolean setting, using the default");
            default
        }
    }
}

/// Names the streaming model used for live partial transcripts.
///
/// Absent, `null` or empty means the feature is off. There is deliberately no
/// separate on/off toggle: a selected model *is* the toggle, which removes the
/// state where partials are "enabled" but impossible.
pub const STREAMING_MODEL_SETTING: &str = "dictation.streaming_model";

/// Whether the HUD shows partials at all, default on. Read once at run start,
/// so a user who turns it off pays no IPC.
pub const SHOW_PARTIALS_SETTING: &str = "dictation.show_partials";

/// Whether a failed offline decode may insert the live draft instead of
/// nothing. Default **off** — see `DictationRunOpts::draft_fallback`.
pub const STREAMING_FALLBACK_SETTING: &str = "dictation.streaming_fallback";

/// Whether to run speaker diarization on a transcribed file.
///
/// Off by default: it needs a 36 MB download and a second inference pass over
/// the whole recording. See `diarize_if_enabled` for why the model — not
/// channel attribution — is what answers here.
pub const TRANSCRIBE_DIARIZE_SETTING: &str = "transcribe.diarization";

/// How far either side of a chunk boundary to search for a quiet frame.
///
/// Two seconds: far enough to clear a sentence, short enough that the chunks
/// stay near the target length and the progress bar stays honest.
const CHUNK_CUT_SEARCH_SECS: u32 = 2;

/// The selected streaming model, or `None` when the feature is off.
///
/// Through `get_optional`: a cleared model writes the JSON literal `null`
/// rather than removing the row, and plain `get::<String>` would fail to
/// deserialize that forever after. The empty string is treated the same way,
/// because the generic `set_setting` command is how a picker clears it.
async fn streaming_model_setting(config_pool: &SqlitePool) -> Option<String> {
    match SettingsRepo::new(config_pool.clone())
        .get_optional::<String>(STREAMING_MODEL_SETTING)
        .await
    {
        Ok(value) => value
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty()),
        Err(e) => {
            tracing::warn!(%e, "failed to read the streaming model setting; partials are off");
            None
        }
    }
}

/// Clears the streaming model setting when it names a model that was deleted.
///
/// The streaming counterpart of `clear_active_model_for_deleted`: a streaming
/// model is selected by a setting rather than a binding, so this is what
/// dangles when its files go.
pub async fn clear_streaming_model_for_deleted(
    config_pool: &SqlitePool,
    model_id: &str,
) -> Result<bool, String> {
    if streaming_model_setting(config_pool).await.as_deref() != Some(model_id) {
        return Ok(false);
    }
    SettingsRepo::new(config_pool.clone())
        .set(STREAMING_MODEL_SETTING, &Option::<String>::None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

async fn store_conversations_enabled(config_pool: &SqlitePool) -> bool {
    read_bool_setting(config_pool, "history.store_conversations", true).await
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

pub async fn execute_rewrite(
    state: &AppState,
    input: RewriteInput,
    profile: &ProfileOverrides,
) -> Result<kea_features::RewriteOutcome, String> {
    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let presets = PresetRepo::new(state.config_pool.clone());
    let overrides = PromptOverrideRepo::new(state.config_pool.clone());
    let textio = new_text_io();
    let conversations = ConversationRepo::new(state.data_pool.clone());
    let usage = UsageRepo::new(state.data_pool.clone());
    // The usage ledger is added whether or not content is stored: the counts
    // are not the user's words. See `ContentStorageOpts`.
    let storage = if store_conversations_enabled(&state.config_pool).await {
        ContentStorageOpts::enabled(&conversations)
    } else {
        ContentStorageOpts::default()
    }
    .with_usage(&usage);
    run_rewrite_with_storage(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &overrides,
        textio.as_ref(),
        input,
        profile,
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
            action.action_id(),
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
        ModelKind::Streaming => Ok(&state.streaming_storage),
        ModelKind::Diarization => Ok(&state.diarization_storage),
        ModelKind::Whisper => Err(format!("unknown onnx model kind: {kind}")),
    }
}

fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
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

/// Apply `update` to the interim schedule of `meeting_id`, if that meeting is
/// still the one recording.
///
/// The guard against a stop that raced the pass: by the time an LLM answers,
/// the slot may be empty or hold a different meeting, and recording a success
/// against that one would skip segments nobody has folded in.
fn update_interim_schedule(
    state: &AppState,
    meeting_id: &str,
    update: impl FnOnce(&mut InterimSchedule),
) {
    if let Ok(mut guard) = state.active_meeting.lock() {
        if let Some(active) = guard.as_mut() {
            if active.session.meeting_id == meeting_id {
                update(&mut active.interim);
            }
        }
    }
}

/// Fold the segments since `from_sequence` into the meeting's notes, off the
/// poll.
///
/// Spawned rather than awaited: the pass calls an LLM, and the segment poll has
/// to keep cutting and transcribing audio while it runs. It holds no audio lock
/// for the same reason — tens of seconds of provider latency must not block
/// dictation, the next segment, or the stop.
///
/// A trigger that arrives while a pass is in flight is *dropped*. Queuing would
/// turn a provider slower than the cadence into an unbounded backlog that keeps
/// billing after the meeting has ended; the next window asks again.
///
/// Nothing on this path can fail the meeting: `run_interim_notes_pass` takes no
/// `ActiveMeeting` and so cannot call `ActiveMeeting::fail`. A failed pass
/// leaves the meeting Recording with its ledger row open, and the full pass at
/// stop still writes the notes the user keeps.
fn spawn_interim_notes_pass(
    state: &Arc<AppState>,
    app: &AppHandle,
    meeting_id: String,
    from_sequence: i32,
) {
    // The same owner the hotkey handlers use to drop a press that arrives
    // mid-run, for the same reason and with the same release-on-Drop.
    let Some(guard) = try_acquire_busy(&state.interim_notes_in_flight) else {
        tracing::debug!(meeting_id = %meeting_id, "interim notes: a pass is already running");
        return;
    };
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Moved into the task so the flag is released when it ends — including
        // if it panics — rather than when this function returns.
        let _guard = guard;
        let bindings = BindingRepo::new(state.config_pool.clone());
        let usage = UsageRepo::new(state.data_pool.clone());
        let result = run_interim_notes_pass(
            &state.engines,
            &bindings,
            &state.meeting_repo,
            &meeting_id,
            from_sequence,
            Some(&usage),
        )
        .await;

        match result {
            Ok(pass) => {
                let next_sequence = pass.next_sequence;
                update_interim_schedule(&state, &meeting_id, |schedule| {
                    schedule.record_success(next_sequence)
                });
                if let Some(notes) = pass.notes {
                    emit_meeting_notes(&app, &notes);
                }
            }
            Err(e) => {
                tracing::warn!(meeting_id = %meeting_id, error = %e, "interim notes pass failed");
                // `record_failure` deliberately leaves the segment counter
                // alone: those segments still have not been folded in, and
                // clearing it would drop them from the notes for good.
                update_interim_schedule(&state, &meeting_id, InterimSchedule::record_failure);
                emit_meeting_notes_error(&app, &e);
            }
        }
    });
}

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

                // Whether this tick actually produced a segment, read before
                // the match below consumes the result: the interim cadence
                // counts segments, not ticks, and this loop runs once a second
                // whether anybody spoke or not.
                let transcribed = matches!(poll_result, Ok(Some(_)));

                // The poll cursor and the interim schedule advance under one
                // lock, held for arithmetic only — the pass itself is spawned
                // after it is dropped, so a slow provider never holds the lock
                // the stop needs to take the session out.
                let mut interim_from = None;
                if let Ok(mut guard) = state.active_meeting.lock() {
                    if let Some(active) = guard.as_mut() {
                        if active.session.meeting_id == meeting_id {
                            active.sequence = sequence;
                            active.elapsed_ms = elapsed_ms;
                            if transcribed {
                                active.interim.note_segment();
                            }
                            // The setting is read every tick rather than
                            // captured at start, so turning interim notes off
                            // mid-meeting stops the next pass rather than the
                            // one after a restart.
                            if settings.interim_notes && active.interim.is_due() {
                                interim_from = Some(active.interim.next_sequence());
                            }
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
                                speaker_key: ev.speaker_key,
                            },
                        );
                    }
                    Ok(None) => {}
                    Err(e) => emit_meeting_error(&app, &e),
                }

                if let Some(from_sequence) = interim_from {
                    spawn_interim_notes_pass(&state, &app, meeting_id, from_sequence);
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
            // Built whether or not interim notes are on: the poll consults the
            // setting on every tick, so turning it on mid-meeting starts the
            // cadence from here rather than from a schedule that was never
            // created.
            interim: InterimSchedule::new(settings.interim_cadence()),
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

    // Read here rather than carried from the start: the calendar toggle is a
    // settings row, and honouring the value it had when the meeting started
    // would mean a user who turned it off mid-meeting still got a calendar
    // title. A settings read that fails falls back to the defaults, which have
    // calendar titles off — the stop must not fail over a config read.
    let settings = MeetingSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "meeting stop: could not read meeting settings");
            MeetingSettings::default()
        });

    let detail = run_meeting_stop_with(
        &state.engines,
        &bindings,
        &actions,
        meetings,
        &session.session,
        drain_result,
        &vocabulary,
        MeetingStopOptions {
            calendar: state.calendar.clone(),
            calendar_titles: settings.calendar_titles,
            usage: Some(UsageRepo::new(state.data_pool.clone())),
        },
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

    // Probed before the microphone opens and before any KEA window can take
    // focus, so the answer is the app the user is actually dictating into.
    let app_context = capture_app_context_now(state).await;
    if let Ok(mut slot) = state.dictation_app_context.lock() {
        *slot = app_context;
    }

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

    let frame_rx;
    {
        let mut audio = state.audio.lock().await;
        if audio.state() == DictationState::Listening {
            return Err("dictation is already listening".into());
        }
        if audio.state() == DictationState::Processing {
            return Err("dictation is processing".into());
        }
        // The receiver is kept, not dropped: it is the streaming tap, and
        // dropping it is what used to close the channel and make every frame's
        // `try_send` fail.
        frame_rx = match audio.start_mic().await {
            Ok(rx) => rx,
            Err(e) => {
                emit_dictation_state(app, DictationState::Idle);
                return Err(e.to_string());
            }
        };
        report_device_fallback(app, audio.as_mut());
    }

    emit_dictation_state(app, listening_state(state));
    spawn_level_poll(state, app);
    // After the state is published, so the HUD is already on screen when the
    // first hypothesis lands. Never fails the run: with no model selected or
    // none installed, `frame_rx` is dropped inside and dictation behaves
    // exactly as it always has.
    start_partials(state, app, frame_rx);
    Ok(())
}

/// Starts the display-only streaming pass, if it is configured and possible.
///
/// Every exit is silent by design — no error cue, no HUD change, no
/// user-visible difference. Live partials are an enhancement to feedback, and
/// an enhancement that reports its own absence is worse than one that is
/// simply absent.
///
/// Spawned rather than awaited: loading the ONNX bundle takes long enough to
/// matter, and the caller holds `dictation_busy` for the whole handler — so
/// awaiting it here would mean a stop pressed during the load is *dropped*
/// rather than queued. The recording is never waiting on this: frames go to
/// the session buffer regardless, and the ones that arrive before the decoder
/// is ready are dropped from the tap and counted.
fn start_partials(
    state: &Arc<AppState>,
    app: &AppHandle,
    frames: tokio::sync::mpsc::Receiver<PcmFrame>,
) {
    let generation = state
        .dictation_partials_generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(
        async move { open_partials(&state, &app, frames, generation).await },
    );
}

async fn open_partials(
    state: &Arc<AppState>,
    app: &AppHandle,
    frames: tokio::sync::mpsc::Receiver<PcmFrame>,
    generation: u64,
) {
    let Some(model) = streaming_model_setting(&state.config_pool).await else {
        return;
    };
    if !read_bool_setting(&state.config_pool, SHOW_PARTIALS_SETTING, true).await {
        return;
    }
    let Some(engine) = state.engines.any_streaming_stt() else {
        tracing::debug!("a streaming model is selected but this build registers no engine for it");
        return;
    };

    let stream = match engine
        .open(SttOpts {
            model: Some(model.clone()),
            ..Default::default()
        })
        .await
    {
        Ok(stream) => stream,
        Err(e) => {
            tracing::debug!(model = %model, error = %e, "no live partials for this run");
            return;
        }
    };

    let (session, mut partials) = spawn_partials(stream, frames);
    // Shared with the final emit in `stop_dictation_run`, so `seq` stays
    // monotonic across the whole run and the HUD's "highest seq wins" rule
    // cannot discard the final.
    let throttle = Arc::new(Mutex::new(PartialThrottle::new()));

    let emit_app = app.clone();
    let emit_throttle = throttle.clone();
    // Channel-driven, not interval-driven: `spawn_cancellable_poll` is the
    // wrong shape here even though the cancel half would fit. The loop ends
    // when the pump drops its sender, which `PartialsSession::finish` does.
    tauri::async_runtime::spawn(async move {
        while let Some(partial) = partials.recv().await {
            let payload = emit_throttle
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .offer(&partial.text, false, std::time::Instant::now());
            if let Some(payload) = payload {
                emit_dictation_partial(&emit_app, &payload);
            }
        }
    });

    // The run may have ended — stopped, cancelled, or replaced — while the
    // bundle was loading. Its session belongs to nobody, and parking it here
    // would hand a stale draft to the next run. Dropping it is enough: the
    // pump finishes on its own when the capture channel closes.
    if state.dictation_partials_generation.load(Ordering::SeqCst) != generation {
        tracing::debug!("the run ended before the streaming decoder was ready");
        return;
    }
    if let Ok(mut slot) = state.dictation_partials.lock() {
        *slot = Some(DictationPartials { session, throttle });
    }
}

/// A running streaming pass and the throttle its partials go out through.
pub struct DictationPartials {
    session: kea_features::dictation::PartialsSession,
    throttle: Arc<Mutex<PartialThrottle>>,
}

/// Ends the streaming pass, returning its last hypothesis and the throttle the
/// final partial must go out through.
async fn take_partials(
    state: &Arc<AppState>,
) -> (Option<String>, Option<Arc<Mutex<PartialThrottle>>>) {
    // Invalidates any session still opening, for the same reason the input
    // preview carries a generation: the thing being cancelled may not exist
    // yet.
    state
        .dictation_partials_generation
        .fetch_add(1, Ordering::SeqCst);
    let taken = state
        .dictation_partials
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    match taken {
        Some(DictationPartials { session, throttle }) => (session.finish().await, Some(throttle)),
        None => (None, None),
    }
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

    // The streaming pass goes with the audio it was reading. Its draft is
    // discarded along with the recording — a cancelled run inserts nothing.
    let _ = take_partials(state).await;

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

    // Before "processing" is emitted, and before the offline decode starts, so
    // the streaming decoder's threads are gone by the time the pass that
    // produces the inserted text wants the cores.
    let (streaming_draft, partial_throttle) = take_partials(state).await;

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
    let usage = UsageRepo::new(state.data_pool.clone());
    // The usage ledger is added whether or not content is stored: the counts
    // are not the user's words. See `ContentStorageOpts`.
    let storage = if store_conversations_enabled(&state.config_pool).await {
        ContentStorageOpts::enabled(&conversations)
    } else {
        ContentStorageOpts::default()
    }
    .with_usage(&usage);

    let vocabulary = load_vocabulary(&state.config_pool).await;
    let app_context = state
        .dictation_app_context
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    let profile = profile_for(&state.config_pool, app_context.as_ref()).await;
    let profile = ProfileOverrides::from_profile(profile.as_ref());

    // Read through the repo rather than the two raw keys: it already tolerates
    // both the UI's stringified encoding and the typed one, which is the trap
    // that shipped two other toggles inert.
    let voice_commands = DictationSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .voice_commands()
        .await
        .unwrap_or_default();

    let result = run_dictation_with_commands(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &overrides,
        &mut replay,
        textio.as_ref(),
        &settings,
        &vocabulary,
        &profile,
        DictationRunOpts {
            storage,
            streaming_draft,
            draft_fallback: read_bool_setting(
                &state.config_pool,
                STREAMING_FALLBACK_SETTING,
                false,
            )
            .await,
        },
        &voice_commands,
    )
    .await;

    // The last frame in which the HUD shows the text that was actually typed.
    // Without it, the HUD's final state is a claim the app never honoured: the
    // streaming hypothesis and the offline transcript come from two different
    // decoders. Emitted before `_run_guard` drops and publishes "idle", which
    // is what clears it.
    if let (Ok(text), Some(throttle)) = (&result, partial_throttle) {
        let payload = throttle.lock().unwrap_or_else(|p| p.into_inner()).offer(
            text,
            true,
            std::time::Instant::now(),
        );
        if let Some(payload) = payload {
            emit_dictation_partial(app, &payload);
        }
    }

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

/// Probes the well-known local LLM ports and reports what answered.
///
/// Infallible on purpose: a port nobody is listening on is the *normal*
/// answer, not an error the user should have to read. An empty list means
/// "nothing found", and the UI says so.
///
/// Worst case is one probe timeout (two seconds), because the probes run
/// concurrently — see `kea_engines::discover_local_llms`.
#[tauri::command]
pub async fn discover_local_llms() -> Vec<kea_engines::LocalLlmServer> {
    kea_engines::discover_local_llms(&kea_engines::ReqwestProbe::new()).await
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

/// Reads the two opt-in capture flags.
///
/// Both default to OFF and are read with `get` rather than `get_optional`: they
/// are plain bools with a default, and an absent row means "not opted in".
async fn capture_opts(config_pool: &SqlitePool) -> kea_platform::CaptureOpts {
    kea_platform::CaptureOpts {
        window_title: read_bool_setting(
            config_pool,
            kea_platform::CaptureOpts::SETTING_WINDOW_TITLE,
            false,
        )
        .await,
        url: read_bool_setting(config_pool, kea_platform::CaptureOpts::SETTING_URL, false).await,
    }
}

/// The profile that applies to `ctx`, if any.
///
/// Fails open like the vocabulary read: a profile is a refinement, and a broken
/// profiles table must not be the reason a rewrite refuses to run.
pub async fn profile_for(
    config_pool: &SqlitePool,
    ctx: Option<&kea_platform::AppContext>,
) -> Option<AppProfile> {
    let ctx = ctx?;
    let profiles = match AppProfileRepo::new(config_pool.clone())
        .list_enabled()
        .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("app profiles unavailable, using global settings: {e}");
            return None;
        }
    };
    resolve_profile(
        ProfileQuery {
            bundle_id: ctx.bundle_id.as_deref(),
            url: ctx.url.as_deref(),
        },
        &profiles,
    )
    .cloned()
}

/// Renames one side of a meeting.
///
/// Upserts with `source = 'user'`, which is what stops the channel defaults
/// from overwriting a name a human typed the next time a segment lands.
#[tauri::command]
pub async fn set_meeting_speaker_name(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
    speaker_key: String,
    display_name: String,
) -> Result<(), String> {
    state
        .meeting_repo
        .set_speaker_name(&meeting_id, &speaker_key, &display_name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_app_profiles(state: State<'_, Arc<AppState>>) -> Result<Vec<AppProfile>, String> {
    AppProfileRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upsert_app_profile(
    state: State<'_, Arc<AppState>>,
    profile: AppProfile,
) -> Result<(), String> {
    AppProfileRepo::new(state.config_pool.clone())
        .upsert(&profile)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_app_profile(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    AppProfileRepo::new(state.config_pool.clone())
        .delete(&id)
        .await
        .map_err(|e| e.to_string())
}

/// KEA's own bundle id, so a capture can tell "the user is in KEA" from "the
/// user is in the app they wanted". Must match `tauri.conf.json`'s identifier.
const OWN_BUNDLE_ID: &str = "ai.kea.desktop";

/// How long [`capture_app_context`] waits before reading the frontmost app.
const CAPTURE_SWITCH_GRACE: Duration = Duration::from_secs(3);

/// Identifies the app the user wants a profile for, for the Profiles page.
///
/// The trap: pressing a button in KEA's settings window makes KEA frontmost, so
/// reading the frontmost app at click time always answers "KEA". There is no
/// reliable "previously frontmost" to ask for either — `NSWorkspace`'s running
/// list is not ordered by recency, and the accurate answer needs an activation
/// observer running since launch.
///
/// So the capture is deliberately delayed: the button tells the user to switch
/// to the app they mean, and the probe runs a few seconds later. It reads as a
/// quirk and it is one, but it is honest and it works on the first try, which a
/// silently-wrong bundle id does not.
///
/// Returns `None` when the answer is still KEA — the user did not switch — so
/// the page can say so instead of writing a profile that matches itself.
#[tauri::command]
pub async fn capture_app_context(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<kea_platform::AppContext>, String> {
    let opts = capture_opts(&state.config_pool).await;
    tokio::time::sleep(CAPTURE_SWITCH_GRACE).await;
    let ctx = kea_platform::new_app_context_probe().capture(opts);
    if ctx.bundle_id.as_deref() == Some(OWN_BUNDLE_ID) || ctx.bundle_id.is_none() {
        return Ok(None);
    }
    Ok(Some(ctx))
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
        .find_command(action.feature, &action.command)
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
    // Not `HOTKEY_ACTIONS`: the persisted translate rows own hotkeys too, and
    // a check that could not see them would let two languages claim one combo.
    for other in hotkey_owners(bindings) {
        if other.feature == feature && other.command == command {
            continue;
        }
        let other_effective = bindings
            .iter()
            .find(|b| b.feature_id == other.feature && b.command == other.command)
            .map(|b| b.accelerator.clone())
            .or_else(|| compiled_default_accelerator(other.feature, &other.command));

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

/// Unbind one hotkey: drop its row and put the OS registration back to what
/// the row was hiding.
///
/// `set_hotkey` can only ever *move* a binding, so this is the only writer of
/// the empty state. It exists for the translate family, where removing a
/// language has to take its shortcut with it — a row left behind would keep a
/// global combo booked for a language the user can no longer see, and no
/// screen would offer a way to get it back.
///
/// "Back to what the row was hiding" matters for the fixed commands: clearing
/// a custom rewrite shortcut must restore the compiled-in default, not leave
/// the feature with no shortcut at all until the next launch.
#[tauri::command]
pub async fn clear_hotkey(
    state: State<'_, Arc<AppState>>,
    feature: String,
    command: String,
) -> Result<(), String> {
    let repo = HotkeyBindingRepo::new(state.config_pool.clone());
    let Some(row) = repo
        .get(&feature, &command)
        .await
        .map_err(|e| e.to_string())?
    else {
        // Nothing persisted: the compiled default (if any) is already what is
        // registered, so there is nothing to undo.
        return Ok(());
    };
    repo.delete(&feature, &command)
        .await
        .map_err(|e| e.to_string())?;

    // The DB is authoritative and already updated: a failure below leaves a
    // stale registration that dies with the process, which is strictly better
    // than a row the user cannot delete.
    {
        let mut hotkeys = state.hotkeys.lock().map_err(|e| e.to_string())?;
        let fallback = compiled_default_accelerator(&feature, &command);
        let outcome = match (&fallback, hotkey_action(&feature, &command)) {
            (Some(default), Some(action)) => register_hotkey(&mut hotkeys, &action, default),
            _ => hotkeys
                .unregister(&HotkeyBinding {
                    accelerator: row.accelerator.clone(),
                })
                .map_err(|e| e.to_string()),
        };
        if let Err(err) = outcome {
            tracing::warn!(
                feature = %feature, command = %command,
                accelerator = %row.accelerator, %err,
                "clearing a hotkey left the old registration in place (non-fatal)"
            );
        }
    }

    let mut statuses = state.hotkey_reg_status.lock().map_err(|e| e.to_string())?;
    clear_hotkey_reg_status(&mut statuses, &feature, &command);
    Ok(())
}

#[tauri::command]
pub async fn trigger_rewrite(
    state: State<'_, Arc<AppState>>,
    mode: RewriteMode,
    preset_id: Option<String>,
    custom_instruction: Option<String>,
) -> Result<String, String> {
    // No profile here on purpose: this is the UI asking for one specific mode
    // against the selection, and the frontmost app at that moment is KEA's own
    // window. A per-app rule has nothing to match and nothing to override.
    //
    // No undo offer either, and for the same reason: the offer has to record
    // the app to hand focus back to, and "frontmost" here is the settings
    // window. An offer aimed at KEA itself would refuse when pressed — see
    // `undo_last_rewrite` — which is a worse answer than the honest "there is
    // no recent rewrite to undo". The shortcut, the translate keys, `kea://`
    // and the HTTP endpoint all go through `run_selection_rewrite`, which does
    // record one.
    execute_rewrite(
        &state,
        RewriteInput {
            source_text: String::new(),
            mode,
            preset_id,
            custom_instruction,
        },
        &ProfileOverrides::default(),
    )
    .await
    .map(|outcome| outcome.text)
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
    preview_rewrite_inner(state.inner(), text, mode, preset_id, custom_instruction).await
}

/// [`preview_rewrite`] without the Tauri boundary.
///
/// Shared with `kea://rewrite?text=…` and `POST /v1/rewrite` with a `text`
/// field: both want a rewrite that leaves the user's document, History and
/// conversations alone, which is exactly what this already is.
pub async fn preview_rewrite_inner(
    state: &Arc<AppState>,
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

/// Reads one meeting for a command that is about to render or rewrite it.
///
/// Every command below starts this way, and each of them wants the same "not
/// found is an error, not an empty answer" answer that `get_meeting` gives.
async fn meeting_detail(state: &AppState, id: &str) -> Result<MeetingDetail, String> {
    state
        .meeting_repo
        .get(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {id} not found"))
}

/// The meeting rendered as Markdown, for the clipboard.
#[tauri::command]
pub async fn meeting_markdown(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
) -> Result<String, String> {
    let detail = meeting_detail(&state, &meeting_id).await?;
    Ok(kea_core::meetings::meeting_to_markdown(
        &detail,
        &detail.speakers,
        &detail.action_items,
    ))
}

/// Writes the same Markdown into `~/Downloads` and returns where it landed,
/// so the caller can reveal it.
///
/// Downloads rather than a save dialog for the same reason `export_transcript`
/// falls back there: a native picker is a plugin plus an ACL entry, and the
/// path comes back either way.
#[tauri::command]
pub async fn export_meeting_markdown(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
) -> Result<String, String> {
    let detail = meeting_detail(&state, &meeting_id).await?;
    let body =
        kea_core::meetings::meeting_to_markdown(&detail, &detail.speakers, &detail.action_items);
    let dir = dirs_download_dir()
        .ok_or_else(|| "cannot find a writable directory to export into".to_string())?;
    let target = dir.join(kea_core::meetings::markdown_file_name(
        &detail.meeting.title,
        &detail.meeting.started_at,
    ));
    std::fs::write(&target, body)
        .map_err(|e| format!("could not write {}: {e}", target.display()))?;
    Ok(target.to_string_lossy().into_owned())
}

/// The settings key holding the Notion page every export becomes a child of.
///
/// The link is stored exactly as the user pasted it, not as a parsed id: the
/// field shows back what they typed, and the id is re-derived on every use so
/// a bad link is re-diagnosed rather than remembered as garbage.
pub const NOTION_PARENT_PAGE_SETTING: &str = "meetings.notion.parent_page";

/// Whether the Notion export is set up, and what is wrong if it is not.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NotionStatus {
    pub has_token: bool,
    /// The destination link as the user pasted it, "" when unset.
    pub parent_page: String,
    /// Why the saved link cannot be used. Checked on read so a typo surfaces
    /// on the settings screen rather than at the end of a meeting.
    pub parent_page_error: Option<String>,
}

/// The saved setup, judged by the same parser the export runs.
///
/// Pure, so the verdict this screen shows and the one the export acts on
/// cannot disagree. An unset link is not an error — it is the state a fresh
/// install is in — so only a link that was typed and cannot be used reports
/// one.
pub fn notion_status(has_token: bool, parent_page: String) -> NotionStatus {
    let parent_page_error = (!parent_page.trim().is_empty())
        .then(|| parse_page_id(&parent_page).err())
        .flatten()
        .map(|e| e.to_string());
    NotionStatus {
        has_token,
        parent_page,
        parent_page_error,
    }
}

#[tauri::command]
pub async fn get_notion_status(state: State<'_, Arc<AppState>>) -> Result<NotionStatus, String> {
    let has_token = credential_exists(state.credentials.as_ref(), NOTION_TOKEN_REF).await?;
    let parent_page = SettingsRepo::new(state.config_pool.clone())
        .get_optional::<String>(NOTION_PARENT_PAGE_SETTING)
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(notion_status(has_token, parent_page))
}

/// Save the internal integration secret.
///
/// The keychain, never the settings table: this is a bearer credential for the
/// user's whole Notion workspace, and `secrets.rs` is where such a thing goes.
#[tauri::command]
pub async fn set_notion_token(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("paste the integration secret, or use Forget to remove it".into());
    }
    state
        .credentials
        .set(NOTION_TOKEN_REF, token)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_notion_token(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state
        .credentials
        .delete(NOTION_TOKEN_REF)
        .await
        .map_err(|e| e.to_string())
}

/// Write one meeting to Notion as a new page, and return where it landed.
///
/// Manual, from the meeting's own screen — there is no export-on-stop. That is
/// the simplest way to honour the rule that an export must never hold up the
/// end of a meeting: a button pressed after the fact cannot, by construction.
#[tauri::command]
pub async fn export_meeting_to_notion(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
) -> Result<String, String> {
    let token = state
        .credentials
        .get(NOTION_TOKEN_REF)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| NotionError::NoToken.to_string())?;
    let parent = SettingsRepo::new(state.config_pool.clone())
        .get_optional::<String>(NOTION_PARENT_PAGE_SETTING)
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    let detail = meeting_detail(&state, &meeting_id).await?;
    // The same renderer the file and clipboard exports use: Notion is a
    // destination, not a second way of saying what a meeting is.
    let body =
        kea_core::meetings::meeting_to_markdown(&detail, &detail.speakers, &detail.action_items);

    let page = crate::notion::export_markdown(
        &crate::notion::ReqwestNotionApi::new(),
        &token,
        &parent,
        detail.meeting.title.trim(),
        &body,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(page.url.unwrap_or(page.id))
}

/// Tick an action item off, or put it back.
#[tauri::command]
pub async fn set_meeting_action_item_status(
    state: State<'_, Arc<AppState>>,
    id: i64,
    status: String,
) -> Result<(), String> {
    let status = ActionItemStatus::from_str(&status)
        .ok_or_else(|| format!("unknown action item status: {status}"))?;
    state
        .meeting_repo
        .set_action_item_status(id, status)
        .await
        .map_err(|e| e.to_string())
}

/// Add what the synthesis pass missed.
#[tauri::command]
pub async fn add_meeting_action_item(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
    text: String,
) -> Result<(), String> {
    state
        .meeting_repo
        .add_action_item(
            &meeting_id,
            &NewActionItem {
                text,
                ..Default::default()
            },
        )
        .await
        .map(|_id| ())
        .map_err(|e| e.to_string())
}

/// Rename a meeting from the UI.
///
/// [`TitleSource::User`] is the load-bearing part: it is what stops a later
/// synthesis pass — or a calendar match on a re-run — from overwriting a name
/// a human typed.
#[tauri::command]
pub async fn set_meeting_title(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
    title: String,
) -> Result<(), String> {
    state
        .meeting_repo
        .set_title_with_source(&meeting_id, &title, TitleSource::User)
        .await
        .map_err(|e| e.to_string())
}

/// Show a file KEA just wrote in the system file manager.
///
/// Takes the path an export command returned rather than letting the frontend
/// compose one: the webview is KEA's own, but the only paths it is ever handed
/// are ones KEA wrote.
#[tauri::command]
pub fn open_path_in_file_manager(path: String) -> Result<(), String> {
    reveal_in_file_manager(Path::new(&path))
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
    reveal_in_file_manager(&state.log_dir)
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
    // The saved rate, not the default: a preview at a different speed than
    // the real read-aloud run is not a preview of anything.
    let speed = TtsSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .map(|settings| settings.speed)
        .unwrap_or(1.0);
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
                speed: Some(speed),
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

/// The speakers inside one multi-speaker voice bundle.
///
/// Its own command rather than a field on `EngineCaps`: voices belong to a
/// *model*, not to the engine that loads it, and `EngineCaps` is a one-field
/// struct with construction sites all over the workspace. Empty for a
/// single-speaker model, which is not an error — it is what "this voice has
/// no sub-voices" looks like.
#[tauri::command]
pub fn list_onnx_voices(model_id: String) -> Vec<kea_infer::OnnxVoice> {
    ModelRegistry::voices(&model_id)
}

/// The voices the operating system itself offers. Empty where there is no
/// system synthesizer, so the caller needs no platform check of its own.
#[tauri::command]
pub fn list_system_voices() -> Vec<kea_platform::SystemVoice> {
    kea_platform::new_system_tts().voices()
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
    validate_model_id_for_delete(kind, &model_id)?;
    match kind {
        ModelKind::Whisper => state.model_storage.remove_model(&model_id),
        _ => onnx_storage_for(&state, kind)?.remove_onnx(&model_id),
    }
    .map_err(|e| format!("failed to remove model files: {e}"))?;

    clear_references_to_deleted_model(&state.config_pool, kind, &model_id).await
}

/// Drops everything that still points at a model whose files have just been
/// removed. Split out of [`delete_model`] because it is the half that can be
/// tested against a pool alone, and the half that is easy to get wrong.
pub async fn clear_references_to_deleted_model(
    config_pool: &SqlitePool,
    kind: ModelKind,
    model_id: &str,
) -> Result<(), String> {
    // No slot means nothing binds to this kind, so there are no bindings to
    // sweep — and sweeping "stt" for a streaming model would clear the user's
    // dictation engine. What dangles instead is the setting that named it.
    let Some(default_slot) = kind.default_slot() else {
        if clear_streaming_model_for_deleted(config_pool, model_id).await? {
            tracing::info!(model = %model_id, "cleared the streaming model setting");
        }
        return Ok(());
    };

    let bindings = BindingRepo::new(config_pool.clone());
    let cleared = clear_bindings_for_model(&bindings, default_slot, model_id).await?;
    if !cleared.is_empty() {
        tracing::info!(
            model = %model_id,
            slot = %default_slot,
            features = %cleared.join(", "),
            "cleared bindings referencing the deleted model"
        );
    }
    if clear_active_model_for_deleted(config_pool, default_slot, model_id).await? {
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

// ---------------------------------------------------------------------------
// File transcription (plan items 14 and 15)
// ---------------------------------------------------------------------------

/// Bridges the pure chunk driver to Tauri events and the cancel slot.
///
/// A struct rather than two closures because both callbacks need the same
/// `AppHandle` and job id, and two closures capturing it is two clones of the
/// same state — which is the shape [`TranscribeSink`] exists to avoid.
struct EventSink {
    app: AppHandle,
    job_id: String,
    cancel: watch::Receiver<bool>,
}

impl TranscribeSink for EventSink {
    fn progress(&self, chunk_index: usize, chunk_count: usize, done_ms: u64, total_ms: u64) {
        emit_transcribe_file_progress(
            &self.app,
            &TranscribeFileProgressPayload {
                job_id: self.job_id.clone(),
                audio_ms_done: done_ms,
                audio_ms_total: total_ms,
                chunk_index,
                chunk_count,
            },
        );
    }

    fn segment(&self, segment: &kea_engines::traits::SttSegment) {
        emit_transcribe_file_segment(
            &self.app,
            &TranscribeFileSegmentPayload {
                job_id: self.job_id.clone(),
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text.clone(),
            },
        );
    }

    fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }
}

/// Releases `file_transcribe_busy` however the job ends.
///
/// An owner rather than a `store(false)` at each early return: the job has
/// six of them (decode failure, engine resolution, every `?` in the body),
/// and one missed reset leaves the feature permanently refusing new files
/// with no way back short of a restart.
struct FileTranscribeGuard {
    state: Arc<AppState>,
}

impl Drop for FileTranscribeGuard {
    fn drop(&mut self) {
        self.state
            .file_transcribe_busy
            .store(false, Ordering::SeqCst);
        stop_poll(&self.state.file_transcribe_cancel);
    }
}

fn new_job_id() -> String {
    format!(
        "job-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// How many cues the UI is handed at once when it asks for a transcript.
const TRANSCRIPT_LIST_LIMIT: i64 = 200;

/// Transcribes an audio or video file, streaming cues as they land.
///
/// Returns the transcript id immediately-ish — the work is awaited here
/// rather than detached, so the frontend's `invoke` resolves when the job is
/// done and the `transcribe:file:*` events carry everything in between.
#[tauri::command]
pub async fn transcribe_file(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    path: String,
) -> Result<String, String> {
    transcribe_file_run(state.inner(), &app, &path).await
}

/// [`transcribe_file`] without the Tauri boundary, for `kea://transcribe` and
/// `POST /v1/transcribe`.
///
/// It claims `file_transcribe_busy` here rather than leaving that to each
/// caller: the flag is what makes "one job at a time" true, and a second
/// entry point that forgot it would run two decodes over the same CPU.
pub async fn transcribe_file_run(
    state: &Arc<AppState>,
    app: &AppHandle,
    path: &str,
) -> Result<String, String> {
    let state = state.clone();
    // One job at a time, and emphatically *not* the capture gate: this never
    // opens a device, and taking that lock would block dictation for the
    // length of a podcast.
    if state
        .file_transcribe_busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a file is already being transcribed; wait for it to finish".into());
    }
    let _guard = FileTranscribeGuard {
        state: state.clone(),
    };

    let job_id = new_job_id();
    match transcribe_file_inner(&state, app, &job_id, path).await {
        Ok((transcript_id, cancelled)) => {
            emit_transcribe_file_complete(app, &job_id, &transcript_id, cancelled);
            Ok(transcript_id)
        }
        Err(e) => {
            emit_transcribe_file_error(app, &job_id, &e);
            Err(e)
        }
    }
}

async fn transcribe_file_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
    job_id: &str,
    path: &str,
) -> Result<(String, bool), String> {
    let source = PathBuf::from(path);
    let filename = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());

    let bindings = BindingRepo::new(state.config_pool.clone());
    let binding = SlotResolver::new(&state.engines, &bindings)
        .require_stt("transcribe")
        .await
        .map_err(|e| e.to_string())?;
    let engine = state
        .engines
        .stt(&binding.engine_id)
        .ok_or_else(|| format!("no stt engine '{}'", binding.engine_id))?;

    // Decoding is blocking and can take seconds on a long video, so it goes
    // to the blocking pool rather than stalling the async runtime.
    let decode_path = source.clone();
    let audio = tokio::task::spawn_blocking(move || decode_file(&decode_path))
        .await
        .map_err(|e| format!("decode task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    let repo = TranscriptRepo::new(state.data_pool.clone());
    let transcript_id = format!("tr-{job_id}");
    repo.create(&NewTranscript {
        id: transcript_id.clone(),
        source_path: source.to_string_lossy().into_owned(),
        source_filename: filename,
        stt_engine_id: Some(binding.engine_id.clone()),
        model: binding.model.clone(),
        language: None,
    })
    .await
    .map_err(|e| e.to_string())?;

    // The ledger row, so a file transcription shows up in Logs and History
    // like every other feature's run even though its body lives elsewhere.
    let actions = ActionRepo::new(state.data_pool.clone());
    let action_id = actions
        .record(NewAction {
            feature_id: "transcribe".into(),
            command: "transcribe_file".into(),
            engine_id: binding.engine_id.clone(),
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let guard = ActionGuard::new(&actions, action_id, "transcribe");

    let outcome = run_file_transcription(
        state,
        app,
        job_id,
        engine.as_ref(),
        &binding,
        audio,
        &repo,
        &transcript_id,
    )
    .await;

    match outcome {
        Ok(cancelled) => {
            guard.succeed().await;
            Ok((transcript_id, cancelled))
        }
        Err(e) => {
            let message = guard.fail(e).await;
            let _ = repo
                .complete(&transcript_id, TranscriptStatus::Error, 0, Some(&message))
                .await;
            Err(message)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_file_transcription(
    state: &Arc<AppState>,
    app: &AppHandle,
    job_id: &str,
    engine: &dyn kea_engines::traits::SttEngine,
    binding: &Binding,
    // By value: one hour of 16 kHz mono f32 is ~230 MB, so every gratuitous
    // clone of the decoded buffer is another 230 MB resident. The frame is
    // moved into the engine-layer `AudioPcm` below rather than copied.
    audio: PcmFrame,
    repo: &TranscriptRepo,
    transcript_id: &str,
) -> Result<bool, String> {
    let sample_rate_hz = audio.sample_rate_hz;
    let duration_ms = if sample_rate_hz > 0 {
        (audio.samples.len() as u64 * 1_000) / sample_rate_hz as u64
    } else {
        0
    };

    // Another slot beside the two poll cancels rather than new machinery.
    let (cancel_tx, cancel_rx) = watch::channel(false);
    if let Ok(mut slot) = state.file_transcribe_cancel.lock() {
        *slot = Some(cancel_tx);
    }

    let cuts = cut_points(
        &audio.samples,
        sample_rate_hz,
        DEFAULT_CHUNK_SECS,
        CHUNK_CUT_SEARCH_SECS,
    );
    let spans = plan_chunks(audio.samples.len(), sample_rate_hz, &cuts);

    let vocabulary = load_vocabulary(&state.config_pool).await;
    let opts = SttOpts {
        model: binding.model.clone(),
        // No language: file transcription has no language setting of its own,
        // and borrowing dictation's would decode a dropped foreign-language
        // recording as English with nothing in this UI to explain why.
        language: None,
        provider_ref: binding.provider_ref.clone(),
        vocabulary: hint_terms(&vocabulary),
    };

    let sink = EventSink {
        app: app.clone(),
        job_id: job_id.to_string(),
        cancel: cancel_rx,
    };
    let pcm = kea_engines::traits::AudioPcm {
        samples: audio.samples,
        sample_rate_hz,
    };
    let outcome = transcribe_chunks(engine, &pcm, &spans, &opts, &sink)
        .await
        .map_err(|e| e.to_string())?;

    let speakers = diarize_if_enabled(state, &pcm, &outcome.segments).await;
    repo.replace_segments(transcript_id, &outcome.segments, &speakers)
        .await
        .map_err(|e| e.to_string())?;
    repo.complete(
        transcript_id,
        if outcome.cancelled {
            TranscriptStatus::Cancelled
        } else {
            TranscriptStatus::Completed
        },
        duration_ms as i64,
        None,
    )
    .await
    .map_err(|e| e.to_string())?;

    Ok(outcome.cancelled)
}

/// Runs speaker diarization when the setting asks for it and both models are
/// installed; otherwise every cue is unlabelled, which renders exactly as it
/// did before this feature existed.
///
/// Off by default. Channel attribution ("You"/"Others") is the better default
/// where it is available, because it is grounded in which device the audio
/// arrived on rather than in a clustering threshold — but a dropped file is a
/// single mixed stream, so it has no channels to attribute and the model is
/// the only thing that can answer. It stays opt-in because it costs a 36 MB
/// download and a second inference pass over the whole recording.
async fn diarize_if_enabled(
    state: &Arc<AppState>,
    audio: &kea_engines::traits::AudioPcm,
    segments: &[kea_engines::traits::SttSegment],
) -> Vec<Option<String>> {
    let unlabelled = vec![None; segments.len()];
    if segments.is_empty() {
        return unlabelled;
    }
    if !read_bool_setting(&state.config_pool, TRANSCRIBE_DIARIZE_SETTING, false).await {
        return unlabelled;
    }

    let spans = match run_diarization(state, audio).await {
        Ok(spans) => spans,
        Err(e) => {
            // Never fatal: a transcript without speaker labels is still the
            // transcript the user asked for.
            tracing::warn!(error = %e, "diarization unavailable; the transcript has no speaker labels");
            return unlabelled;
        }
    };
    assign_speakers(segments, &spans)
}

#[cfg(feature = "sherpa")]
async fn run_diarization(
    state: &Arc<AppState>,
    audio: &kea_engines::traits::AudioPcm,
) -> Result<Vec<kea_infer::SpeakerSpan>, String> {
    use kea_infer::{
        DiarizationModels, DiarizationOpts, SherpaOnnxDiarization, SpeakerDiarization,
        DIARIZATION_EMBEDDING_ID, DIARIZATION_SEGMENTATION_ID,
    };

    let storage = &state.diarization_storage;
    let models = DiarizationModels::locate(
        &storage.onnx_dir_for(DIARIZATION_SEGMENTATION_ID),
        &storage.onnx_dir_for(DIARIZATION_EMBEDDING_ID),
    )
    .map_err(|e| e.to_string())?;

    SherpaOnnxDiarization::new()
        .diarize(
            kea_infer::AudioPcm {
                samples: audio.samples.clone(),
                sample_rate_hz: audio.sample_rate_hz,
            },
            &models,
            DiarizationOpts::default(),
        )
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "sherpa"))]
async fn run_diarization(
    _state: &Arc<AppState>,
    _audio: &kea_engines::traits::AudioPcm,
) -> Result<Vec<kea_infer::SpeakerSpan>, String> {
    Err("this build has no diarization runtime (the `sherpa` feature is off)".into())
}

/// Opens the system file picker and returns the chosen path, or `None` when
/// the user cancelled.
///
/// A Rust command driving `tauri-plugin-dialog`'s own API rather than the
/// JS plugin package: KEA's `#[tauri::command]`s are not ACL-gated, so this
/// needs neither a `dialog:default` capability entry nor an npm dependency,
/// and the extension filter stays next to `is_probably_decodable`, which is
/// the list the drop zone filters on.
#[tauri::command]
pub async fn pick_audio_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Audio and video", AUDIO_FILE_EXTENSIONS)
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let picked = rx.await.map_err(|e| e.to_string())?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

/// Extensions offered in the picker.
///
/// Advisory, exactly like `kea_platform::audio::is_probably_decodable`: the
/// decoder probes the bytes, so a mislabelled file still works and a filter
/// that was too narrow would only hide it from the picker.
const AUDIO_FILE_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "m4b", "mp4", "mov", "aac", "flac", "ogg", "opus", "caf", "aiff", "mkv",
    "webm",
];

/// Asks a running file transcription to stop.
///
/// Worst-case latency is one chunk: a decode already inside `spawn_blocking`
/// cannot be interrupted, which is why the UI says "Stopping…" rather than
/// pretending the stop is instant.
#[tauri::command]
pub fn cancel_file_transcription(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    stop_poll(&state.file_transcribe_cancel);
    Ok(())
}

#[tauri::command]
pub async fn list_transcripts(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<TranscriptRow>, String> {
    TranscriptRepo::new(state.data_pool.clone())
        .list(TRANSCRIPT_LIST_LIMIT)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_transcript(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<TranscriptDetail, String> {
    TranscriptRepo::new(state.data_pool.clone())
        .get(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("transcript {id} not found"))
}

#[tauri::command]
pub async fn delete_transcript(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    TranscriptRepo::new(state.data_pool.clone())
        .delete(&id)
        .await
        .map_err(|e| e.to_string())
}

/// Renders a stored transcript as SRT or VTT without writing a file, for the
/// copy button and for the page's preview.
#[tauri::command]
pub async fn render_transcript_subtitles(
    state: State<'_, Arc<AppState>>,
    id: String,
    format: String,
) -> Result<String, String> {
    let format = SubtitleFormat::from_str(&format)
        .ok_or_else(|| format!("unknown subtitle format: {format}"))?;
    let detail = TranscriptRepo::new(state.data_pool.clone())
        .get(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("transcript {id} not found"))?;
    Ok(format.render(
        &segments_from_rows(&detail.segments),
        &SubtitleOpts::default(),
    ))
}

/// Where an export lands when the user does not pick a path.
///
/// Beside the source file, which is what a subtitle is for — a player looks
/// for `movie.srt` next to `movie.mp4`. Falls back to Downloads when that
/// directory is not writable: a read-only volume, or an iCloud placeholder
/// whose "directory" is not really there.
fn export_destination(source: &Path, format: SubtitleFormat) -> Result<PathBuf, String> {
    let name = format!(
        "{}.{}",
        source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "transcript".into()),
        format.extension()
    );
    if let Some(dir) = source.parent() {
        if is_writable_dir(dir) {
            return Ok(dir.join(name));
        }
    }
    let downloads = dirs_download_dir()
        .ok_or_else(|| "cannot find a writable directory to export into".to_string())?;
    Ok(downloads.join(name))
}

/// Probes writability by writing, not by reading permissions: a read-only
/// volume, a sandbox denial and an iCloud placeholder all report plausible
/// permissions and then fail the write.
fn is_writable_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".kea-export-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// The user's Downloads directory.
///
/// Resolved from `$HOME` rather than through a Tauri plugin: `download_dir`
/// is not part of core v2's command surface, and adding a plugin plus its ACL
/// entry for one path would be a larger change than the path.
fn dirs_download_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let dir = PathBuf::from(home).join("Downloads");
    dir.is_dir().then_some(dir)
}

/// Writes a transcript as a subtitle file and returns where it landed.
#[tauri::command]
pub async fn export_transcript(
    state: State<'_, Arc<AppState>>,
    id: String,
    format: String,
    destination: Option<String>,
) -> Result<String, String> {
    let format = SubtitleFormat::from_str(&format)
        .ok_or_else(|| format!("unknown subtitle format: {format}"))?;
    let detail = TranscriptRepo::new(state.data_pool.clone())
        .get(&id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("transcript {id} not found"))?;

    let body = format.render(
        &segments_from_rows(&detail.segments),
        &SubtitleOpts::default(),
    );
    let target = match destination {
        Some(path) => PathBuf::from(path),
        None => export_destination(Path::new(&detail.transcript.source_path), format)?,
    };
    std::fs::write(&target, body)
        .map_err(|e| format!("could not write {}: {e}", target.display()))?;
    Ok(target.to_string_lossy().into_owned())
}

// ===========================================================================
// Prompt palette (plan item 12) and screen-capture OCR (item 20)
// ===========================================================================

/// The instruction history's read limit. Arrow-up walks this list; past a few
/// dozen it is faster to retype than to keep pressing.
const PALETTE_HISTORY_READ: usize = 50;

/// Whether a Replace re-reads the selection and compares it before writing
/// over it. Default **on**: the palette deactivates the target app, and a
/// handful of editors (Electron, some web canvases, some terminals) drop the
/// selection on `resignFirstResponder`. Replacing then destroys whatever the
/// caret happens to be near. One extra ⌘C plus `COPY_SETTLE` (~150 ms) is the
/// price; this key is for users who would rather have it back.
pub const PALETTE_VERIFY_SETTING: &str = "palette.verify_selection";

/// Vision's spell-correction pass. Right for prose, ruinous for code,
/// identifiers and serial numbers — which is most of what people OCR off a
/// terminal — so the settings row says as much.
pub const OCR_LANGUAGE_CORRECTION_SETTING: &str = "ocr.language_correction";

/// BCP-47 tags, comma-separated, most preferred first. Empty leaves the choice
/// to Vision, which is the right default: a guessed list is worse than none.
pub const OCR_LANGUAGES_SETTING: &str = "ocr.languages";

/// How long to wait for the palette webview to say it has rendered, before
/// showing the window anyway.
///
/// The webview is loaded at startup and only has to paint two strings, so this
/// never fires in practice. It exists because the alternative to a fallback is
/// a palette that never appears — and, worse, a `BusyGuard` held until restart
/// — if the webview is wedged.
const PALETTE_READY_FALLBACK: Duration = Duration::from_millis(600);

/// One open palette: what it captured, where it came from, and the busy flag
/// it holds until it closes.
///
/// The `BusyGuard` is a field rather than a local of the hotkey handler on
/// purpose. `spawn_dispatch_loop` drops a press's guard when the handler
/// future returns, which is right for the other four actions and wrong for
/// this one: the palette's busy window runs from open to dismissal. Moving the
/// guard in here is what makes the rewrite shortcut stay blocked for exactly
/// as long as the palette is up — no longer (a leak would wedge both features
/// until restart) and no shorter (a ⌘C from a rewrite landing between the
/// palette's ⌘C and its ⌘V corrupts the document).
pub struct PaletteSession {
    pub id: u64,
    /// What the answer is about. Empty is the ordinary "ask KEA anything" case.
    pub source_text: String,
    pub origin: PaletteOrigin,
    pub options: DeliveryOptions,
    /// The frontmost app when the shortcut fired, for the profile lookup.
    pub context: Option<kea_platform::AppContext>,
    /// The pid to hand focus back to. Read before the window appeared: once
    /// KEA is active, "frontmost" is KEA.
    pub target_pid: Option<i32>,
    /// A line the palette shows above the input — why there is no source text,
    /// or why only Copy is on offer.
    pub notice: Option<String>,
    /// True from submit until the request finishes. A second Return while this
    /// is set is ignored rather than spending a second provider call.
    pub running: bool,
    /// Whether the window has been shown for this session yet.
    pub shown: bool,
    _busy: BusyGuard,
}

/// The palette session as the webview sees it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PaletteSessionView {
    pub session_id: u64,
    pub source_text: String,
    pub origin: String,
    /// For the "from Slack" hint. Display only — `app_name` is localized and
    /// is never a match key.
    pub app_name: Option<String>,
    pub can_replace: bool,
    pub can_insert: bool,
    pub default_delivery: String,
    pub notice: Option<String>,
}

/// What a finished palette run actually did, which is not always what was
/// asked: a Replace whose selection went missing is downgraded to an Insert,
/// and anything at all is downgraded to Copy when the target app cannot be
/// brought back.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PaletteOutcome {
    /// [`PaletteDelivery::as_str`], or `"cancelled"`.
    pub delivered: String,
    /// One line for the user, when something is worth saying. `None` on the
    /// ordinary path — a rewrite that worked needs no announcement.
    pub message: Option<String>,
}

/// The state machine's view of one slot. Both readers go through it, so a
/// submit racing a dismissal cannot be judged by two slightly different rules.
fn palette_state_of(session: Option<&PaletteSession>) -> PaletteState {
    match session {
        None => PaletteState::Closed,
        Some(session) if session.running => PaletteState::Running,
        Some(_) => PaletteState::Open,
    }
}

/// The palette's current state, for [`palette_event`].
fn palette_state(state: &AppState) -> PaletteState {
    match state.palette.lock() {
        Ok(slot) => palette_state_of(slot.as_ref()),
        // A poisoned lock means a panic while a session was held. Reporting
        // "closed" lets the next press rebuild one rather than wedging the
        // feature; `take_palette_session` recovers the guard the same way.
        Err(_) => PaletteState::Closed,
    }
}

/// Whether a palette session exists at all, running or not.
///
/// The dispatch loop asks before the busy gate: an open palette holds the
/// shared selection flag, so a second press has to be turned into a dismissal
/// rather than dropped as "already busy".
pub fn palette_is_open(state: &AppState) -> bool {
    palette_state(state) != PaletteState::Closed
}

/// Ends the session, whatever it was doing, and hands back what it held.
///
/// Dropping the returned value releases the shared busy flag, so callers that
/// still need it held (delivery) bind it and callers that do not (dismissal)
/// let it fall.
fn take_palette_session(state: &AppState) -> Option<PaletteSession> {
    match state.palette.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// Whether KEA can post synthetic keystrokes at all right now, and the line to
/// show the user when it cannot.
///
/// Both blockers are checked before the window opens, because both change what
/// the palette can offer: without them Replace and Insert are impossible and
/// Copy is the only delivery left. Refusing to open — which the plan
/// suggests — would be worse: the clipboard path needs neither permission, and
/// "ask KEA a question and copy the answer" is a perfectly good use of the
/// feature that a refusal takes away.
fn palette_typing_ability(state: &AppState) -> (bool, Option<String>) {
    if state.permissions.status(PermKind::Accessibility) != PermStatus::Granted {
        return (
            false,
            Some(
                "KEA needs Accessibility to read your selection and type the answer back. \
                 Until then the answer is copied to the clipboard."
                    .into(),
            ),
        );
    }
    #[cfg(target_os = "macos")]
    if kea_platform::textio::macos_keys::secure_input_enabled() {
        // `post_command_chord` is silently dropped under secure input, so
        // without this the palette would take the user's instruction and fail
        // at the very last step.
        return (
            false,
            Some(
                "A password field is focused somewhere, so macOS is blocking keystrokes. \
                 The answer will be copied to the clipboard."
                    .into(),
            ),
        );
    }
    (true, None)
}

/// Reads the selection for a palette session, tolerating the ordinary "nothing
/// was selected" case.
///
/// Every failure becomes an empty source rather than a refusal to open.
/// `capture_selection` cannot distinguish "nothing is selected" from "the ⌘C
/// did not reach the app": in both cases the pasteboard's change count is
/// unmoved, and `TextIoError` is a single `Other(String)`, so telling them
/// apart would mean matching on prose. Since the Accessibility and
/// secure-input blockers are already ruled out by
/// [`palette_typing_ability`], what is left is overwhelmingly "nothing
/// selected", and the palette works fine that way. The real message is logged,
/// not shown.
async fn palette_capture_selection(textio: &dyn kea_platform::TextIo) -> (String, Option<String>) {
    match textio.capture_selection().await {
        Ok(text) => (text, None),
        Err(e) => {
            tracing::debug!(error = %e, "palette: no selection to work on");
            (
                String::new(),
                Some("Nothing selected — ask KEA anything.".into()),
            )
        }
    }
}

/// Opens a palette session, taking ownership of the press's busy guard.
///
/// **Everything that touches the user's app happens before the window is
/// shown**, and the order is the feature: once KEA is active, the app context
/// answers about KEA, the synthetic ⌘C goes to the palette's own text field,
/// and the pid to hand focus back to is KEA's. The plan's Risks section
/// suggests painting the window first and filling the preview in behind it;
/// that is wrong at HEAD, and the ~150 ms of `COPY_SETTLE` spent here is what
/// buys a selection at all.
pub async fn open_palette_session(
    state: &Arc<AppState>,
    app: &AppHandle,
    origin: PaletteOrigin,
    prefilled: Option<String>,
    busy: BusyGuard,
) {
    if palette_state(state) != PaletteState::Closed {
        // The dispatch loop turns a press arriving while the palette is up
        // into a dismissal before it ever gets here; this is the belt and
        // braces for the other entry points.
        return;
    }

    let ctx = capture_app_context_now(state).await;
    let target_pid = tokio::task::spawn_blocking(crate::macfocus::frontmost_pid)
        .await
        .unwrap_or(None);

    let (can_type, mut notice) = palette_typing_ability(state);

    let source_text = match prefilled {
        // A screen capture already has its text; there is no selection to read
        // and firing a ⌘C at the user's app would be gratuitous.
        Some(text) => {
            if text.trim().is_empty() {
                notice = Some("No text found in that capture.".into());
            }
            text
        }
        None if !can_type => String::new(),
        None => {
            let textio = new_text_io();
            let (text, why) = palette_capture_selection(textio.as_ref()).await;
            // A blocker message outranks "nothing selected": it explains both.
            notice = notice.or(why);
            text
        }
    };

    let options = DeliveryOptions::resolve(origin, !source_text.trim().is_empty(), can_type);
    let id = state.palette_counter.fetch_add(1, Ordering::SeqCst) + 1;

    if let Ok(mut slot) = state.palette.lock() {
        *slot = Some(PaletteSession {
            id,
            source_text,
            origin,
            options,
            context: ctx,
            target_pid,
            notice,
            running: false,
            shown: false,
            _busy: busy,
        });
    } else {
        tracing::error!("the palette slot is poisoned; not opening");
        return;
    }

    emit_palette_open(app, id);
    spawn_palette_ready_fallback(state.clone(), app.clone(), id);
}

/// Shows the window even if the webview never reports back, so a wedged
/// frontend cannot strand the session — and with it the shared busy flag.
fn spawn_palette_ready_fallback(state: Arc<AppState>, app: AppHandle, id: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PALETTE_READY_FALLBACK).await;
        if show_palette_once(&state, &app, id) {
            tracing::warn!(
                session = id,
                "the palette webview never reported ready; showing it anyway"
            );
        }
    });
}

/// Shows the palette for `id` if that session is still current and has not been
/// shown yet. Returns whether this call was the one that showed it.
fn show_palette_once(state: &AppState, app: &AppHandle, id: u64) -> bool {
    let should_show = match state.palette.lock() {
        Ok(mut slot) => match slot.as_mut() {
            Some(session) if session.id == id && !session.shown => {
                session.shown = true;
                true
            }
            _ => false,
        },
        Err(_) => false,
    };
    if should_show {
        crate::palette::show(app);
    }
    should_show
}

/// Closes the palette in response to `event`, if the rules say it should.
///
/// The one place a session ends. Escape, a blur, the toggle shortcut and a
/// delivered result all come through here or through [`take_palette_session`],
/// so "the session is over" has exactly one meaning and a cancel racing a
/// finished request cannot both win.
pub async fn close_palette_for(state: &Arc<AppState>, app: &AppHandle, event: PaletteEvent) {
    let reaction = palette_event(palette_state(state), event);
    if !matches!(reaction, PaletteReaction::Dismiss | PaletteReaction::Cancel) {
        return;
    }

    // Taken first: a request finishing a microsecond from now must find the
    // slot empty and discard its result rather than paste it.
    let session = take_palette_session(state);
    let Some(session) = session else { return };

    crate::palette::hide(app);
    emit_palette_close(app);

    if event.restores_focus() {
        let pid = session.target_pid;
        let _ = tokio::task::spawn_blocking(move || crate::macfocus::restore_focus(pid)).await;
    }
    tracing::debug!(session = session.id, ?reaction, "palette closed");
    // `session` — and with it the BusyGuard — drops here.
}

/// Where a Copy delivery puts the text.
///
/// A seam, not indirection for its own sake: the other two deliveries already
/// go through `TextIo`, which tests substitute, and without a matching one
/// here "Copy must not type anything" could only be asserted by clobbering the
/// clipboard of whoever is running the tests.
pub trait ClipboardSink: Send + Sync {
    fn copy(&self, text: &str) -> Result<(), String>;
}

/// The real one.
pub struct SystemClipboard;

impl ClipboardSink for SystemClipboard {
    fn copy(&self, text: &str) -> Result<(), String> {
        copy_to_clipboard(text)
    }
}

/// Puts `text` on the clipboard, confirming it stuck.
///
/// **This belongs in `kea_platform::textio`**, beside
/// `set_clipboard_text_verified`, which does the same job for the paste path
/// and for the same reason: a clipboard manager declaring ownership between
/// the write and the read makes a bare `set_text` succeed while the clipboard
/// holds something else. `TextIo` has no "just copy this" method today, so the
/// palette's Copy delivery carries its own until it does.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    let mut last = String::from("no attempt was made");
    for _ in 0..3 {
        let attempt: Result<String, arboard::Error> =
            clipboard.set_text(text).and_then(|()| clipboard.get_text());
        match attempt {
            Ok(back) if back == text => return Ok(()),
            Ok(_) => last = "another app took the clipboard".into(),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!("could not copy to the clipboard: {last}"))
}

/// Writes the answer where the user asked, downgrading rather than guessing.
///
/// Two downgrades, both of which prevent a worse outcome than a disappointing
/// one:
///
/// * The target app never came back — it quit, hung, or the machine slept — so
///   any keystroke would land in whatever *is* frontmost. Everything becomes
///   Copy.
/// * The selection is no longer what it was when the palette opened. Replacing
///   would destroy a span the user did not choose, so it becomes an Insert.
#[allow(clippy::too_many_arguments)]
async fn deliver_palette_result(
    textio: &dyn kea_platform::TextIo,
    clipboard: &dyn ClipboardSink,
    delivery: PaletteDelivery,
    text: &str,
    source_text: &str,
    verify: bool,
    reactivation: crate::macfocus::Reactivation,
    replace_mode: kea_platform::ReplaceMode,
) -> Result<PaletteOutcome, String> {
    let mut message = None;
    let mut delivery = delivery;

    if delivery != PaletteDelivery::Copy && !reactivation.can_deliver() {
        tracing::warn!(
            ?reactivation,
            "palette: could not bring the target app back; copying instead"
        );
        delivery = PaletteDelivery::Copy;
        message = Some(
            "KEA could not bring that app back to the front, so the answer is on your clipboard."
                .into(),
        );
    }

    if delivery == PaletteDelivery::Replace && verify {
        // Costs one more ⌘C plus COPY_SETTLE. Safe to run: capture_selection
        // saves and restores the user's clipboard around it.
        let still_there = match textio.capture_selection().await {
            Ok(current) => current == source_text,
            Err(e) => {
                tracing::debug!(error = %e, "palette: could not re-read the selection");
                false
            }
        };
        if !still_there {
            delivery = PaletteDelivery::Insert;
            message = Some(
                "That app dropped the selection while the palette was open, \
                 so the answer was inserted at the caret instead of replacing it."
                    .into(),
            );
        }
    }

    match delivery {
        PaletteDelivery::Replace => textio
            .replace_with_mode(text, replace_mode)
            .await
            .map_err(|e| e.to_string())?,
        PaletteDelivery::Insert => textio
            .insert_at_cursor(text)
            .await
            .map_err(|e| e.to_string())?,
        PaletteDelivery::Copy => {
            clipboard.copy(text)?;
            message = message.or(Some("Copied to the clipboard.".into()));
        }
    }

    Ok(PaletteOutcome {
        delivered: delivery.as_str().into(),
        message,
    })
}

/// Opens the palette from the UI (or a test) rather than from the shortcut.
///
/// Takes the shared selection flag the same way a press does, so a palette
/// opened this way still locks out the rewrite shortcut.
#[tauri::command]
pub async fn open_palette(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    let state = state.inner().clone();
    let Some(busy) = try_acquire_busy(&state.selection_busy) else {
        return Err("KEA is already working on a selection.".into());
    };
    open_palette_session(&state, &app, PaletteOrigin::Selection, None, busy).await;
    Ok(())
}

#[tauri::command]
pub fn get_palette_session(
    state: State<'_, Arc<AppState>>,
    session_id: u64,
) -> Result<PaletteSessionView, String> {
    let slot = state.palette.lock().map_err(|e| e.to_string())?;
    let session = slot
        .as_ref()
        .filter(|s| s.id == session_id)
        .ok_or_else(|| "that palette session is no longer open".to_string())?;
    Ok(PaletteSessionView {
        session_id: session.id,
        source_text: session.source_text.clone(),
        origin: session.origin.as_str().into(),
        app_name: session.context.as_ref().and_then(|c| c.app_name.clone()),
        can_replace: session.options.can_replace,
        can_insert: session.options.can_insert,
        default_delivery: session.options.default.as_str().into(),
        notice: session.notice.clone(),
    })
}

/// The webview has rendered the session; show the window.
///
/// The window is shown from here rather than at open so it never appears
/// holding the previous run's text.
#[tauri::command]
pub fn palette_ready(state: State<'_, Arc<AppState>>, app: AppHandle, session_id: u64) {
    show_palette_once(state.inner(), &app, session_id);
}

#[tauri::command]
pub async fn cancel_palette(state: State<'_, Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    close_palette_for(state.inner(), &app, PaletteEvent::Escape).await;
    Ok(())
}

#[tauri::command]
pub async fn list_palette_history(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    PaletteHistoryRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .recent(PALETTE_HISTORY_READ)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_palette_history(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    PaletteHistoryRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .clear()
        .await
        .map_err(|e| e.to_string())
}

/// Whether to remember instructions: `palette.store_history` (default on),
/// with `history.store_conversations = false` as a global override.
///
/// The override is one-directional on purpose. Turning off conversation
/// storage is a statement about content, and an instruction is content
/// ("rewrite this rejection letter for Bob"); turning it on says nothing about
/// whether the user wants a shortcut list.
async fn palette_history_enabled(config_pool: &SqlitePool) -> bool {
    read_bool_setting(config_pool, STORE_HISTORY_SETTING, true).await
        && store_conversations_enabled(config_pool).await
}

/// Runs one palette instruction and delivers the answer.
///
/// The ordering that matters is at the end: hide, reactivate, *confirm* the
/// reactivation, then write. Step by step, and why each step is where it is,
/// in [`crate::palette`]'s module docs.
#[tauri::command]
pub async fn run_palette(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    session_id: u64,
    instruction: String,
    delivery: String,
) -> Result<PaletteOutcome, String> {
    let state = state.inner().clone();
    let delivery = PaletteDelivery::from_str(&delivery)
        .ok_or_else(|| format!("unknown palette delivery: {delivery}"))?;

    // Claim the session: one run at a time, and only for the session the
    // webview thinks it is looking at.
    let (source_text, context, target_pid, options) = {
        let mut slot = state.palette.lock().map_err(|e| e.to_string())?;
        let reaction = palette_event(
            palette_state_of(slot.as_ref()),
            PaletteEvent::Submit {
                instruction_empty: instruction.trim().is_empty(),
            },
        );
        if reaction != PaletteReaction::Run {
            // The webview applies the same rule before invoking, so getting
            // here means a stale window or a second Return that raced the
            // first — neither of which should spend a provider call.
            return Err("that instruction cannot be run right now".into());
        }
        let session = slot
            .as_mut()
            .filter(|s| s.id == session_id)
            .ok_or_else(|| "that palette session is no longer open".to_string())?;
        if !session.options.allows(delivery) {
            return Err(format!(
                "this palette session cannot deliver by {}",
                delivery.as_str()
            ));
        }
        session.running = true;
        (
            session.source_text.clone(),
            session.context.clone(),
            session.target_pid,
            session.options,
        )
    };

    let outcome = run_palette_inner(
        &state,
        &app,
        session_id,
        &instruction,
        delivery,
        source_text,
        context,
        target_pid,
        options,
    )
    .await;

    if outcome.is_err() {
        // The request failed before anything was delivered, so the palette
        // stays up with its error showing and the user can edit and retry.
        // Clearing `running` is what makes that retry possible.
        if let Ok(mut slot) = state.palette.lock() {
            if let Some(session) = slot.as_mut().filter(|s| s.id == session_id) {
                session.running = false;
            }
        }
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn run_palette_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
    session_id: u64,
    instruction: &str,
    delivery: PaletteDelivery,
    source_text: String,
    context: Option<kea_platform::AppContext>,
    target_pid: Option<i32>,
    options: DeliveryOptions,
) -> Result<PaletteOutcome, String> {
    let _ = options;
    if palette_history_enabled(&state.config_pool).await {
        if let Err(e) = PaletteHistoryRepo::new(SettingsRepo::new(state.config_pool.clone()))
            .record(instruction)
            .await
        {
            // A history write is a convenience; it must never cost the run.
            tracing::warn!(error = %e, "could not record the palette instruction");
        }
    }

    // A per-app profile applies to a palette ask exactly as it does to a
    // hotkey rewrite — same binding, same insertion mode, same post-process
    // choice. Its `mode` and `preset_id` deliberately do not: the user typed
    // an instruction, and that IS the mode. Forcing them into Professional
    // because they are in Slack would answer a question they did not ask.
    let profile = profile_for(&state.config_pool, context.as_ref()).await;
    let overrides = ProfileOverrides::from_profile(profile.as_ref());

    let input = RewriteInput {
        source_text: source_text.clone(),
        mode: RewriteMode::AskKea,
        preset_id: None,
        custom_instruction: Some(instruction.to_string()),
    };

    let bindings = BindingRepo::new(state.config_pool.clone());
    let actions = ActionRepo::new(state.data_pool.clone());
    let presets = PresetRepo::new(state.config_pool.clone());
    let prompt_overrides = PromptOverrideRepo::new(state.config_pool.clone());
    let conversations = ConversationRepo::new(state.data_pool.clone());
    let usage = UsageRepo::new(state.data_pool.clone());
    // The usage ledger is added whether or not content is stored: the counts
    // are not the user's words. See `ContentStorageOpts`.
    let storage = if store_conversations_enabled(&state.config_pool).await {
        ContentStorageOpts::enabled(&conversations)
    } else {
        ContentStorageOpts::default()
    }
    .with_usage(&usage);

    let (text, action_id) = complete_rewrite(
        &state.engines,
        &bindings,
        &actions,
        &presets,
        &prompt_overrides,
        PALETTE_COMMAND,
        &input,
        &overrides,
        storage,
    )
    .await?;

    // From here a provider call has been made and the ledger row is open, so
    // every branch below closes it.
    let stale = match state.palette.lock() {
        Ok(slot) => slot.as_ref().map(|s| s.id) != Some(session_id),
        Err(_) => true,
    };
    if palette_event(PaletteState::Running, PaletteEvent::Completed { stale })
        != PaletteReaction::Deliver
    {
        // Dismissed mid-flight. Deliver nothing — and do not quietly put the
        // answer on the clipboard either: a cancelled request that replaces
        // the clipboard is a surprise, and the clipboard is the one piece of
        // state this feature borrows and has to give back. The row is still
        // closed — the call happened and was billed — but as Cancelled rather
        // than Error: the user ending their own request is not a fault, and
        // colouring it like one teaches people to ignore the colour.
        let guard = ActionGuard::new(&actions, action_id, "rewrite");
        guard
            .cancel("the palette was dismissed before the answer arrived")
            .await;
        return Ok(PaletteOutcome {
            delivered: "cancelled".into(),
            message: None,
        });
    }

    // Holds the BusyGuard through delivery: the rewrite shortcut must stay
    // blocked until the last synthetic keystroke has landed.
    let _session = take_palette_session(state);

    crate::palette::hide(app);
    emit_palette_close(app);

    let reactivation =
        tokio::task::spawn_blocking(move || crate::macfocus::restore_focus(target_pid))
            .await
            .unwrap_or(crate::macfocus::Reactivation::Unknown);

    let verify = read_bool_setting(&state.config_pool, PALETTE_VERIFY_SETTING, true).await;
    let textio = new_text_io();
    let guard = ActionGuard::new(&actions, action_id, "rewrite");
    match deliver_palette_result(
        textio.as_ref(),
        &SystemClipboard,
        delivery,
        &text,
        &source_text,
        verify,
        reactivation,
        overrides.replace_mode(),
    )
    .await
    {
        Ok(outcome) => {
            guard.succeed().await;
            if let Some(message) = &outcome.message {
                notify_user(app, message);
            }
            Ok(outcome)
        }
        Err(e) => Err(guard.fail(e).await),
    }
}

/// Tells the user something about a run whose window has already gone.
///
/// A notification rather than an in-app banner: the palette, the screen
/// capture and a `kea://` URL are all invoked from someone else's app, so by
/// the time there is something to say the settings window is very likely
/// closed and a banner would be a message nobody ever sees.
pub fn notify_user(app: &AppHandle, message: &str) {
    if let Err(e) = app
        .notification()
        .builder()
        .title("KEA")
        .body(message)
        .show()
    {
        tracing::warn!(error = %e, message, "could not show the notification");
    }
}

// ---------------------------------------------------------------------------
// Screenshot OCR (item 20): capture a region, recognise it, open the palette
// ---------------------------------------------------------------------------

/// The OCR knobs, read from settings.
async fn ocr_options(config_pool: &SqlitePool) -> kea_platform::OcrOptions {
    let languages = SettingsRepo::new(config_pool.clone())
        .get_optional::<String>(OCR_LANGUAGES_SETTING)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    kea_platform::OcrOptions {
        languages: languages
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect(),
        language_correction: read_bool_setting(config_pool, OCR_LANGUAGE_CORRECTION_SETTING, true)
            .await,
    }
}

/// The recognition languages this macOS build actually supports, asked of the
/// framework rather than guessed, so the settings page can show a real list.
#[tauri::command]
pub fn get_ocr_languages() -> Result<Vec<String>, String> {
    kea_platform::new_text_recognizer()
        .supported_languages()
        .map_err(|e| e.to_string())
}

// ===========================================================================
// Undo, usage and rates
// ===========================================================================

/// Puts the last rewrite back, from the settings window's button.
///
/// Takes the same flag the shortcut does. The undo brings another app forward
/// and writes into it, so it must not interleave with a rewrite, a palette
/// delivery or a capture — all four are edits to the same document.
#[tauri::command]
pub async fn undo_last_rewrite_command(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let state = state.inner().clone();
    let Some(_busy) = try_acquire_busy(&state.selection_busy) else {
        return Err("KEA is busy with the text in another app".into());
    };
    undo_last_rewrite(&state).await
}

/// Everything the usage view shows, in one round trip.
///
/// One command rather than three because the three answers have to agree: a
/// breakdown priced against one snapshot of the rate table and a daily strip
/// read a moment later would be two views of two different windows.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// Per feature and per provider/model, most tokens first.
    pub totals: Vec<UsageSpend>,
    /// The same window, one row per day with any activity.
    pub daily: Vec<UsageDay>,
    /// Whether the user has entered any rate at all. The view uses it to
    /// explain an all-blank money column once, rather than per row.
    pub any_rates: bool,
    /// The window actually used, after clamping — so the heading says what
    /// was measured rather than what was asked for.
    pub days: i64,
}

#[tauri::command]
pub async fn get_usage_report(
    state: State<'_, Arc<AppState>>,
    days: i64,
) -> Result<UsageReport, String> {
    let days = usage_window(days);
    let usage = UsageRepo::new(state.data_pool.clone());
    let rates = RateRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let totals = usage.totals(days).await.map_err(|e| e.to_string())?;
    let daily = usage.daily(days).await.map_err(|e| e.to_string())?;
    Ok(UsageReport {
        totals: priced(totals, &rates),
        daily,
        any_rates: !rates.is_empty(),
        days,
    })
}

#[tauri::command]
pub async fn clear_usage(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    UsageRepo::new(state.data_pool.clone())
        .clear()
        .await
        .map_err(|e| e.to_string())
}

/// Clamps a requested window to something a SQL date expression can take.
///
/// The number reaches `printf('-%d days', ?)`, so a negative one would ask for
/// rows from the future and quietly return nothing at all.
fn usage_window(days: i64) -> i64 {
    days.clamp(1, 3650)
}

#[tauri::command]
pub async fn list_llm_rates(state: State<'_, Arc<AppState>>) -> Result<Vec<LlmRate>, String> {
    RateRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn upsert_llm_rate(state: State<'_, Arc<AppState>>, rate: LlmRate) -> Result<(), String> {
    RateRepo::new(state.config_pool.clone())
        .upsert(&rate)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_llm_rate(
    state: State<'_, Arc<AppState>>,
    provider_key: String,
    model: String,
) -> Result<(), String> {
    RateRepo::new(state.config_pool.clone())
        .delete(&provider_key, &model)
        .await
        .map_err(|e| e.to_string())
}

/// Captures a screen region, recognises its text, and opens the palette with
/// it. `None` when the user cancelled, which is not an error.
///
/// The image is owned by a `CapturedImage` that deletes its whole temp
/// directory on drop, including on a panic. It is bound for exactly as long as
/// `recognize` needs it and dropped on the next line — a screenshot of
/// someone's screen left in `/tmp` is the worst bug this feature can produce,
/// so its path never outlives the value that owns it.
async fn recognize_screen_region(config_pool: &SqlitePool) -> Result<Option<String>, String> {
    let capture = kea_platform::new_screen_capture();
    capture.availability().map_err(|e| e.to_string())?;

    let outcome = capture.capture_region().await.map_err(|e| e.to_string())?;
    let image = match outcome {
        // Escape. Open nothing, show nothing: changing one's mind is a success.
        kea_platform::CaptureOutcome::Cancelled => return Ok(None),
        kea_platform::CaptureOutcome::Captured(image) => image,
    };

    let opts = ocr_options(config_pool).await;
    let observations = kea_platform::new_text_recognizer()
        .recognize(image.path(), &opts)
        .await
        .map_err(|e| e.to_string());
    drop(image);

    Ok(Some(kea_platform::observations_to_text(&observations?)))
}

/// Runs the screen-capture shortcut's whole flow, taking the press's busy
/// guard with it.
pub async fn capture_screen_text_inner(
    state: &Arc<AppState>,
    app: &AppHandle,
    busy: BusyGuard,
) -> Result<(), String> {
    match recognize_screen_region(&state.config_pool).await? {
        Some(text) => {
            open_palette_session(state, app, PaletteOrigin::ScreenCapture, Some(text), busy).await;
        }
        // The user pressed Escape. Open nothing, show nothing: changing one's
        // mind is a success, and an error toast for a non-event is worse than
        // silence. `busy` drops here.
        None => tracing::debug!("screen capture cancelled"),
    }
    Ok(())
}

/// The screen-capture shortcut, from the UI rather than the hotkey.
///
/// The error comes back to the caller here — unlike the hotkey path, the
/// settings page is on screen and has a line to show it on.
#[tauri::command]
pub async fn capture_screen_text(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let state = state.inner().clone();
    let Some(busy) = try_acquire_busy(&state.selection_busy) else {
        return Err("KEA is already working on a selection.".into());
    };
    capture_screen_text_inner(&state, &app, busy).await
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
    use std::sync::atomic::AtomicUsize;
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
                provider_ref: "mistral".into(),
                name: "Mistral".into(),
            },
            // shadowed by a built-in ref: dropped
            CustomProvider {
                provider_ref: "openai".into(),
                name: "Shadow".into(),
            },
        ];
        let entries = provider_entries(&custom);
        assert_eq!(entries.len(), BUILT_IN_PROVIDERS.len() + 1);
        assert_eq!(entries[0].provider_ref, "openai");
        assert_eq!(entries[0].name, "OpenAI");
        assert!(entries[0].built_in);
        assert!(entries[..BUILT_IN_PROVIDERS.len()]
            .iter()
            .all(|e| e.built_in));
        let last = entries.last().unwrap();
        assert_eq!(last.provider_ref, "mistral");
        assert!(!last.built_in);
    }

    /// A built-in provider is one the user only has to hand a key to. That is
    /// true exactly when `kea_engines::WELL_KNOWN_PROVIDERS` already knows its
    /// endpoint, so the two tables are pinned to each other here rather than
    /// left to agree by habit. `local-llm` is the deliberate exception: it is
    /// keyless and its URL is whatever server the user is running, which is
    /// what `discover_local_llms` fills in.
    #[test]
    fn every_built_in_provider_needs_only_a_key() {
        assert_eq!(BUILT_IN_PROVIDERS.len(), 6);
        for (provider_ref, name) in BUILT_IN_PROVIDERS {
            assert!(!name.is_empty(), "{provider_ref} has no display name");
            if provider_ref == "local-llm" {
                continue;
            }
            let known = kea_engines::well_known(provider_ref)
                .unwrap_or_else(|| panic!("{provider_ref} is not a well-known provider"));
            assert!(!known.base_url.is_empty(), "{provider_ref} has no base URL");
            assert!(
                !known.default_model.is_empty(),
                "{provider_ref} has no default model"
            );
        }
    }

    #[test]
    fn validate_new_provider_rejects_bad_input() {
        let existing = vec![CustomProvider {
            provider_ref: "together".into(),
            name: "Together".into(),
        }];
        assert!(validate_new_provider("", "Name", &existing).is_err());
        assert!(validate_new_provider("mistral", "  ", &existing).is_err());
        assert!(validate_new_provider("openai", "Name", &existing).is_err());
        // Groq ships built in now; re-adding it by hand is the same refusal.
        assert!(validate_new_provider("groq", "Name", &existing).is_err());
        assert!(validate_new_provider("together", "Name", &existing).is_err());
        assert!(validate_new_provider("mistral", "Mistral", &existing).is_ok());
    }

    #[test]
    fn validate_new_provider_trims_before_checking_and_storing() {
        let existing = vec![CustomProvider {
            provider_ref: "together".into(),
            name: "Together".into(),
        }];
        // Padding must not smuggle a ref past the built-in / duplicate checks.
        assert!(validate_new_provider(" openai", "Name", &existing).is_err());
        assert!(validate_new_provider("together ", "Name", &existing).is_err());

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
        // Not "groq": it is a built-in ref now, and refused for that reason
        // rather than for its characters.
        for good in ["mistral", "my-server", "my_server", "v1.2", "llama3"] {
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

    /// Deleting a streaming model must not touch the dictation binding.
    ///
    /// The trap `ModelKind::default_slot` returns `Option` for: a streaming
    /// model lives in the STT *family* but nothing binds to it, so sweeping
    /// the "stt" slot for it would silently unset the user's actual
    /// speech-to-text engine. What has to be cleared instead is the setting
    /// that named it.
    #[tokio::test]
    async fn deleting_a_streaming_model_clears_its_setting_not_the_stt_binding() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let bindings = kea_core::store::bindings::BindingRepo::new(pool.clone());
        let stt = Binding {
            engine_id: "parakeet".into(),
            model: Some("parakeet-tdt-0.6b-v2".into()),
            provider_ref: None,
        };
        bindings
            .set(DEFAULT_FEATURE_ID, "stt", stt.clone())
            .await
            .unwrap();
        SettingsRepo::new(pool.clone())
            .set(STREAMING_MODEL_SETTING, &"streaming-zipformer-en-20m")
            .await
            .unwrap();

        clear_references_to_deleted_model(
            &pool,
            ModelKind::Streaming,
            "streaming-zipformer-en-20m",
        )
        .await
        .unwrap();

        assert_eq!(
            bindings.get(DEFAULT_FEATURE_ID, "stt").await.unwrap(),
            Some(stt),
            "the speech-to-text default must survive a streaming delete"
        );
        assert_eq!(streaming_model_setting(&pool).await, None);
    }

    /// The other half: deleting a *different* streaming model leaves the
    /// selected one alone.
    #[tokio::test]
    async fn deleting_another_streaming_model_leaves_the_selection_alone() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        SettingsRepo::new(pool.clone())
            .set(STREAMING_MODEL_SETTING, &"streaming-zipformer-en-20m")
            .await
            .unwrap();

        assert!(!clear_streaming_model_for_deleted(&pool, "something-else")
            .await
            .unwrap());
        assert_eq!(
            streaming_model_setting(&pool).await.as_deref(),
            Some("streaming-zipformer-en-20m")
        );
    }

    /// A cleared setting is the JSON literal `null`, not a missing row, and a
    /// picker clearing it through the generic `set_setting` command writes an
    /// empty string. Both mean "off", and neither may read as a model id.
    #[tokio::test]
    async fn an_absent_cleared_or_blank_streaming_model_all_read_as_off() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool.clone());

        assert_eq!(streaming_model_setting(&pool).await, None, "absent");
        settings
            .set(STREAMING_MODEL_SETTING, &Option::<String>::None)
            .await
            .unwrap();
        assert_eq!(
            streaming_model_setting(&pool).await,
            None,
            "cleared to null"
        );
        settings.set(STREAMING_MODEL_SETTING, &"  ").await.unwrap();
        assert_eq!(streaming_model_setting(&pool).await, None, "blank");
        settings
            .set(STREAMING_MODEL_SETTING, &"streaming-zipformer-en-20m")
            .await
            .unwrap();
        assert_eq!(
            streaming_model_setting(&pool).await.as_deref(),
            Some("streaming-zipformer-en-20m")
        );
    }

    /// Partials default on, and the toggle has to survive both encodings —
    /// the generic `set_setting` command JSON-encodes a `String`, so the UI's
    /// toggles land as the JSON string `"false"` rather than a JSON bool.
    #[tokio::test]
    async fn the_partials_toggle_defaults_on_and_reads_both_encodings() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool.clone());

        assert!(read_bool_setting(&pool, SHOW_PARTIALS_SETTING, true).await);
        settings.set(SHOW_PARTIALS_SETTING, &"false").await.unwrap();
        assert!(!read_bool_setting(&pool, SHOW_PARTIALS_SETTING, true).await);
        settings.set(SHOW_PARTIALS_SETTING, &false).await.unwrap();
        assert!(!read_bool_setting(&pool, SHOW_PARTIALS_SETTING, true).await);

        // The draft fallback is the opposite default: inserting knowably worse
        // text after an invisible failure is the regression two passes exist
        // to prevent.
        assert!(!read_bool_setting(&pool, STREAMING_FALLBACK_SETTING, false).await);
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
                language: None,
            })
            .await
            .unwrap();
        let tts = TtsSettingsRepo::new(SettingsRepo::new(pool.clone()));
        tts.set(&TtsSettings {
            active_voice: Some("alloy".into()),
            active_model: Some("vits-piper-en-us-amy-low".into()),
            ..Default::default()
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

    /// Every compiled default has to survive `normalize_accelerator` and
    /// `HotKey::from_str`, and the palette's is the first one whose key is a
    /// word rather than a letter — `platform_accelerator` takes a `char`, so
    /// "Space" is spelled out and is exactly the kind of literal that would
    /// register as a dead key instead of failing loudly.
    #[test]
    fn every_compiled_default_accelerator_parses() {
        for action in HOTKEY_ACTIONS {
            let accel = compiled_default_accelerator(action.feature, &action.command)
                .unwrap_or_else(|| panic!("{} declares no default", action.action_id()));
            assert!(
                validate_accelerator(&accel).is_ok(),
                "{} default {accel:?} does not parse",
                action.action_id()
            );
        }
    }

    /// Two features sharing a default means `set_hotkey` refuses the second
    /// one the first time a user tries to rebind it, and startup silently
    /// registers whichever went first.
    #[test]
    fn no_two_hotkey_actions_share_a_default() {
        for a in HOTKEY_ACTIONS {
            for b in HOTKEY_ACTIONS {
                if a.action_id() == b.action_id() {
                    continue;
                }
                let (x, y) = (
                    compiled_default_accelerator(a.feature, &a.command).unwrap_or_default(),
                    compiled_default_accelerator(b.feature, &b.command).unwrap_or_default(),
                );
                assert!(
                    !same_accelerator(&x, &y),
                    "{} and {} both default to {x}",
                    a.action_id(),
                    b.action_id()
                );
            }
        }
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
                compiled_default_accelerator(action.feature, &action.command).unwrap()
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

    /// The regression this helper exists for: the generic `set_setting` command
    /// writes a JSON *string*, so a reader expecting a JSON bool silently falls
    /// back to its default and the toggle never does anything. Both app-context
    /// capture flags shipped that way for exactly one batch.
    #[test]
    fn a_bool_setting_reads_both_encodings() {
        use serde_json::json;
        for (value, want) in [
            (json!(true), true),
            (json!(false), false),
            (json!("true"), true),
            (json!("false"), false),
        ] {
            assert_eq!(bool_setting(Some(&value), !want), want, "value {value}");
        }
    }

    #[test]
    fn an_absent_or_unreadable_bool_setting_takes_the_default() {
        use serde_json::json;
        assert!(bool_setting(None, true));
        assert!(!bool_setting(None, false));
        // A shape nobody writes must not flip a default-on setting off.
        assert!(bool_setting(Some(&json!(42)), true));
        assert!(!bool_setting(Some(&json!("yes")), false));
    }

    #[tokio::test]
    async fn capture_flags_default_off_and_honour_a_string_true() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();

        let opts = capture_opts(&pool).await;
        assert!(!opts.window_title, "capture must be opt-in");
        assert!(!opts.url, "URL capture must be opt-in");

        // Exactly what the UI's toggle sends through `set_setting`.
        SettingsRepo::new(pool.clone())
            .set(kea_platform::CaptureOpts::SETTING_URL, &"true".to_string())
            .await
            .unwrap();

        assert!(
            capture_opts(&pool).await.url,
            "the toggle the UI actually writes has to turn the flag on"
        );
    }

    /// A preset replaces the prompt template outright, so a caller that names
    /// a mode and gets the saved preset anyway did not get the mode. For a
    /// translate shortcut that is the difference between translating and not.
    #[tokio::test]
    async fn forcing_a_mode_drops_the_saved_preset() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        SettingsRepo::new(pool.clone())
            .set("rewrite.translate.target", &"de".to_string())
            .await
            .unwrap();

        let mut input = RewriteInput {
            source_text: "hello".into(),
            mode: RewriteMode::Improve,
            preset_id: Some("preset-1".into()),
            custom_instruction: None,
        };
        set_rewrite_mode(&mut input, RewriteMode::Translate, &pool).await;
        assert_eq!(input.mode, RewriteMode::Translate);
        assert_eq!(input.preset_id, None);
        // And the parameter comes from the key the new mode reads, not the
        // one the old mode did.
        assert_eq!(input.custom_instruction.as_deref(), Some("de"));
    }

    /// The whole path a translate shortcut takes: the override names the mode
    /// and carries the tag, and neither the saved preset nor the saved style
    /// survives it.
    #[tokio::test]
    async fn a_translate_override_wins_over_the_saved_style_and_preset() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool.clone());
        settings
            .set("rewrite.active_mode", &"friendly".to_string())
            .await
            .unwrap();

        let mut input = default_rewrite_input(&pool).await;
        input.preset_id = Some("preset-1".into());
        RewriteOverride {
            mode: Some(RewriteMode::Translate),
            instruction: Some("pt-BR".into()),
            ..RewriteOverride::default()
        }
        .apply(&mut input, &pool)
        .await;

        assert_eq!(input.mode, RewriteMode::Translate);
        assert_eq!(input.preset_id, None);
        assert_eq!(input.custom_instruction.as_deref(), Some("pt-BR"));
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
                    .find_command(action.feature, &action.command)
                    .is_some(),
                "{}/{} is not declared by its feature",
                action.feature,
                action.command
            );
            // The ids the dispatch loop matches on are still the constants the
            // handlers name, now that they are derived rather than stored.
            assert!(
                [
                    REWRITE_ACTION_ID,
                    DICTATION_ACTION_ID,
                    TTS_ACTION_ID,
                    MEETINGS_ACTION_ID,
                    PALETTE_ACTION_ID,
                    OCR_ACTION_ID,
                    UNDO_ACTION_ID,
                ]
                .contains(&action.action_id().as_str()),
                "{} is not one of the declared action ids",
                action.action_id()
            );
            assert!(
                action.translate_target().is_none(),
                "a fixed row must not look like a translate row: {}",
                action.command
            );
        }
        let mut pairs: Vec<_> = HOTKEY_ACTIONS
            .iter()
            .map(|a| (a.feature, a.command.clone()))
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

    /// The picker needs names, not indices — and a model with no published
    /// speaker table returns nothing rather than inventing one.
    #[test]
    fn list_onnx_voices_names_the_speakers_of_a_multi_speaker_bundle() {
        let kokoro = list_onnx_voices("kokoro-en-v0.19".into());
        assert!(!kokoro.is_empty());
        assert_eq!(kokoro[0].sid, 0);
        assert!(kokoro.iter().any(|v| v.name == "bm_lewis"));
        assert!(list_onnx_voices("vits-piper-en-us-lessac-medium".into()).is_empty());
        assert!(list_onnx_voices("not-a-model".into()).is_empty());
    }

    /// Retiring a model by deleting its catalog row would make an
    /// already-downloaded copy undeletable, because this is the check
    /// `delete_model` runs first. A retired entry has to keep passing it.
    #[test]
    fn a_retired_model_can_still_be_deleted() {
        let retired = ModelRegistry::find_whisper("ggml-medium.en").expect("still catalogued");
        assert!(retired.deprecated);
        assert!(validate_model_id_for_delete(ModelKind::Whisper, "ggml-medium.en").is_ok());
        // ...and the IPC listing still carries it, flagged, so the Models page
        // can show an installed copy with a Remove button.
        assert!(list_whisper_models()
            .iter()
            .any(|m| m.id == "ggml-medium.en" && m.deprecated));
    }

    #[test]
    fn a_subtitle_export_lands_beside_its_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Team sync.m4a");
        std::fs::write(&source, b"x").unwrap();

        let target = export_destination(&source, SubtitleFormat::Srt).unwrap();
        assert_eq!(target.parent(), Some(dir.path()));
        assert_eq!(
            target.file_name().unwrap().to_string_lossy(),
            "Team sync.srt",
            "a player looks for <stem>.srt next to <stem>.m4a"
        );
        assert!(export_destination(&source, SubtitleFormat::Vtt)
            .unwrap()
            .to_string_lossy()
            .ends_with(".vtt"));
    }

    /// The read-only-volume / iCloud-placeholder case. Probed by writing,
    /// because all three of those report plausible permissions and then fail.
    #[test]
    fn an_unwritable_source_directory_falls_back_rather_than_failing() {
        let missing = Path::new("/definitely/not/a/directory/clip.mp3");
        match export_destination(missing, SubtitleFormat::Srt) {
            // With a HOME/Downloads present, the fallback is used.
            Ok(target) => assert!(target.to_string_lossy().ends_with("clip.srt")),
            // Without one, it says so rather than writing somewhere random.
            Err(e) => assert!(e.contains("writable"), "{e}"),
        }
        assert!(!is_writable_dir(Path::new("/definitely/not/a/directory")));
    }

    #[test]
    fn the_diarization_kind_has_a_storage_root_of_its_own() {
        // Every kind but whisper resolves to a root, and no two share one —
        // an exhaustive match, so a new kind is a compile error here rather
        // than a runtime "unknown kind".
        for kind in [
            ModelKind::Parakeet,
            ModelKind::Tts,
            ModelKind::Streaming,
            ModelKind::Diarization,
        ] {
            assert!(onnx_catalog_for_kind(kind).is_ok(), "{kind}");
        }
        assert!(onnx_catalog_for_kind(ModelKind::Whisper).is_err());
    }

    /// The pair is listed as installed only through the entry's own shape.
    /// With the old `tokens.txt` check both would be invisible forever.
    #[test]
    fn diarization_models_are_listed_installed_by_their_own_markers() {
        let dir = tempfile::tempdir().unwrap();
        let storage = ModelStorage::new(dir.path().to_path_buf());
        let catalog = onnx_catalog_for_kind(ModelKind::Diarization).unwrap();
        assert!(installed_onnx_model_ids(&storage, &catalog).is_empty());

        for entry in &catalog {
            let model_dir = storage.onnx_dir_for(&entry.id);
            std::fs::create_dir_all(&model_dir).unwrap();
            std::fs::write(model_dir.join(entry.bundle.marker()), b"w").unwrap();
        }
        let installed = installed_onnx_model_ids(&storage, &catalog);
        assert_eq!(installed.len(), 2, "{installed:?}");
        assert!(!dir.path().join(&catalog[1].id).join("tokens.txt").exists());
    }

    #[test]
    fn onnx_catalog_for_kind_returns_parakeet_and_tts() {
        let parakeet = onnx_catalog_for_kind(ModelKind::Parakeet).unwrap();
        assert!(!parakeet.is_empty());
        let tts = onnx_catalog_for_kind(ModelKind::Tts).unwrap();
        assert!(!tts.is_empty());
        let streaming = onnx_catalog_for_kind(ModelKind::Streaming).unwrap();
        assert_eq!(streaming.len(), 1);
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
        assert_eq!(parse_perm_kind("calendar"), Ok(PermKind::Calendar));
        assert_eq!(parse_perm_kind("speech"), Ok(PermKind::Speech));
        assert!(parse_perm_kind("camera").is_err());
    }

    #[test]
    fn all_permission_statuses_lists_every_kind() {
        let permissions = kea_platform::new_permissions();
        let items = all_permission_statuses(permissions.as_ref());
        assert_eq!(items.len(), PERM_KINDS.len());
        assert!(items.iter().any(|i| i.kind == "microphone"));
        assert!(items.iter().any(|i| i.kind == "screen_recording"));
        assert!(items.iter().any(|i| i.kind == "accessibility"));
        assert!(items.iter().any(|i| i.kind == "calendar"));
        assert!(items.iter().any(|i| i.kind == "speech"));
    }

    /// The table is the only place a kind is spelled, so the two commands and
    /// the status list cannot disagree about what exists. Pinning the length
    /// here is what catches a row added to `PERM_KINDS` without the matching
    /// `"calendar"`-style entry in `ui/src/api.ts` and `PermissionPanel`.
    #[test]
    fn every_perm_kind_round_trips_through_its_wire_name() {
        assert_eq!(PERM_KINDS.len(), 5);
        for (name, kind) in PERM_KINDS {
            assert_eq!(parse_perm_kind(name), Ok(kind), "{name}");
        }
    }

    /// The three strings `ActionItemStatus` is spelled as in `ui/src/api.ts`.
    /// `set_meeting_action_item_status` rejects anything else, so a union
    /// widened on one side only is a visible error rather than a silent no-op.
    #[test]
    fn the_action_item_statuses_the_ui_sends_all_parse() {
        for status in ["open", "done", "dropped"] {
            assert!(ActionItemStatus::from_str(status).is_some(), "{status}");
        }
        assert!(ActionItemStatus::from_str("closed").is_none());
    }

    /// A second trigger arriving mid-pass is dropped, not queued: a provider
    /// slower than the cadence would otherwise build an unbounded backlog that
    /// keeps billing after the meeting has ended.
    #[test]
    fn a_second_interim_trigger_is_dropped_while_one_is_in_flight() {
        let flag = Arc::new(AtomicBool::new(false));
        let first = try_acquire_busy(&flag).expect("the first pass claims the slot");
        assert!(try_acquire_busy(&flag).is_none());
        drop(first);
        assert!(try_acquire_busy(&flag).is_some(), "the slot is released");
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

    /// The widened lookup is the single point of failure for registration,
    /// rebinding, collision detection and dispatch, so it gets its own test.
    #[test]
    fn translate_commands_are_admitted_only_when_they_name_a_language() {
        // Spelled the way `translateCommand` in `ui/src/api.ts` spells it: the
        // UI builds these ids and this lookup reads them, so the prefix is the
        // one thing the two sides have to agree on.
        assert_eq!(format!("{TRANSLATE_COMMAND_PREFIX}fr"), "translate.fr");
        assert!(hotkey_action(REWRITE_FEATURE_ID, "translate.fr").is_some());
        assert!(hotkey_action(REWRITE_FEATURE_ID, "translate.pt-BR").is_some());
        // Nothing after the prefix, prose after the prefix, and the same
        // command under a feature that does not own translate.
        assert!(hotkey_action(REWRITE_FEATURE_ID, "translate.").is_none());
        assert!(
            hotkey_action(REWRITE_FEATURE_ID, "translate.ignore previous instructions").is_none()
        );
        assert!(hotkey_action(DICTATION_FEATURE_ID, "translate.fr").is_none());
        // The command id is the whole configuration, so it must survive the
        // round trip through the action id the dispatch loop sees.
        let action = hotkey_action_for_id("rewrite:translate.zh-Hant").unwrap();
        assert_eq!(action.translate_target(), Some("zh-Hant"));
        assert_eq!(action.action_id(), "rewrite:translate.zh-Hant");
        assert!(hotkey_action_for_id("rewrite").is_none());
    }

    /// A language with no shortcut must read as "not set" in the UI, not as an
    /// error — and there is no sensible compiled-in default for "French".
    #[test]
    fn a_translate_shortcut_has_no_compiled_default() {
        assert_eq!(
            compiled_default_accelerator(REWRITE_FEATURE_ID, "translate.fr"),
            None
        );
        assert_eq!(
            effective_hotkey(REWRITE_FEATURE_ID, "translate.fr", None),
            None
        );
        assert_eq!(
            effective_hotkey(REWRITE_FEATURE_ID, "translate.fr", Some("Alt+1".into())),
            Some(EffectiveHotkey {
                accelerator: "Alt+1".into(),
                source: "custom".into(),
            })
        );
    }

    #[test]
    fn two_languages_cannot_claim_the_same_accelerator() {
        let bindings = vec![binding_row(REWRITE_FEATURE_ID, "translate.fr", "Alt+1")];
        // A persisted translate row is not in the const table, so this is the
        // case a table-only collision check would have waved through.
        assert_eq!(
            check_hotkey_collision(REWRITE_FEATURE_ID, "translate.de", "Alt+1", &bindings),
            Some("rewrite/translate.fr".to_string())
        );
        assert_eq!(
            check_hotkey_collision(REWRITE_FEATURE_ID, "translate.de", "Alt+2", &bindings),
            None
        );
        // And it still collides with the fixed rows, in both directions.
        let rewrite_default =
            compiled_default_accelerator(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID).unwrap();
        assert_eq!(
            check_hotkey_collision(REWRITE_FEATURE_ID, "translate.de", &rewrite_default, &[]),
            Some(format!("{REWRITE_FEATURE_ID}/{REWRITE_COMMAND_ID}"))
        );
        assert_eq!(
            check_hotkey_collision(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+1", &bindings),
            Some("rewrite/translate.fr".to_string())
        );
    }

    /// A rebind must hand the old combo back to the translate row that still
    /// owns it, exactly as it does for a fixed row — otherwise the language
    /// keeps a DB row and loses its registration.
    #[test]
    fn old_hotkey_action_reassigns_to_a_persisted_translate_owner() {
        let bindings = vec![
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K"),
            binding_row(REWRITE_FEATURE_ID, "translate.fr", "Cmd+Shift+R"),
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
                feature_id: REWRITE_FEATURE_ID.into(),
                command: "translate.fr".into(),
            }
        );
    }

    #[test]
    fn hotkey_owners_adds_persisted_rows_without_duplicating_the_table() {
        let bindings = vec![
            // Already in the table: must not appear twice.
            binding_row(REWRITE_FEATURE_ID, REWRITE_COMMAND_ID, "Alt+K"),
            binding_row(REWRITE_FEATURE_ID, "translate.fr", "Alt+1"),
            // Not a hotkey at all: a row persisted for some other command.
            binding_row("somewhere", "else", "Alt+9"),
        ];
        let owners = hotkey_owners(&bindings);
        assert_eq!(owners.len(), HOTKEY_ACTIONS.len() + 1);
        assert_eq!(
            owners
                .iter()
                .filter(|a| a.command == REWRITE_COMMAND_ID)
                .count(),
            1
        );
        assert!(owners.iter().any(|a| a.translate_target() == Some("fr")));
    }

    #[test]
    fn notion_setup_reports_a_bad_link_but_not_a_missing_one() {
        // Nothing typed yet is the state a fresh install is in, not a fault.
        let fresh = notion_status(false, String::new());
        assert!(!fresh.has_token);
        assert_eq!(fresh.parent_page_error, None);

        let good = notion_status(
            true,
            "https://www.notion.so/Notes-0123456789abcdef0123456789abcdef".into(),
        );
        assert_eq!(good.parent_page_error, None);

        // The same parser the export uses, so the screen cannot approve a link
        // the export would refuse.
        let bad = notion_status(true, "my notion page".into());
        assert!(bad
            .parent_page_error
            .unwrap()
            .contains("does not look like a Notion page link"));
        // Whitespace only is still "unset", not a typo to be told off about.
        assert_eq!(notion_status(true, "   ".into()).parent_page_error, None);
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

    // -----------------------------------------------------------------
    // Prompt palette
    // -----------------------------------------------------------------

    /// Records which delivery a run actually reached for. Nothing here touches
    /// a window server, a clipboard or an app: the question these tests ask is
    /// "which of the three paths ran", and that is answerable at the `TextIo`
    /// seam, which is the same one `crates/features/src/rewrite.rs` uses.
    struct RecordingTextIo {
        /// Answers returned by successive `capture_selection` calls. The
        /// verify step is a *second* read, so the two have to be separable.
        selections: Mutex<Vec<Result<String, String>>>,
        replaced: Mutex<Option<String>>,
        inserted: Mutex<Option<String>>,
        captures: AtomicUsize,
    }

    impl RecordingTextIo {
        fn with(selections: Vec<Result<String, String>>) -> Self {
            Self {
                selections: Mutex::new(selections),
                replaced: Mutex::new(None),
                inserted: Mutex::new(None),
                captures: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl kea_platform::TextIo for RecordingTextIo {
        async fn capture_selection(&self) -> Result<String, kea_platform::TextIoError> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            let mut queue = self.selections.lock().unwrap();
            let next = if queue.is_empty() {
                Err("no selection".to_string())
            } else {
                queue.remove(0)
            };
            next.map_err(kea_platform::TextIoError::Other)
        }

        async fn replace_with_mode(
            &self,
            text: &str,
            _mode: kea_platform::ReplaceMode,
        ) -> Result<(), kea_platform::TextIoError> {
            *self.replaced.lock().unwrap() = Some(text.to_string());
            Ok(())
        }

        async fn insert_at_cursor(&self, text: &str) -> Result<(), kea_platform::TextIoError> {
            *self.inserted.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeClipboard {
        contents: Mutex<Option<String>>,
    }

    impl ClipboardSink for FakeClipboard {
        fn copy(&self, text: &str) -> Result<(), String> {
            *self.contents.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    async fn palette_test_pool() -> SqlitePool {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        pool
    }

    async fn deliver(
        textio: &RecordingTextIo,
        clipboard: &FakeClipboard,
        delivery: PaletteDelivery,
        source: &str,
        verify: bool,
        reactivation: crate::macfocus::Reactivation,
    ) -> PaletteOutcome {
        deliver_palette_result(
            textio,
            clipboard,
            delivery,
            "the answer",
            source,
            verify,
            reactivation,
            kea_platform::ReplaceMode::ClipboardPaste,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn copy_delivery_types_nothing_anywhere() {
        let textio = RecordingTextIo::with(vec![]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Copy,
            "source",
            true,
            crate::macfocus::Reactivation::Active,
        )
        .await;

        assert_eq!(outcome.delivered, "copy");
        assert_eq!(
            clipboard.contents.lock().unwrap().as_deref(),
            Some("the answer")
        );
        assert!(textio.replaced.lock().unwrap().is_none());
        assert!(textio.inserted.lock().unwrap().is_none());
        // Not even the verify read: there is no selection involved in a copy.
        assert_eq!(textio.captures.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn insert_delivery_goes_to_the_caret() {
        let textio = RecordingTextIo::with(vec![]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Insert,
            "source",
            true,
            crate::macfocus::Reactivation::Active,
        )
        .await;

        assert_eq!(outcome.delivered, "insert");
        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some("the answer")
        );
        assert!(textio.replaced.lock().unwrap().is_none());
        assert!(clipboard.contents.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn replace_writes_over_a_selection_that_is_still_there() {
        let textio = RecordingTextIo::with(vec![Ok("the original".into())]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Replace,
            "the original",
            true,
            crate::macfocus::Reactivation::Active,
        )
        .await;

        assert_eq!(outcome.delivered, "replace");
        assert_eq!(
            outcome.message, None,
            "nothing to explain on the happy path"
        );
        assert_eq!(
            textio.replaced.lock().unwrap().as_deref(),
            Some("the answer")
        );
        assert_eq!(textio.captures.load(Ordering::SeqCst), 1);
    }

    /// The guard against the worst outcome this feature can produce: replacing
    /// a span the user never selected, because the app dropped the selection
    /// when the palette took focus.
    #[tokio::test]
    async fn replace_downgrades_to_insert_when_the_selection_moved() {
        let textio = RecordingTextIo::with(vec![Ok("something else entirely".into())]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Replace,
            "the original",
            true,
            crate::macfocus::Reactivation::Active,
        )
        .await;

        assert_eq!(outcome.delivered, "insert");
        assert!(textio.replaced.lock().unwrap().is_none());
        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some("the answer")
        );
        // And the user is told, because the result is not where they expected.
        assert!(outcome.message.is_some());
    }

    #[tokio::test]
    async fn a_selection_that_cannot_be_reread_is_treated_as_gone() {
        // Apps that clear the selection on resignFirstResponder answer the
        // verify Cmd+C with a failure rather than with different text.
        let textio = RecordingTextIo::with(vec![Err("no selection".into())]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Replace,
            "the original",
            true,
            crate::macfocus::Reactivation::Active,
        )
        .await;
        assert_eq!(outcome.delivered, "insert");
    }

    #[tokio::test]
    async fn verification_can_be_turned_off() {
        let textio = RecordingTextIo::with(vec![Ok("something else".into())]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Replace,
            "the original",
            false,
            crate::macfocus::Reactivation::Active,
        )
        .await;

        assert_eq!(outcome.delivered, "replace");
        // The point of the setting: no second Cmd+C, so no extra 150 ms.
        assert_eq!(textio.captures.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_target_that_never_came_back_gets_the_clipboard_instead() {
        // The app quit, hung, or the machine slept while the palette was up.
        // Typing now would land in whatever *is* frontmost.
        for delivery in [PaletteDelivery::Replace, PaletteDelivery::Insert] {
            let textio = RecordingTextIo::with(vec![Ok("the original".into())]);
            let clipboard = FakeClipboard::default();
            let outcome = deliver(
                &textio,
                &clipboard,
                delivery,
                "the original",
                true,
                crate::macfocus::Reactivation::Failed,
            )
            .await;

            assert_eq!(outcome.delivered, "copy", "{delivery:?}");
            assert!(textio.replaced.lock().unwrap().is_none());
            assert!(textio.inserted.lock().unwrap().is_none());
            assert_eq!(
                clipboard.contents.lock().unwrap().as_deref(),
                Some("the answer")
            );
            assert!(outcome.message.is_some(), "the user has to be told");
        }
    }

    #[tokio::test]
    async fn an_unknown_reactivation_is_not_treated_as_success() {
        // No pid was captured (no GUI session, or not macOS). Optimism here
        // pastes into whatever the user happens to be looking at.
        let textio = RecordingTextIo::with(vec![]);
        let clipboard = FakeClipboard::default();
        let outcome = deliver(
            &textio,
            &clipboard,
            PaletteDelivery::Insert,
            "",
            true,
            crate::macfocus::Reactivation::Unknown,
        )
        .await;
        assert_eq!(outcome.delivered, "copy");
    }

    #[tokio::test]
    async fn the_palette_is_a_rewrite_hotkey_row_not_a_feature_of_its_own() {
        // The accelerator falls back to `RewriteFeature`'s declared default,
        // so a palette registered under some other feature id would register
        // an empty accelerator and the key would silently be dead.
        for action in HOTKEY_ACTIONS {
            let action_id = action.action_id();
            if action_id == PALETTE_ACTION_ID || action_id == OCR_ACTION_ID {
                assert_eq!(action.feature, REWRITE_FEATURE_ID);
                assert!(
                    compiled_default_accelerator(action.feature, &action.command).is_some(),
                    "{action_id} has no compiled default"
                );
            }
        }
    }

    #[tokio::test]
    async fn ocr_options_default_to_vision_choosing_the_language() {
        let pool = palette_test_pool().await;
        let opts = ocr_options(&pool).await;
        assert!(
            opts.languages.is_empty(),
            "a guessed list is worse than none"
        );
        assert!(opts.language_correction);
    }

    #[tokio::test]
    async fn ocr_options_read_the_settings_rows() {
        let pool = palette_test_pool().await;
        let settings = SettingsRepo::new(pool.clone());
        settings
            .set(OCR_LANGUAGES_SETTING, &"en-US, ja ,".to_string())
            .await
            .unwrap();
        // Written the way the generic `set_setting` command writes it: a JSON
        // string, not a JSON bool. Reading this with a plain typed get is the
        // trap that shipped two other toggles inert.
        settings
            .set(OCR_LANGUAGE_CORRECTION_SETTING, &"false".to_string())
            .await
            .unwrap();

        let opts = ocr_options(&pool).await;
        assert_eq!(opts.languages, vec!["en-US".to_string(), "ja".to_string()]);
        assert!(!opts.language_correction);
    }

    #[tokio::test]
    async fn instruction_history_is_on_by_default_and_follows_the_content_switch() {
        let pool = palette_test_pool().await;
        assert!(palette_history_enabled(&pool).await);

        let settings = SettingsRepo::new(pool.clone());
        // Turning off content storage takes instructions with it: an
        // instruction is content ("rewrite this rejection letter for Bob").
        settings
            .set("history.store_conversations", &"false".to_string())
            .await
            .unwrap();
        assert!(!palette_history_enabled(&pool).await);

        settings
            .set("history.store_conversations", &"true".to_string())
            .await
            .unwrap();
        settings
            .set(STORE_HISTORY_SETTING, &"false".to_string())
            .await
            .unwrap();
        assert!(!palette_history_enabled(&pool).await);
    }

    // =======================================================================
    // Undo, usage and rates
    // =======================================================================

    fn outcome(source: &str, text: &str) -> kea_features::RewriteOutcome {
        kea_features::RewriteOutcome {
            text: text.into(),
            source_text: source.into(),
        }
    }

    fn offer(inserted: &str, original: &str, age: Duration) -> UndoOffer {
        UndoOffer {
            inserted: inserted.into(),
            original: original.into(),
            target_pid: Some(1234),
            // Subtracting is how an old offer is built without sleeping for
            // two minutes in a unit test.
            made_at: std::time::Instant::now()
                .checked_sub(age)
                .expect("the test clock is not near the epoch"),
        }
    }

    #[test]
    fn a_rewrite_that_replaced_a_selection_is_undoable() {
        assert!(is_undoable(&outcome(
            "i think we should ship",
            "I think we should ship."
        )));
    }

    #[test]
    fn an_insertion_with_no_selection_is_not_offered() {
        // Undoing this would mean deleting what was inserted, which the swap
        // cannot express — see `is_undoable`.
        assert!(!is_undoable(&outcome("", "Some fresh text.")));
    }

    #[test]
    fn a_rewrite_that_changed_nothing_is_not_offered() {
        assert!(!is_undoable(&outcome(
            "Already perfect.",
            "Already perfect."
        )));
    }

    #[test]
    fn there_is_nothing_to_undo_before_the_first_rewrite() {
        let err = undo_target(None).unwrap_err();
        assert!(err.contains("no recent rewrite"), "{err}");
    }

    #[test]
    fn a_fresh_offer_is_the_one_to_act_on() {
        let fresh = offer("new", "old", Duration::from_secs(1));
        assert_eq!(undo_target(Some(&fresh)).unwrap().original, "old");
    }

    #[test]
    fn an_expired_offer_is_refused_with_a_reason() {
        // The guard the undo leans on is "find the text KEA wrote, exactly
        // once", and that stops being trustworthy as the user keeps typing.
        let stale = offer("new", "old", UNDO_WINDOW + Duration::from_secs(1));
        let err = undo_target(Some(&stale)).unwrap_err();
        assert!(err.contains("too old"), "{err}");
    }

    #[test]
    fn the_undo_window_is_long_enough_to_read_the_result() {
        // Short enough to be a reaction to reading, long enough not to expire
        // mid-sentence. Both bounds are the point; see `UNDO_WINDOW`.
        assert!(UNDO_WINDOW >= Duration::from_secs(30));
        assert!(UNDO_WINDOW <= Duration::from_secs(600));
    }

    #[test]
    fn the_undo_shortcut_does_not_steal_the_systems_own_undo() {
        // Cmd+Z and Cmd+Shift+Z belong to whichever app is in front. A global
        // registration of either would take them away everywhere.
        let accel = compiled_default_accelerator(REWRITE_FEATURE_ID, UNDO_COMMAND).unwrap();
        assert!(
            !accel.ends_with("Z"),
            "{accel} collides with the app's own undo"
        );
        assert!(
            validate_accelerator(&accel).is_ok(),
            "{accel} does not parse"
        );
    }

    #[test]
    fn the_usage_window_is_clamped_to_something_sql_can_take() {
        // The number reaches printf('-%d days', ?): zero or negative asks for
        // rows from the future and silently returns none.
        assert_eq!(usage_window(30), 30);
        assert_eq!(usage_window(0), 1);
        assert_eq!(usage_window(-7), 1);
        assert_eq!(usage_window(i64::MAX), 3650);
    }
}
