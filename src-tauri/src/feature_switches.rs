//! Master switches gate commands, never engine slots shared by other features.
use crate::{commands, AppState};
use kea_core::store::{hotkeys::HotkeyBindingRepo, settings::SettingsRepo};
use sqlx::SqlitePool;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

pub const FEATURES: [&str; 6] = [
    "rewrite",
    "dictation",
    "meetings",
    "transcribe",
    "tts",
    "assistant",
];
pub(crate) static UPDATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn enabled(pool: &SqlitePool, feature: &str) -> bool {
    commands::read_bool_setting(pool, &format!("features.{feature}.enabled"), true).await
}
pub async fn require(pool: &SqlitePool, feature: &str) -> Result<(), String> {
    if enabled(pool, feature).await {
        Ok(())
    } else {
        Err(format!("{feature} feature disabled"))
    }
}
#[tauri::command]
pub async fn get_feature_enabled(
    state: State<'_, Arc<AppState>>,
    feature: String,
) -> Result<bool, String> {
    validate(&feature)?;
    Ok(enabled(&state.config_pool, &feature).await)
}
fn validate(feature: &str) -> Result<(), String> {
    if FEATURES.contains(&feature) {
        Ok(())
    } else {
        Err(format!("unknown feature: {feature}"))
    }
}
#[tauri::command]
pub async fn set_feature_enabled(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    feature: String,
    enabled: bool,
) -> Result<(), String> {
    validate(&feature)?;
    let _update = UPDATE.lock().await;
    let bindings = HotkeyBindingRepo::new(state.config_pool.clone())
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let mut actions = Vec::new();
    for action in commands::hotkey_owners(&bindings)
        .into_iter()
        .filter(|a| a.feature == feature)
    {
        let accel = commands::resolve_accelerator(&state.config_pool, &action).await;
        if !accel.is_empty() {
            actions.push((action, accel));
        }
    }
    SettingsRepo::new(state.config_pool.clone())
        .set(&format!("features.{feature}.enabled"), &enabled)
        .await
        .map_err(|e| e.to_string())?;
    let mut failure = None;
    {
        let mut hotkeys = state.hotkeys.lock().map_err(|e| e.to_string())?;
        let mut statuses = state.hotkey_reg_status.lock().map_err(|e| e.to_string())?;
        for (action, accelerator) in actions {
            let result = if enabled {
                commands::register_hotkey(&mut hotkeys, &action, &accelerator)
            } else {
                hotkeys
                    .unregister(&kea_platform::HotkeyBinding { accelerator })
                    .or_else(|e| {
                        if matches!(e, kea_platform::HotkeyError::NotRegistered(_)) {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| e.to_string())
            };
            if let Err(e) = &result {
                failure = Some(e.clone());
            }
            commands::record_hotkey_reg_status(
                &mut statuses,
                action.feature,
                &action.command,
                result,
            );
        }
    }
    if !enabled && feature == "meetings" {
        let recording = state
            .active_meeting
            .lock()
            .map_err(|e| e.to_string())?
            .is_some();
        if recording {
            if let Err(error) = commands::stop_meeting_inner(&state, &app).await {
                failure = Some(error);
            }
        }
    }
    if !enabled && feature == "transcribe" {
        if let Some(cancel) = state
            .file_transcribe_cancel
            .lock()
            .map_err(|e| e.to_string())?
            .as_ref()
        {
            let _ = cancel.send(true);
        }
    }
    if !enabled && feature == "rewrite" {
        commands::close_palette_for(&state, &app, crate::palette::PaletteEvent::Escape).await;
    }
    if !enabled && feature == "assistant" {
        state
            .assistant_cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    if feature == "dictation" {
        let settings = kea_core::dictation::DictationSettingsRepo::new(SettingsRepo::new(
            state.config_pool.clone(),
        ))
        .get()
        .await
        .map_err(|e| e.to_string())?;
        if !enabled {
            let _ = commands::cancel_dictation_inner(&state, &app).await;
            commands::stop_input_preview_inner(&state, &app).await;
        }
        commands::apply_dictation_settings(&state, &app, &settings).await;
    }
    app.emit("feature-switch-changed", &feature)
        .map_err(|e| e.to_string())?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn defaults_and_independent_gates() {
        let pool = kea_core::store::db::open_pool("sqlite::memory:")
            .await
            .unwrap();
        kea_core::store::db::run_config_migrations(&pool)
            .await
            .unwrap();
        for feature in FEATURES {
            assert!(enabled(&pool, feature).await);
        }
        SettingsRepo::new(pool.clone())
            .set("features.rewrite.enabled", &false)
            .await
            .unwrap();
        assert_eq!(
            require(&pool, "rewrite").await.unwrap_err(),
            "rewrite feature disabled"
        );
        assert!(require(&pool, "dictation").await.is_ok());
    }
}
