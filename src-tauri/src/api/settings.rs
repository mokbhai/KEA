//! The commands the General settings page drives the local API with.
//!
//! Kept out of `commands.rs` (7 900 lines) and next to the server they
//! configure: turning the API on is a lifecycle operation, not a settings
//! write, because the socket has to be bound before the toggle can honestly
//! say "on".

use std::path::PathBuf;
use std::sync::Arc;

use kea_core::store::settings::SettingsRepo;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use super::{ratelimit, server, token, ENABLED_SETTING, RATE_LIMIT_SETTING};
use crate::AppState;

/// What the settings page shows. `running` is asked of the handle rather than
/// inferred from `enabled`: a bind that failed must not read as on.
#[derive(Debug, Clone, Serialize)]
pub struct ApiSettings {
    pub enabled: bool,
    pub running: bool,
    /// Empty on a platform with no unix sockets, where `supported` is false.
    pub socket_path: String,
    pub token_file: String,
    pub max_rewrites_per_minute: u32,
    pub supported: bool,
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("no app data directory: {e}"))
}

/// The configured rate limit.
///
/// Through `get_optional` because a cleared value writes the JSON literal
/// `null` rather than removing the row, and a plain `get::<u32>` would fail to
/// deserialize that and fall back with a warning on every read.
async fn rate_limit(state: &Arc<AppState>) -> u32 {
    SettingsRepo::new(state.config_pool.clone())
        .get_optional::<u32>(RATE_LIMIT_SETTING)
        .await
        .ok()
        .flatten()
        .unwrap_or(ratelimit::DEFAULT_REWRITES_PER_MINUTE)
}

fn is_running(state: &Arc<AppState>) -> bool {
    state
        .api_server
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Bind the socket, mint the token if there is none, and write the token file.
///
/// Public because `setup` calls it too: one start path, whether the API comes
/// up at launch or from the toggle.
pub async fn start_server(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    if is_running(state) {
        return Ok(());
    }
    let dir = app_data_dir(app)?;
    let secret = token::load_or_mint(state.credentials.as_ref()).await?;
    let handle = server::start(
        state.clone(),
        app.clone(),
        &dir,
        secret.clone(),
        rate_limit(state).await,
    )
    .await?;
    // Only after the socket is up: a token file for an API that is not
    // listening is a secret on disk buying nothing.
    token::write_token_file(&dir, &secret)?;
    match state.api_server.lock() {
        Ok(mut slot) => *slot = Some(handle),
        Err(poisoned) => *poisoned.into_inner() = Some(handle),
    }
    Ok(())
}

/// Stop the socket and delete the token file.
///
/// The file goes with the server, always: a `0600` token left behind for a
/// disabled API is the one piece of this design with no upside.
pub fn stop_server(state: &Arc<AppState>, app: &AppHandle) {
    let handle = match state.api_server.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(handle) = handle {
        handle.stop();
    }
    if let Ok(dir) = app_data_dir(app) {
        token::remove_token_file(&dir);
    }
}

#[tauri::command]
pub async fn get_api_settings(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<ApiSettings, String> {
    let state = state.inner().clone();
    let dir = app_data_dir(&app)?;
    Ok(ApiSettings {
        enabled: crate::commands::read_bool_setting(&state.config_pool, ENABLED_SETTING, false)
            .await,
        running: is_running(&state),
        socket_path: token::socket_path(&dir).to_string_lossy().into_owned(),
        token_file: token::token_file_path(&dir).to_string_lossy().into_owned(),
        max_rewrites_per_minute: rate_limit(&state).await,
        supported: cfg!(unix),
    })
}

/// Turn the API on or off.
///
/// The setting is only written once the socket is actually up, so a failed
/// bind leaves the toggle off rather than claiming an API that is not there.
#[tauri::command]
pub async fn set_api_enabled(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    enabled: bool,
) -> Result<ApiSettings, String> {
    let owned = state.inner().clone();
    if enabled {
        start_server(&owned, &app).await?;
    } else {
        stop_server(&owned, &app);
    }
    SettingsRepo::new(owned.config_pool.clone())
        .set(ENABLED_SETTING, &enabled)
        .await
        .map_err(|e| e.to_string())?;
    get_api_settings(state, app).await
}

#[tauri::command]
pub async fn set_api_rate_limit(state: State<'_, Arc<AppState>>, limit: u32) -> Result<(), String> {
    // Written as a JSON number, and read back as one: the generic
    // `set_setting` command would store the string "20" here, which
    // `get_optional::<u32>` cannot read.
    SettingsRepo::new(state.config_pool.clone())
        .set(RATE_LIMIT_SETTING, &limit)
        .await
        .map_err(|e| e.to_string())?;
    // The running bucket keeps the limit it started with; saying so beats
    // silently applying a number the user thinks is live.
    tracing::info!(
        limit,
        "local API rate limit changed; restart the API to apply it"
    );
    Ok(())
}

/// The token, for the reveal-and-copy row.
#[tauri::command]
pub async fn reveal_api_token(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    token::load_or_mint(state.credentials.as_ref()).await
}

/// Mint a new token, invalidating every script holding the old one.
#[tauri::command]
pub async fn regenerate_api_token(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<String, String> {
    let state = state.inner().clone();
    let secret = token::regenerate(state.credentials.as_ref()).await?;
    tracing::info!("local API token regenerated; existing scripts must be updated");
    // A restart is what actually makes the server compare against the new
    // token — it captured the old one when it bound — and it is also what
    // rewrites the shim's token file, so the file and the keychain cannot be
    // left disagreeing.
    if is_running(&state) {
        stop_server(&state, &app);
        start_server(&state, &app).await?;
    }
    Ok(secret)
}

/// Where the shim lives inside the bundle.
///
/// `bundle.resources` in `tauri.conf.json` copies `resources/kea-cli` into
/// `KEA.app/Contents/Resources/resources/kea-cli`, so it is sealed by the same
/// (ad-hoc, in `make install`) signature as the app itself.
const SHIM_RESOURCE: &str = "resources/kea-cli";

/// Where the symlink goes. Not written by `make install`: that target needs no
/// password today, and writing to `/usr/local/bin` would give it one.
const SHIM_INSTALL_DIR: &str = "/usr/local/bin";

/// Symlink the bundled shim onto the PATH.
#[tauri::command]
pub fn install_cli_shim(app: AppHandle) -> Result<String, String> {
    let source = app
        .path()
        .resolve(SHIM_RESOURCE, tauri::path::BaseDirectory::Resource)
        .map_err(|e| format!("could not find the bundled CLI tool: {e}"))?;
    if !source.exists() {
        return Err(format!(
            "the CLI tool is missing from this build (expected {})",
            source.display()
        ));
    }
    install_shim_at(&source, std::path::Path::new(SHIM_INSTALL_DIR))
}

#[cfg(unix)]
fn install_shim_at(source: &std::path::Path, dir: &std::path::Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;

    let target = dir.join("kea");
    if !dir.is_dir() {
        return Err(manual_instructions(source, &target, "does not exist"));
    }
    // The bundler copies resources, and whether it carries the execute bit
    // across is not something to bet the feature on: a symlink to a
    // non-executable file fails with a permission error that says nothing
    // about why. Restoring it is cheap and idempotent.
    if let Ok(meta) = std::fs::metadata(source) {
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            let _ = std::fs::set_permissions(
                source,
                std::fs::Permissions::from_mode((mode & 0o777) | 0o755),
            );
        }
    }
    // Replace rather than fail: the app moves on every update, so a symlink
    // from a previous install points at a bundle that may be gone.
    match std::fs::remove_file(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(manual_instructions(source, &target, &e.to_string())),
    }
    std::os::unix::fs::symlink(source, &target)
        .map_err(|e| manual_instructions(source, &target, &e.to_string()))?;
    Ok(target.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn install_shim_at(_source: &std::path::Path, _dir: &std::path::Path) -> Result<String, String> {
    Err("the KEA command-line tool is a shell script, so it is macOS/Linux only".into())
}

/// The command to run by hand when the symlink needs a password.
///
/// `/usr/local/bin` is root-owned on a fresh macOS install, so this is the
/// common outcome, not the exceptional one — which is why the error carries a
/// command the user can paste rather than just a reason.
fn manual_instructions(source: &std::path::Path, target: &std::path::Path, why: &str) -> String {
    format!(
        "could not link {} ({why}). Run this in Terminal instead:\n  sudo ln -sf \"{}\" \"{}\"",
        target.display(),
        source.display(),
        target.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn installing_the_shim_links_it_and_replaces_a_stale_link() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("kea-cli");
        std::fs::write(&source, "#!/bin/sh\n").unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin).unwrap();

        // Written without the execute bit, as a stripped resource copy would
        // be, so the repair path is what the test exercises.
        let linked = install_shim_at(&source, &bin).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&source).unwrap().permissions().mode() & 0o111,
                0,
                "the shim was left non-executable"
            );
        }
        assert_eq!(linked, bin.join("kea").to_string_lossy());
        assert_eq!(std::fs::read_link(bin.join("kea")).unwrap(), source);

        // A second install over the existing link succeeds rather than
        // failing with EEXIST — the bundle path changes on every update.
        assert!(install_shim_at(&source, &bin).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_bin_directory_explains_the_manual_command() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("kea-cli");
        std::fs::write(&source, "#!/bin/sh\n").unwrap();
        let err = install_shim_at(&source, &home.path().join("nope")).unwrap_err();
        assert!(err.contains("sudo ln -sf"), "{err}");
    }
}
