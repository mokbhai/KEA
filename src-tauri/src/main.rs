#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod hotkeys;
mod overlay;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

use kea_core::rewrite::{CredentialSourceAdapter, ProviderConfigRepo};
use kea_core::secrets::KeyringCredentialStore;
use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
use kea_core::store::meetings::MeetingRepo;
use kea_core::store::settings::SettingsRepo;
use kea_engines::{
    register_phase1_engines, register_phase2_stt_engines, register_phase4_tts_engines,
    EngineRegistry, ReqwestHttpClient,
};
use kea_features::FeatureRegistry;
use kea_infer::ModelStorage;
use kea_platform::{new_audio_io, new_hotkeys, new_permissions};
use sqlx::SqlitePool;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, Wry};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::commands::{feature_registry, ActiveMeetingSession, HotkeyRegStatus};

/// A download in flight: what the UI is waiting on, plus everything needed to
/// stop it. Aborting drops the transfer mid-write, so the partial file has to
/// be removed by whoever cancels — nothing else will.
pub struct ActiveDownload {
    pub model_id: String,
    pub temp_path: PathBuf,
    pub task: tauri::async_runtime::JoinHandle<()>,
}

pub struct AppState {
    pub engines: EngineRegistry,
    /// The registered features, shared with [`commands::feature_registry`]:
    /// immutable after startup, and the hotkey defaults are read off it from
    /// pure helpers that never see this state.
    pub features: &'static FeatureRegistry,
    pub config_pool: SqlitePool,
    pub data_pool: SqlitePool,
    pub meeting_repo: MeetingRepo,
    pub credentials: Arc<dyn kea_core::secrets::CredentialStore>,
    pub permissions: Box<dyn kea_platform::Permissions>,
    pub hotkeys: Mutex<Box<dyn kea_platform::Hotkeys>>,
    pub model_storage: ModelStorage,
    pub parakeet_storage: ModelStorage,
    pub tts_storage: ModelStorage,
    pub log_dir: PathBuf,
    /// Shared mic/meeting capture (mutually exclusive with dictation).
    pub audio: AsyncMutex<Box<dyn kea_platform::AudioIo>>,
    pub active_meeting: Mutex<Option<ActiveMeetingSession>>,
    pub level_poll_cancel: Mutex<Option<watch::Sender<bool>>>,
    pub segment_poll_cancel: Mutex<Option<watch::Sender<bool>>>,
    /// Per-feature hotkey registration outcomes recorded at startup; updated
    /// on re-registration via `set_hotkey`.
    pub hotkey_reg_status: Mutex<HashMap<String, HotkeyRegStatus>>,
    /// Downloads in flight, keyed by [`crate::commands::download_key`]. Guards
    /// against starting the same model twice, and holds what `cancel_model_download`
    /// needs to stop one.
    pub active_downloads: Mutex<HashMap<String, ActiveDownload>>,
    /// Dictation in-flight run tracking: (generation counter, current run id).
    /// The counter guards against stale emits: a newer run's id is larger.
    pub dictation_run_counter: AtomicU64,
    pub dictation_current_run: Mutex<Option<u64>>,
    /// The app that was frontmost when the current dictation run started.
    ///
    /// Captured at the start rather than read at the end: by the time a
    /// transcript is ready the user may have switched apps, and a profile
    /// resolved against the wrong app would rewrite into it with the wrong
    /// settings. Cleared with the run.
    pub dictation_app_context: Mutex<Option<kea_platform::AppContext>>,
    /// True while a voice preview is synthesizing or playing. Playback pins a
    /// blocking-pool thread and mixes with anything already playing, so a
    /// second preview is refused rather than overlaid.
    pub preview_playing: AtomicBool,
    /// True while a meeting stop is in its post-capture processing window
    /// (STT + synthesis, after `active_meeting` was taken). Consulted so a
    /// hotkey press can't park on the audio lock and replay as a fresh start.
    pub meeting_processing: AtomicBool,
    /// Serialises dictation handlers so a trigger arriving during one is
    /// dropped rather than queued. Shared state rather than a local of the
    /// hotkey loop because hold-to-talk drives the same handlers from its own
    /// task, and the two must not be able to start a run each.
    pub dictation_busy: Arc<AtomicBool>,
    /// Whether ⌥⇧ hold-to-talk is armed. Read by the platform listener on every
    /// event, so the settings toggle takes effect immediately.
    pub hold_to_talk_enabled: Arc<AtomicBool>,
    /// Whether the hold-to-talk listener has been installed. It cannot be taken
    /// back down (see `kea_platform::hotkeys::macos_hold`), so this guards
    /// against installing a second one — while still allowing a retry after the
    /// first attempt failed for want of Accessibility permission.
    pub hold_to_talk_installed: Mutex<bool>,
    /// A handle on the installed hold machine, so a lock can be ended by
    /// something other than the keyboard tap: Escape, or a run that finished
    /// by another route. `None` until the listener installs.
    pub hold_control: Mutex<Option<kea_platform::hotkeys::HoldControl>>,
    /// Whether to open the microphone early on the first modifier of the hold
    /// chord. Read on every modifier edge, so the settings toggle takes effect
    /// without a restart.
    pub preroll_enabled: Arc<AtomicBool>,
    /// Whether the running dictation is a double-tap lock rather than a hold.
    /// The capture device cannot tell them apart; this is what makes the HUD,
    /// the hotkey gate and Escape able to.
    pub dictation_locked: AtomicBool,
    /// Bumped whenever the input preview starts or stops, so a 30s auto-stop
    /// timer can tell whether it is still about the preview it was armed for.
    pub preview_generation: AtomicU64,
}

fn on_tray_menu_event(app: &tauri::AppHandle, e: tauri::menu::MenuEvent) {
    if e.id() == "open" {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    } else if e.id() == "quit" {
        app.exit(0);
    }
}

fn handle_migration_error(msg: &str, app: &tauri::App<Wry>) -> ! {
    tracing::error!("{msg}");
    eprintln!("{msg}");
    eprintln!("The database may be corrupt. Try renaming or deleting the .db files in the app data directory and restarting KEA.");
    let _ = app
        .notification()
        .builder()
        .title("KEA startup failed")
        .body(msg)
        .show();
    std::process::exit(1);
}

fn main() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init());

    // Updater plugin: gated behind `cfg(feature = "updater")` so the
    // default build stays green without a signing key. See docs/RELEASE.md.
    #[cfg(feature = "updater")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());

    builder
        .setup(setup)
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // A microphone test the user walked away from holds the input
                // device and keeps the macOS orange indicator lit, which looks
                // exactly like the app recording behind their back. Losing the
                // window is as clear a signal to stop as the 30s timer.
                tauri::WindowEvent::Focused(false) if window.label() == "main" => {
                    let app = window.app_handle().clone();
                    if let Some(state) = app.try_state::<Arc<AppState>>() {
                        let state = state.inner().clone();
                        let for_task = app.clone();
                        tauri::async_runtime::spawn(async move {
                            commands::stop_input_preview_inner(&state, &for_task).await;
                        });
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_engines,
            commands::list_llm_engines,
            commands::list_stt_engines,
            commands::list_features,
            commands::get_setting,
            commands::set_setting,
            commands::get_binding,
            commands::set_binding,
            commands::delete_binding,
            commands::get_provider_config,
            commands::set_provider_config,
            commands::set_credential,
            commands::delete_credential,
            commands::has_credential,
            commands::test_provider,
            commands::list_providers,
            commands::add_custom_provider,
            commands::update_custom_provider,
            commands::remove_custom_provider,
            commands::list_presets,
            commands::upsert_preset,
            commands::delete_preset,
            commands::list_app_profiles,
            commands::upsert_app_profile,
            commands::delete_app_profile,
            commands::capture_app_context,
            commands::list_vocabulary,
            commands::upsert_vocabulary_entry,
            commands::delete_vocabulary_entry,
            commands::preview_vocabulary,
            commands::get_prompt_override,
            commands::set_prompt_override,
            commands::get_hotkey,
            commands::get_effective_hotkey,
            commands::get_hotkey_registration_status,
            commands::set_hotkey,
            commands::trigger_rewrite,
            commands::preview_rewrite,
            commands::run_demo,
            commands::list_whisper_models,
            commands::list_installed_whisper_models,
            commands::download_whisper_model,
            commands::get_dictation_settings,
            commands::set_dictation_settings,
            commands::set_dictation_stt_binding,
            commands::start_dictation,
            commands::stop_dictation,
            commands::list_input_devices,
            commands::start_input_preview,
            commands::stop_input_preview,
            commands::get_meeting_settings,
            commands::set_meeting_settings,
            commands::get_system_audio_capability,
            commands::get_permission_status,
            commands::request_permission,
            commands::get_all_permission_statuses,
            commands::open_accessibility_settings,
            commands::list_meetings,
            commands::get_meeting,
            commands::delete_meeting,
            commands::start_meeting,
            commands::stop_meeting,
            commands::get_meeting_state,
            commands::get_dictation_state,
            commands::list_actions,
            commands::get_action,
            commands::list_conversations,
            commands::list_messages,
            commands::delete_conversation,
            commands::tail_logs,
            commands::open_log_folder,
            commands::list_tts_engines,
            commands::set_tts_binding,
            commands::get_tts_settings,
            commands::set_tts_settings,
            commands::run_read_aloud,
            commands::trigger_tts,
            commands::read_selection,
            commands::list_onnx_models,
            commands::list_onnx_voices,
            commands::list_system_voices,
            commands::list_installed_onnx_models,
            commands::download_onnx_model,
            commands::cancel_model_download,
            commands::delete_model,
            commands::preview_voice,
            commands::set_autostart,
            commands::get_autostart,
            commands::show_notification,
            commands::check_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building KEA")
        .run(|app_handle, event| {
            // RunEvent::Reopen (Dock icon click) only exists on macOS.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app_handle.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app_handle, event);
        });
}

/// The model directories the engines read from.
struct ModelStorages {
    whisper: ModelStorage,
    parakeet: ModelStorage,
    tts: ModelStorage,
}

/// A model directory, created up front so a download does not have to. A
/// failure here is not fatal: the directory is created again on first use, and
/// the warning is what tells us why a download later failed.
fn ensure_storage(root: PathBuf, what: &str) -> ModelStorage {
    let storage = ModelStorage::new(root);
    storage.ensure_root().unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %storage.root.display(), "failed to create {what} model directory at startup");
    });
    storage
}

/// Unwrap, or take the app down the way a broken DB has to — see
/// [`handle_migration_error`].
fn or_abort<T, E: std::fmt::Display>(result: Result<T, E>, app: &tauri::App<Wry>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(e) => handle_migration_error(&format!("{what}: {e}"), app),
    }
}

/// Open both databases and bring them up to date. Config first: it holds the
/// settings every other subsystem reads.
fn open_databases(app: &tauri::App<Wry>, dir: &Path) -> (SqlitePool, SqlitePool) {
    let config_path = dir.join("config.db");
    let data_path = dir.join("data.db");
    let config_url = format!("sqlite://{}?mode=rwc", config_path.display());
    let data_url = format!("sqlite://{}?mode=rwc", data_path.display());

    tauri::async_runtime::block_on(async {
        let config = or_abort(
            open_pool(&config_url).await,
            app,
            &format!("failed to open config DB at {}", config_path.display()),
        );
        or_abort(
            run_config_migrations(&config).await,
            app,
            &format!("config DB migration failed for {}", config_path.display()),
        );
        let data = or_abort(
            open_pool(&data_url).await,
            app,
            &format!("failed to open data DB at {}", data_path.display()),
        );
        or_abort(
            run_data_migrations(&data).await,
            app,
            &format!("data DB migration failed for {}", data_path.display()),
        );
        (config, data)
    })
}

/// Build the engine registry and the model directories it reads from.
///
/// The `#[cfg(feature = ...)]` blocks are not interchangeable: each registers a
/// differently-shaped local engine behind a real cargo feature split, so they
/// stay written out.
fn build_engines(
    dir: &Path,
    config_pool: &SqlitePool,
    credentials: &Arc<dyn kea_core::secrets::CredentialStore>,
) -> (EngineRegistry, ModelStorages) {
    let creds = Arc::new(CredentialSourceAdapter::new(credentials.clone()));
    let provider_configs = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(
        config_pool.clone(),
    )));

    let http = Arc::new(ReqwestHttpClient::new());

    let mut engines = EngineRegistry::default();
    register_phase1_engines(
        &mut engines,
        http.clone(),
        creds.clone(),
        provider_configs.clone(),
    );
    register_phase2_stt_engines(
        &mut engines,
        http.clone(),
        creds.clone(),
        provider_configs.clone(),
    );
    register_phase4_tts_engines(&mut engines, http, creds.clone(), provider_configs);

    let storages = ModelStorages {
        whisper: ensure_storage(ModelStorage::default_whisper_root(dir), "whisper"),
        parakeet: ensure_storage(ModelStorage::default_parakeet_root(dir), "parakeet"),
        tts: ensure_storage(ModelStorage::default_tts_root(dir), "tts"),
    };

    #[cfg(feature = "whisper")]
    {
        use kea_engines::register_whisper_stt_engine;
        use kea_infer::WhisperRsInference;
        let whisper_storage = Arc::new(ModelStorage::new(storages.whisper.root.clone()));
        register_whisper_stt_engine(
            &mut engines,
            Arc::new(WhisperRsInference::new()),
            whisper_storage,
        );
    }

    #[cfg(feature = "parakeet")]
    {
        use kea_engines::register_parakeet_stt_engine;
        use kea_infer::SherpaOnnxSttInference;
        let storage = Arc::new(ModelStorage::new(storages.parakeet.root.clone()));
        register_parakeet_stt_engine(
            &mut engines,
            Arc::new(SherpaOnnxSttInference::new()),
            storage,
        );
    }

    #[cfg(feature = "tts-local")]
    {
        use kea_engines::register_sherpa_tts_engine;
        use kea_infer::SherpaOnnxTtsInference;
        let storage = Arc::new(ModelStorage::new(storages.tts.root.clone()));
        register_sherpa_tts_engine(
            &mut engines,
            Arc::new(SherpaOnnxTtsInference::new()),
            storage,
        );
    }

    // The OS synthesizer needs nothing downloaded, so it is registered
    // wherever there is one to register — and only there: off macOS the
    // platform layer has only a stub, and an engine in the picker that can
    // exclusively refuse is worse than no engine at all.
    #[cfg(all(feature = "tts-system", target_os = "macos"))]
    {
        use kea_engines::register_system_tts_engine;
        register_system_tts_engine(&mut engines, Arc::from(kea_platform::new_system_tts()));
    }

    (engines, storages)
}

/// Startup cleanup: prune history tables and old log files older than 90 days.
fn spawn_startup_maintenance(data_pool: &SqlitePool, log_dir: &Path) {
    let data_pool = data_pool.clone();
    let log_dir = log_dir.to_path_buf();
    tauri::async_runtime::spawn(async move {
        let actions = kea_core::store::actions::ActionRepo::new(data_pool.clone());
        match actions.prune_older_than_days(90).await {
            Ok(n) if n > 0 => tracing::info!(pruned = n, "pruned old action rows"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "failed to prune action history"),
        }
        let conversations =
            kea_core::store::conversations::ConversationRepo::new(data_pool.clone());
        match conversations.prune_older_than_days(90).await {
            Ok(n) if n > 0 => tracing::info!(pruned = n, "pruned old conversations"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "failed to prune conversation history"),
        }
        let meetings = kea_core::store::meetings::MeetingRepo::new(data_pool);
        match meetings.prune_older_than_days(90).await {
            Ok(n) if n > 0 => tracing::info!(pruned = n, "pruned old meetings"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "failed to prune meeting history"),
        }
        match kea_core::log::prune_old_logs(&log_dir, 90) {
            Ok(n) if n > 0 => tracing::info!(pruned = n, "pruned old log files"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "failed to prune old log files"),
        }
    });
}

/// Assemble the shared application state. The platform handles (hotkeys, audio,
/// permissions) are constructed here so the composition root does not hold
/// half-built state.
fn build_state(
    engines: EngineRegistry,
    storages: ModelStorages,
    config_pool: SqlitePool,
    data_pool: SqlitePool,
    credentials: Arc<dyn kea_core::secrets::CredentialStore>,
    log_dir: PathBuf,
) -> Arc<AppState> {
    let meeting_repo = MeetingRepo::new(data_pool.clone());
    Arc::new(AppState {
        engines,
        features: feature_registry(),
        config_pool,
        data_pool,
        meeting_repo,
        credentials,
        permissions: new_permissions(),
        hotkeys: Mutex::new(new_hotkeys()),
        model_storage: storages.whisper,
        parakeet_storage: storages.parakeet,
        tts_storage: storages.tts,
        log_dir,
        audio: AsyncMutex::new(new_audio_io()),
        active_meeting: Mutex::new(None),
        level_poll_cancel: Mutex::new(None),
        segment_poll_cancel: Mutex::new(None),
        hotkey_reg_status: Mutex::new(HashMap::new()),
        active_downloads: Mutex::new(HashMap::new()),
        dictation_run_counter: AtomicU64::new(0),
        dictation_current_run: Mutex::new(None),
        dictation_app_context: Mutex::new(None),
        preview_playing: AtomicBool::new(false),
        meeting_processing: AtomicBool::new(false),
        dictation_busy: Arc::new(AtomicBool::new(false)),
        hold_to_talk_enabled: Arc::new(AtomicBool::new(false)),
        hold_to_talk_installed: Mutex::new(false),
        hold_control: Mutex::new(None),
        preroll_enabled: Arc::new(AtomicBool::new(true)),
        dictation_locked: AtomicBool::new(false),
        preview_generation: AtomicU64::new(0),
    })
}

/// The macOS Services entry ("Rewrite with KEA" in the Services menu): a
/// selection arrives as text, and takes the same rewrite path a hotkey does.
#[cfg(target_os = "macos")]
fn spawn_macos_rewrite_service(
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    config_pool: SqlitePool,
) {
    use crate::commands::{default_rewrite_input, execute_rewrite};
    use crate::events::{emit_rewrite_error, emit_rewrite_progress};
    use kea_features::ProfileOverrides;

    let (svc_tx, mut svc_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    kea_platform::macos_services::register_rewrite_service(svc_tx);

    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(source_text) = svc_rx.recv().await {
            emit_rewrite_progress(&app, "Rewriting selection...");
            let mut input = default_rewrite_input(&config_pool).await;
            input.source_text = source_text;
            // No profile: the Services menu hands us the text directly, so
            // there is no guarantee the sending app is still frontmost by the
            // time this runs, and a wrong profile is worse than none.
            match execute_rewrite(&state, input, &ProfileOverrides::default()).await {
                Ok(_) => emit_rewrite_progress(&app, "Done"),
                Err(error) => emit_rewrite_error(&app, &error),
            }
        }
    });
}

/// Launch-time update check: non-blocking, offline-safe, silent when
/// up-to-date. Only active when the `updater` feature is enabled.
#[cfg(feature = "updater")]
fn spawn_launch_update_check(app: tauri::AppHandle, config_pool: SqlitePool) {
    use tauri_plugin_updater::UpdaterExt;
    tauri::async_runtime::spawn(async move {
        let auto_check: Option<String> = SettingsRepo::new(config_pool)
            .get("updates.auto_check")
            .await
            .ok()
            .flatten();
        let enabled = auto_check.as_deref() != Some("false");
        if !enabled {
            return;
        }
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(%e, "launch-time update check: updater init failed");
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                tracing::info!(
                    version = %update.version,
                    "update available"
                );
            }
            Ok(None) => {
                tracing::info!("app is up-to-date");
            }
            Err(e) => {
                tracing::warn!(%e, "launch-time update check failed (offline?)");
            }
        }
    });
}

/// Attach the tray menu.
fn install_tray(app: &mut tauri::App<Wry>) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "open", "Open KEA", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit KEA", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    // The tray icon is created from tauri.conf.json's `app.trayIcon` (iconPath),
    // which Tauri loads and decodes itself — so it always has a valid icon.
    // We attach the menu to that existing tray here. Do NOT build a tray from
    // `default_window_icon()`: it is `None` on macOS (macOS uses the .icns
    // bundle icon, not an embedded window icon), so `.unwrap()` there panics
    // during setup and aborts the whole app at launch.
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_menu(Some(menu))?;
        tray.on_menu_event(on_tray_menu_event);
    } else {
        // Fallback (no config tray): build one, setting the icon only when
        // one is actually available so we never unwrap `None`.
        let mut builder = TrayIconBuilder::with_id("main").menu(&menu);
        if let Some(icon) = app.default_window_icon().cloned() {
            builder = builder.icon(icon);
        }
        builder.on_menu_event(on_tray_menu_event).build(app)?;
    }
    Ok(())
}

/// Composition root: open the databases, build the engines and the shared
/// state, then start the things that run for the life of the app.
///
/// Manual test (macOS):
/// 1. Grant Accessibility + Microphone to KEA in System Settings > Privacy & Security.
/// 2. Configuration → set OpenAI provider + API key; bind dictation stt slot to `openai-stt`.
/// 3. Features → set push-to-talk hotkey (default Cmd+Shift+D).
/// 4. Place caret in TextEdit; press hotkey once to start listening (level meter events);
///    press again to stop, transcribe, and insert at cursor.
/// 5. Optional: build with `--features whisper`, download a GGUF model, bind `whisper` engine.
///
/// Manual test — meetings (mic-only, macOS):
/// 1. `cargo tauri dev`; grant Microphone when prompted.
/// 2. Configuration → OpenAI credentials; Features → bind `meetings` `stt` + `llm` slots.
/// 3. Meetings → Start → speak for ~30s → `meeting:segment` events with live transcript;
///    `meeting:level` RMS events while recording.
/// 4. Stop → title + notes populated in `data.db`; `meeting:state` idle.
/// 5. `capture_mode` = `mic_only` when loopback/SCK unavailable (default CI build).
///
/// Screen Recording grant + system audio are manual; not asserted in unit tests.
/// Headless CI does not assert real hotkey delivery, mic capture, or synthetic paste.
///
/// Manual test — TTS read-aloud (macOS):
/// 1. `cargo tauri dev`; grant Accessibility + Microphone.
/// 2. Configuration → OpenAI credentials; Features → bind `tts`/`tts` slot to `openai-tts`.
/// 3. Select text in TextEdit; hotkey (default Cmd+Shift+T) or invoke `run_read_aloud`.
/// 4. Hear playback via rodio; History shows `feature_id = tts` action row.
/// 5. `--features tts-local,sherpa` adds `sherpa-tts` after ONNX model download (manual).
///
/// Manual test — History + Logs:
/// 1. History page lists actions from `data.db`; conversations when rewrite stores content.
/// 2. Logs page tails `kea.log` via `tail_logs`; `open_log_folder` opens log dir in Finder.
///
/// Manual test — autostart + notifications:
/// 1. Settings → toggle autostart (`set_autostart` / `get_autostart`).
/// 2. `show_notification` displays a test OS notification (grant if prompted).
///
/// Manual test — first-run permissions:
/// 1. `get_all_permission_statuses` returns mic, screen recording, accessibility chips.
/// 2. Grant each in System Settings; re-check status (not asserted in unit tests).
fn setup(app: &mut tauri::App<Wry>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir().expect("app data dir");
    std::fs::create_dir_all(&dir).ok();
    let log_dir = app.path().app_log_dir().unwrap_or(dir.clone());
    std::fs::create_dir_all(&log_dir).ok();
    let guard = kea_core::log::init_logging(&log_dir, "info");
    app.manage(guard);

    let (config_pool, data_pool) = open_databases(app, &dir);

    let credential_store: Arc<dyn kea_core::secrets::CredentialStore> =
        Arc::new(KeyringCredentialStore::new("ai.kea.desktop"));
    let (engines, storages) = build_engines(&dir, &config_pool, &credential_store);

    spawn_startup_maintenance(&data_pool, &log_dir);

    let state = build_state(
        engines,
        storages,
        config_pool.clone(),
        data_pool,
        credential_store,
        log_dir,
    );

    // Hotkey dispatcher: register every shortcut, then spawn the listener that
    // turns a press into a feature handler (see [`crate::hotkeys`]).
    let app_handle = app.handle().clone();
    let action_rx = hotkeys::register_all(&state, &config_pool);

    #[cfg(target_os = "macos")]
    spawn_macos_rewrite_service(&state, &app_handle, config_pool.clone());

    hotkeys::spawn_dispatch_loop(state.clone(), app_handle.clone(), action_rx);

    // Deliberately after the dispatcher is up: the hold-to-talk listener drives
    // the same dictation handlers.
    hotkeys::spawn_saved_dictation_settings(&state, &app_handle, config_pool.clone());

    app.manage(state);

    #[cfg(feature = "updater")]
    spawn_launch_update_check(app_handle, config_pool);

    // Built up-front and left hidden: `emit_dictation_state` only shows and
    // hides it, so the webview is already loaded and listening when the first
    // state arrives. A failure here must not abort startup — dictation works
    // without the HUD.
    if let Err(e) = overlay::create(app.handle()) {
        tracing::warn!(error = %e, "failed to create the dictation overlay window");
    }

    install_tray(app)?;

    Ok(())
}
