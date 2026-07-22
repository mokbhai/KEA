#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kea_core::rewrite::{CredentialSourceAdapter, ProviderConfigRepo};
use kea_core::secrets::KeyringCredentialStore;
use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
use kea_core::store::meetings::MeetingRepo;
use kea_core::store::settings::SettingsRepo;
use kea_engines::{
    register_phase1_engines, register_phase2_stt_engines, register_phase4_tts_engines,
    ReqwestHttpClient, EngineRegistry,
};
use kea_features::{
    demo::DemoFeature,
    DictationFeature, FeatureRegistry, MeetingFeature, RewriteFeature, TtsFeature,
};
use kea_infer::ModelStorage;
use kea_platform::{new_audio_io, new_hotkeys, new_permissions};
use sqlx::SqlitePool;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, Wry};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::commands::{
    default_rewrite_input, dictation_hotkey_action, execute_rewrite,
    meeting_hotkey_action, register_dictation_hotkey, register_meeting_hotkey,
    register_rewrite_hotkey, register_tts_hotkey, resolve_dictation_accelerator,
    resolve_meeting_accelerator, resolve_rewrite_accelerator, resolve_tts_accelerator,
    start_dictation_inner, start_meeting_inner, stop_dictation_inner, stop_meeting_inner,
    trigger_tts_inner, ActiveMeetingSession, DictationHotkeyAction, MeetingHotkeyAction,
    DICTATION_ACTION_ID, MEETINGS_ACTION_ID, REWRITE_ACTION_ID,
    TTS_ACTION_ID, HotkeyRegStatus, record_hotkey_reg_status,
};
use crate::events::{emit_dictation_error, emit_meeting_error, emit_rewrite_error, emit_rewrite_progress, emit_tts_error};

pub struct AppState {
    pub engines: EngineRegistry,
    pub features: FeatureRegistry,
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
    /// Guards against concurrent downloads of the same model.
    pub active_downloads: Mutex<HashSet<String>>,
    /// Dictation in-flight run tracking: (generation counter, current run id).
    /// The counter guards against stale emits: a newer run's id is larger.
    pub dictation_run_counter: AtomicU64,
    pub dictation_current_run: Mutex<Option<u64>>,
    /// True while a meeting stop is in its post-capture processing window
    /// (STT + synthesis, after `active_meeting` was taken). Consulted so a
    /// hotkey press can't park on the audio lock and replay as a fresh start.
    pub meeting_processing: AtomicBool,
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
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
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
            commands::get_prompt_override,
            commands::set_prompt_override,
            commands::get_hotkey,
            commands::get_effective_hotkey,
            commands::get_hotkey_registration_status,
            commands::set_hotkey,
            commands::trigger_rewrite,
            commands::run_demo,
            commands::list_whisper_models,
            commands::list_installed_whisper_models,
            commands::download_whisper_model,
            commands::get_dictation_settings,
            commands::set_dictation_settings,
            commands::set_dictation_stt_binding,
            commands::start_dictation,
            commands::stop_dictation,
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
            commands::list_installed_onnx_models,
            commands::download_onnx_model,
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

fn setup(app: &mut tauri::App<Wry>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir().expect("app data dir");
    std::fs::create_dir_all(&dir).ok();
    let log_dir = app.path().app_log_dir().unwrap_or(dir.clone());
    std::fs::create_dir_all(&log_dir).ok();
    let guard = kea_core::log::init_logging(&log_dir, "info");
    app.manage(guard);

    let config_url = format!("sqlite://{}?mode=rwc", dir.join("config.db").display());
    let data_url = format!("sqlite://{}?mode=rwc", dir.join("data.db").display());

    let (config_pool, data_pool) = tauri::async_runtime::block_on(async {
        let c = match open_pool(&config_url).await {
            Ok(p) => p,
            Err(e) => {
                handle_migration_error(
                    &format!("failed to open config DB at {}: {e}", dir.join("config.db").display()),
                    app,
                );
            }
        };
        if let Err(e) = run_config_migrations(&c).await {
            handle_migration_error(
                &format!(
                    "config DB migration failed for {}: {e}",
                    dir.join("config.db").display()
                ),
                app,
            );
        }
        let d = match open_pool(&data_url).await {
            Ok(p) => p,
            Err(e) => {
                handle_migration_error(
                    &format!("failed to open data DB at {}: {e}", dir.join("data.db").display()),
                    app,
                );
            }
        };
        if let Err(e) = run_data_migrations(&d).await {
            handle_migration_error(
                &format!(
                    "data DB migration failed for {}: {e}",
                    dir.join("data.db").display()
                ),
                app,
            );
        }
        (c, d)
    });

    let credential_store: Arc<dyn kea_core::secrets::CredentialStore> =
        Arc::new(KeyringCredentialStore::new("ai.kea.desktop"));
    let creds = Arc::new(CredentialSourceAdapter::new(credential_store.clone()));
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
    register_phase2_stt_engines(&mut engines, http.clone(), creds.clone(), provider_configs.clone());
    register_phase4_tts_engines(
        &mut engines,
        http,
        creds.clone(),
        provider_configs,
    );

    let model_storage = ModelStorage::new(ModelStorage::default_whisper_root(&dir));
    model_storage.ensure_root().unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %model_storage.root.display(), "failed to create whisper model directory at startup");
    });
    let parakeet_storage = ModelStorage::new(ModelStorage::default_parakeet_root(&dir));
    parakeet_storage.ensure_root().unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %parakeet_storage.root.display(), "failed to create parakeet model directory at startup");
    });
    let tts_storage = ModelStorage::new(ModelStorage::default_tts_root(&dir));
    tts_storage.ensure_root().unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %tts_storage.root.display(), "failed to create tts model directory at startup");
    });

    #[cfg(feature = "whisper")]
    {
        use kea_engines::register_whisper_stt_engine;
        use kea_infer::WhisperRsInference;
        let whisper_storage = Arc::new(ModelStorage::new(model_storage.root.clone()));
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
        let storage = Arc::new(ModelStorage::new(parakeet_storage.root.clone()));
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
        let storage = Arc::new(ModelStorage::new(tts_storage.root.clone()));
        register_sherpa_tts_engine(
            &mut engines,
            Arc::new(SherpaOnnxTtsInference::new()),
            storage,
        );
    }

    let mut features = FeatureRegistry::default();
    features.register(Arc::new(DemoFeature));
    features.register(Arc::new(RewriteFeature));
    features.register(Arc::new(DictationFeature));
    features.register(Arc::new(MeetingFeature));
    features.register(Arc::new(TtsFeature));

    let hotkeys = Mutex::new(new_hotkeys());
    let audio = AsyncMutex::new(new_audio_io());
    let permissions = new_permissions();
    let meeting_repo = MeetingRepo::new(data_pool.clone());

    // Startup cleanup: prune history tables and old log files older than 90 days.
    {
        let data_pool = data_pool.clone();
        let log_dir = log_dir.clone();
        tauri::async_runtime::spawn(async move {
            let actions = kea_core::store::actions::ActionRepo::new(data_pool.clone());
            match actions.prune_older_than_days(90).await {
                Ok(n) if n > 0 => tracing::info!(pruned = n, "pruned old action rows"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "failed to prune action history"),
            }
            let conversations = kea_core::store::conversations::ConversationRepo::new(data_pool.clone());
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

    let state = Arc::new(AppState {
        engines,
        features,
        config_pool: config_pool.clone(),
        data_pool,
        meeting_repo,
        credentials: credential_store,
        permissions,
        hotkeys,
        model_storage,
        parakeet_storage,
        tts_storage,
        log_dir: log_dir.clone(),
        audio,
        active_meeting: Mutex::new(None),
        level_poll_cancel: Mutex::new(None),
        segment_poll_cancel: Mutex::new(None),
        hotkey_reg_status: Mutex::new(HashMap::new()),
        active_downloads: Mutex::new(HashSet::new()),
        dictation_run_counter: AtomicU64::new(0),
        dictation_current_run: Mutex::new(None),
        meeting_processing: AtomicBool::new(false),
    });

    // Hotkey dispatcher: register rewrite + dictation shortcuts and spawn listener.
    //
    // Manual test (macOS):
    // 1. Grant Accessibility + Microphone to KEA in System Settings > Privacy & Security.
    // 2. Configuration → set OpenAI provider + API key; bind dictation stt slot to `openai-stt`.
    // 3. Features → set push-to-talk hotkey (default Cmd+Shift+D).
    // 4. Place caret in TextEdit; press hotkey once to start listening (level meter events);
    //    press again to stop, transcribe, and insert at cursor.
    // 5. Optional: build with `--features whisper`, download a GGUF model, bind `whisper` engine.
    //
    // Manual test — meetings (mic-only, macOS):
    // 1. `cargo tauri dev`; grant Microphone when prompted.
    // 2. Configuration → OpenAI credentials; Features → bind `meetings` `stt` + `llm` slots.
    // 3. Meetings → Start → speak for ~30s → `meeting:segment` events with live transcript;
    //    `meeting:level` RMS events while recording.
    // 4. Stop → title + notes populated in `data.db`; `meeting:state` idle.
    // 5. `capture_mode` = `mic_only` when loopback/SCK unavailable (default CI build).
    // Screen Recording grant + system audio are manual; not asserted in unit tests.
    // Headless CI does not assert real hotkey delivery, mic capture, or synthetic paste.
    // Manual test — TTS read-aloud (macOS):
    // 1. `cargo tauri dev`; grant Accessibility + Microphone.
    // 2. Configuration → OpenAI credentials; Features → bind `tts`/`tts` slot to `openai-tts`.
    // 3. Select text in TextEdit; hotkey (default Cmd+Shift+T) or invoke `run_read_aloud`.
    // 4. Hear playback via rodio; History shows `feature_id = tts` action row.
    // 5. `--features tts-local,sherpa` adds `sherpa-tts` after ONNX model download (manual).
    //
    // Manual test — History + Logs:
    // 1. History page lists actions from `data.db`; conversations when rewrite stores content.
    // 2. Logs page tails `kea.log` via `tail_logs`; `open_log_folder` opens log dir in Finder.
    //
    // Manual test — autostart + notifications:
    // 1. Settings → toggle autostart (`set_autostart` / `get_autostart`).
    // 2. `show_notification` displays a test OS notification (grant if prompted).
    //
    // Manual test — first-run permissions:
    // 1. `get_all_permission_statuses` returns mic, screen recording, accessibility chips.
    // 2. Grant each in System Settings; re-check status (not asserted in unit tests).
    let app_handle = app.handle().clone();
    let state_for_hotkeys = state.clone();
    let action_rx = {
        let rewrite_accel =
            tauri::async_runtime::block_on(resolve_rewrite_accelerator(&config_pool));
        let dictation_accel =
            tauri::async_runtime::block_on(resolve_dictation_accelerator(&config_pool));
        let tts_accel = tauri::async_runtime::block_on(resolve_tts_accelerator(&config_pool));
        let meeting_accel =
            tauri::async_runtime::block_on(resolve_meeting_accelerator(&config_pool));
        let mut hk = state_for_hotkeys.hotkeys.lock().expect("hotkeys lock");
        {
            let mut statuses = state_for_hotkeys
                .hotkey_reg_status
                .lock()
                .expect("hotkey_reg_status lock");
            record_hotkey_reg_status(
                &mut statuses,
                crate::commands::REWRITE_FEATURE_ID,
                crate::commands::REWRITE_COMMAND_ID,
                register_rewrite_hotkey(&mut hk, &rewrite_accel),
            );
            record_hotkey_reg_status(
                &mut statuses,
                crate::commands::DICTATION_FEATURE_ID,
                crate::commands::DICTATION_COMMAND_ID,
                register_dictation_hotkey(&mut hk, &dictation_accel),
            );
            record_hotkey_reg_status(
                &mut statuses,
                crate::commands::TTS_FEATURE_ID,
                crate::commands::TTS_COMMAND_ID,
                register_tts_hotkey(&mut hk, &tts_accel),
            );
            record_hotkey_reg_status(
                &mut statuses,
                crate::commands::MEETINGS_FEATURE_ID,
                crate::commands::MEETINGS_COMMAND_ID,
                register_meeting_hotkey(&mut hk, &meeting_accel),
            );
        }
        hk.on_action()
    };

    let state_for_task = state.clone();

    #[cfg(target_os = "macos")]
    {
        let (svc_tx, mut svc_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        kea_platform::macos_services::register_rewrite_service(svc_tx);

        let state_for_svc = state.clone();
        let app_handle_for_svc = app_handle.clone();
        let config_pool_for_svc = config_pool.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(source_text) = svc_rx.recv().await {
                emit_rewrite_progress(&app_handle_for_svc, "Rewriting selection...");
                let mut input = default_rewrite_input(&config_pool_for_svc).await;
                input.source_text = source_text;
                match execute_rewrite(&state_for_svc, input).await {
                    Ok(_) => emit_rewrite_progress(&app_handle_for_svc, "Done"),
                    Err(error) => emit_rewrite_error(&app_handle_for_svc, &error),
                }
            }
        });
    }

    tauri::async_runtime::spawn(async move {
        let mut rx = action_rx;

        // Per-feature busy flags: each flag serialises its own feature's
        // handlers so presses queued during a long handler are dropped
        // (rather than replaying as fresh starts).
        let rewrite_busy  = Arc::new(AtomicBool::new(false));
        let dictation_busy = Arc::new(AtomicBool::new(false));
        let tts_busy      = Arc::new(AtomicBool::new(false));
        let meetings_busy = Arc::new(AtomicBool::new(false));

        while let Some(action) = rx.recv().await {
            if action == REWRITE_ACTION_ID {
                match crate::commands::try_acquire_busy(&rewrite_busy) {
                    None => {
                        tracing::debug!("hotkey press ignored: rewrite handler in flight");
                        continue;
                    }
                    Some(_guard) => {
                        let state = state_for_task.clone();
                        let app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let _busy = _guard;
                            emit_rewrite_progress(&app, "Capturing selection...");
                            let input = default_rewrite_input(&state.config_pool).await;
                            match execute_rewrite(&state, input).await {
                                Ok(_) => emit_rewrite_progress(&app, "Done"),
                                Err(error) => emit_rewrite_error(&app, &error),
                            }
                        });
                    }
                }
                continue;
            }

            if action == DICTATION_ACTION_ID {
                match crate::commands::try_acquire_busy(&dictation_busy) {
                    None => {
                        tracing::debug!("hotkey press ignored: dictation handler in flight");
                        continue;
                    }
                    Some(_guard) => {
                        let state = state_for_task.clone();
                        let app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let _busy = _guard;

                            // Check meeting state BEFORE taking the audio lock:
                            // during meeting synthesis the lock is free but a
                            // start would be wrong, and reading the flag first
                            // avoids any park-and-replay.
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

                            match dictation_hotkey_action(current, meeting_active, in_flight) {
                                DictationHotkeyAction::Start => {
                                    if let Err(error) =
                                        start_dictation_inner(&state, &app).await
                                    {
                                        emit_dictation_error(&app, &error);
                                    }
                                }
                                DictationHotkeyAction::Stop => {
                                    if let Err(error) =
                                        stop_dictation_inner(&state, &app).await
                                    {
                                        emit_dictation_error(&app, &error);
                                    }
                                }
                                DictationHotkeyAction::Ignore => {}
                            }
                        });
                    }
                }
                continue;
            }

            if action == TTS_ACTION_ID {
                match crate::commands::try_acquire_busy(&tts_busy) {
                    None => {
                        tracing::debug!("hotkey press ignored: tts handler in flight");
                        continue;
                    }
                    Some(_guard) => {
                        let state = state_for_task.clone();
                        let app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let _busy = _guard;
                            if let Err(error) = trigger_tts_inner(&state, &app).await {
                                emit_tts_error(&app, &error);
                            }
                        });
                    }
                }
                continue;
            }

            if action == MEETINGS_ACTION_ID {
                match crate::commands::try_acquire_busy(&meetings_busy) {
                    None => {
                        tracing::debug!("hotkey press ignored: meetings handler in flight");
                        continue;
                    }
                    Some(_guard) => {
                        let state = state_for_task.clone();
                        let app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let _busy = _guard;

                            let recording = {
                                state
                                    .active_meeting
                                    .lock()
                                    .map(|guard| guard.is_some())
                                    .unwrap_or(false)
                            };
                            let processing =
                                state.meeting_processing.load(Ordering::SeqCst);
                            match meeting_hotkey_action(recording, processing) {
                                MeetingHotkeyAction::Ignore => {
                                    tracing::debug!(
                                        "meeting hotkey ignored: a meeting is finishing"
                                    );
                                }
                                MeetingHotkeyAction::Stop => {
                                    if let Err(error) =
                                        stop_meeting_inner(&state, &app).await
                                    {
                                        emit_meeting_error(&app, &error);
                                    }
                                }
                                MeetingHotkeyAction::Start => {
                                    if let Err(error) =
                                        start_meeting_inner(&state, &app).await
                                    {
                                        emit_meeting_error(&app, &error);
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
    });

    app.manage(state);

    // Launch-time update check: non-blocking, offline-safe, silent when
    // up-to-date. Only active when the `updater` feature is enabled.
    #[cfg(feature = "updater")]
    {
        use tauri_plugin_updater::UpdaterExt;
        let app_handle = app.handle().clone();
        let config_pool = config_pool.clone();
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
            let updater = match app_handle.updater() {
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
