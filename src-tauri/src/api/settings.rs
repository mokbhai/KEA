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
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager, State};

use super::server::LoopbackPort;
use super::{
    ratelimit, server, token, DEFAULT_TCP_PORT, ENABLED_SETTING, RATE_LIMIT_SETTING,
    TCP_ENABLED_SETTING, TCP_PORT_SETTING,
};
use crate::commands::read_bool_setting;
use crate::AppState;

/// What the settings page shows. `running` and `tcp_running` are asked of the
/// handles rather than inferred from the settings: a bind that failed must not
/// read as on.
#[derive(Debug, Clone, Serialize)]
pub struct ApiSettings {
    pub enabled: bool,
    pub running: bool,
    /// Empty where there is no unix socket.
    pub socket_path: String,
    pub token_file: String,
    pub max_rewrites_per_minute: u32,
    /// Whether the API can run on this platform at all. True everywhere now
    /// that the loopback port exists; on a platform without unix sockets the
    /// port is the only transport. Use `unix_socket_supported` to decide what
    /// to show.
    pub supported: bool,
    /// Whether this OS has unix sockets, the default transport. False on
    /// Windows, where the port is the only way in.
    pub unix_socket_supported: bool,
    /// Whether the loopback port is switched on. Only meaningful on unix: the
    /// non-unix build always runs the port.
    pub tcp_enabled: bool,
    /// Whether the port is listening right now.
    pub tcp_running: bool,
    /// The port the listener uses, or would use the next time it starts.
    pub tcp_port: u16,
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

/// The running server, if there is one. Cloned out of the slot so the caller
/// can await on it without holding the std lock.
fn running_server(state: &Arc<AppState>) -> Option<Arc<server::ServerHandle>> {
    match state.api_server.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn is_running(state: &Arc<AppState>) -> bool {
    running_server(state).is_some()
}

/// The loopback port, read through the same tolerant path as the rate limit.
/// Zero is refused at the setter, so a `0` here can only be a hand-edited row,
/// and it falls back rather than binding a random port the page cannot show.
async fn tcp_port(pool: &SqlitePool) -> u16 {
    SettingsRepo::new(pool.clone())
        .get_optional::<u16>(TCP_PORT_SETTING)
        .await
        .ok()
        .flatten()
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_TCP_PORT)
}

async fn tcp_enabled(pool: &SqlitePool) -> bool {
    read_bool_setting(pool, TCP_ENABLED_SETTING, false).await
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
    // Without unix sockets the port is the only way in, so it runs whatever
    // the setting says. Elsewhere it runs only when the setting is on.
    let tcp_on = !cfg!(unix) || tcp_enabled(&state.config_pool).await;
    let tcp = tcp_on.then_some(LoopbackPort::new(tcp_port(&state.config_pool).await));
    let handle = server::start(
        state.clone(),
        app.clone(),
        &dir,
        secret.clone(),
        rate_limit(state).await,
        tcp,
    )
    .await?;
    // Only after the transports are up: a token file for an API that is not
    // listening is a secret on disk buying nothing.
    if let Err(e) = token::write_token_file(&dir, &secret) {
        handle.stop().await;
        return Err(e);
    }
    match state.api_server.lock() {
        Ok(mut slot) => *slot = Some(Arc::new(handle)),
        Err(poisoned) => *poisoned.into_inner() = Some(Arc::new(handle)),
    }
    Ok(())
}

/// Stop the socket and the port, and delete the token file.
///
/// The file goes with the server, always: a `0600` token left behind for a
/// disabled API is the one piece of this design with no upside.
pub async fn stop_server(state: &Arc<AppState>, app: &AppHandle) {
    let handle = match state.api_server.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(handle) = handle {
        handle.stop().await;
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
    let server = running_server(&state);
    let tcp_running = match &server {
        Some(server) => server.tcp_addr().await.is_some(),
        None => false,
    };
    Ok(ApiSettings {
        enabled: crate::commands::read_bool_setting(&state.config_pool, ENABLED_SETTING, false)
            .await,
        running: server.is_some(),
        socket_path: if cfg!(unix) {
            token::socket_path(&dir).to_string_lossy().into_owned()
        } else {
            String::new()
        },
        token_file: token::token_file_path(&dir).to_string_lossy().into_owned(),
        max_rewrites_per_minute: rate_limit(&state).await,
        supported: true,
        unix_socket_supported: cfg!(unix),
        tcp_enabled: tcp_enabled(&state.config_pool).await,
        tcp_running,
        tcp_port: tcp_port(&state.config_pool).await,
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
        stop_server(&owned, &app).await;
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

/// Switch the loopback port on or off, live.
///
/// While the API runs, this starts or stops only the port; the socket keeps
/// serving. The setting is written only once the port is in the state it
/// names, so a failed bind leaves the toggle off rather than claiming a port
/// that is not open. Off while the API is stopped just records the choice for
/// the next start.
#[tauri::command]
pub async fn set_api_tcp_enabled(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    enabled: bool,
) -> Result<ApiSettings, String> {
    let owned = state.inner().clone();
    if cfg!(not(unix)) && !enabled {
        return Err(
            "this platform has no unix socket, so the localhost port cannot be turned off while the API runs; turn the API off instead"
                .into(),
        );
    }
    if let Some(server) = running_server(&owned) {
        let port = if enabled {
            Some(LoopbackPort::new(tcp_port(&owned.config_pool).await))
        } else {
            None
        };
        server.set_tcp(port).await?;
    }
    SettingsRepo::new(owned.config_pool.clone())
        .set(TCP_ENABLED_SETTING, &enabled)
        .await
        .map_err(|e| e.to_string())?;
    get_api_settings(state, app).await
}

/// Change the loopback port. If the port is listening, it moves now.
///
/// A port that cannot be bound is an error, and the socket keeps serving. The
/// port is then not listening, so the enabled setting is cleared to match:
/// the toggle must not claim a port that is not open. The new number is stored
/// only once it has bound.
#[tauri::command]
pub async fn set_api_tcp_port(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    port: u16,
) -> Result<ApiSettings, String> {
    let owned = state.inner().clone();
    if port == 0 {
        return Err("the port must be between 1 and 65535".into());
    }
    if let Some(server) = running_server(&owned) {
        if tcp_enabled(&owned.config_pool).await {
            if let Err(bind_error) = server.set_tcp(Some(LoopbackPort::new(port))).await {
                // Not on Windows: there the port is the only transport, and
                // clearing the setting would only hide that it is down.
                if cfg!(unix) {
                    SettingsRepo::new(owned.config_pool.clone())
                        .set(TCP_ENABLED_SETTING, &false)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                return Err(bind_error);
            }
        }
    }
    SettingsRepo::new(owned.config_pool.clone())
        .set(TCP_PORT_SETTING, &port)
        .await
        .map_err(|e| e.to_string())?;
    get_api_settings(state, app).await
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
        stop_server(&state, &app).await;
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

    /// Until someone writes them, the port is off and listens on 8765, and a
    /// stored `0` (which the setter refuses) falls back rather than binding a
    /// random port the page could not show.
    #[tokio::test]
    async fn the_tcp_port_is_off_on_8765_until_someone_sets_it() {
        use kea_core::store::db::{open_pool, run_config_migrations};
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();

        assert!(!tcp_enabled(&pool).await);
        assert_eq!(tcp_port(&pool).await, 8765);

        SettingsRepo::new(pool.clone())
            .set(TCP_PORT_SETTING, &0u16)
            .await
            .unwrap();
        assert_eq!(tcp_port(&pool).await, DEFAULT_TCP_PORT);

        SettingsRepo::new(pool.clone())
            .set(TCP_PORT_SETTING, &9000u16)
            .await
            .unwrap();
        assert_eq!(tcp_port(&pool).await, 9000);
    }

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
