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
    /// Whether the loopback port is switched on. Off by default on every OS;
    /// on one without unix sockets the API cannot start until it is on.
    pub tcp_enabled: bool,
    /// Whether the port is listening right now.
    pub tcp_running: bool,
    /// Why the port is switched on but not listening — typically another
    /// process holding it. `None` when it is listening or switched off.
    pub tcp_error: Option<String>,
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
    // The port is opt-in everywhere, Windows included: a browser-reachable
    // listener must never start because of the platform. Where it is the only
    // transport, `server::start` refuses to start without it and says so.
    let tcp_on = tcp_enabled(&state.config_pool).await;
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
    let (tcp_running, tcp_error) = match &server {
        Some(server) => (server.tcp_addr().await.is_some(), server.tcp_error()),
        None => (false, None),
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
        tcp_error,
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
pub async fn install_cli_shim(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || install_cli_shim_blocking(app))
        .await
        .map_err(|e| e.to_string())?
}

fn install_cli_shim_blocking(app: AppHandle) -> Result<String, String> {
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
    let dir = std::path::Path::new(SHIM_INSTALL_DIR);
    match install_shim_at(&source, dir) {
        Ok(path) => Ok(path),
        Err(_error) => {
            #[cfg(target_os = "macos")]
            {
                // `make install` remains password-free. This explicit user action
                // can request authorization through the system-owned prompt.
                let target = dir.join("kea");
                refuse_regular_file(&target)?;
                let script = admin_link_script(&source, dir)?;
                let output = std::process::Command::new("osascript")
                    .arg("-e")
                    .arg(script)
                    .output()
                    .map_err(|e| format!("could not request administrator authorization: {e}"))?;
                if output.status.success() {
                    return Ok(target.to_string_lossy().into_owned());
                }
                let reason = String::from_utf8_lossy(&output.stderr);
                if !reason.contains("(-128)") {
                    return Err(format!("could not install CLI tool: {reason}"));
                }
                let local = app
                    .path()
                    .home_dir()
                    .map_err(|e| e.to_string())?
                    .join(".local/bin");
                std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
                let linked = install_shim_at(&source, &local)?;
                Ok(format!(
                    "{linked}. Add {} to PATH in your shell configuration.",
                    local.display()
                ))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(_error)
            }
        }
    }
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
    if std::fs::read_link(&target).is_ok_and(|path| path == source) {
        return Ok(target.to_string_lossy().into_owned());
    }
    refuse_regular_file(&target)?;
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

fn refuse_regular_file(target: &std::path::Path) -> Result<(), String> {
    if std::fs::symlink_metadata(target).is_ok_and(|m| !m.file_type().is_symlink()) {
        return Err(format!(
            "{} already exists and is not a symlink; move it before installing KEA",
            target.display()
        ));
    }
    Ok(())
}

fn shell_quote(path: &std::path::Path) -> Result<String, String> {
    let path = path.to_str().ok_or("CLI path is not valid UTF-8")?;
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

fn admin_link_script(source: &std::path::Path, dir: &std::path::Path) -> Result<String, String> {
    let command = format!(
        "mkdir -p {} && ln -sfn {} {}",
        shell_quote(dir)?,
        shell_quote(source)?,
        shell_quote(&dir.join("kea"))?
    );
    Ok(format!(
        "do shell script \"{}\" with administrator privileges",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[tauri::command]
pub fn get_cli_shim_status(app: AppHandle) -> Result<String, String> {
    let source = app
        .path()
        .resolve(SHIM_RESOURCE, tauri::path::BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;
    let mut targets = vec![std::path::Path::new(SHIM_INSTALL_DIR).join("kea")];
    if let Ok(home) = app.path().home_dir() {
        targets.push(home.join(".local/bin/kea"));
    }
    Ok(targets
        .iter()
        .map(|target| shim_status(&source, target))
        .collect::<Vec<_>>()
        .join(". "))
}

fn shim_status(source: &std::path::Path, target: &std::path::Path) -> String {
    match std::fs::read_link(target) {
        Ok(path) if path == source && source.exists() => {
            format!("Already linked at {}", target.display())
        }
        Ok(path) => format!(
            "Stale link at {} → {}; install to update it",
            target.display(),
            path.display()
        ),
        Err(_) if target.exists() => format!("{} exists and is not a KEA link", target.display()),
        Err(_) => format!("Not installed at {}", target.display()),
    }
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
    fn admin_command_quotes_shell_and_applescript_independently() {
        let source = std::path::Path::new("/Apps/KEA's \"test\" $(touch bad)`bad`\\app/kea");
        let script = admin_link_script(source, std::path::Path::new("/usr/local/bin")).unwrap();
        assert!(script.starts_with("do shell script \"mkdir -p '/usr/local/bin' && ln -sfn '"));
        assert!(script.contains("KEA'\\\\''s"), "{script}");
        assert!(script.ends_with("with administrator privileges"));
        // A real shell parses the quoted path literally, without substitutions.
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", shell_quote(source).unwrap()))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            source.to_str().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_status_distinguishes_current_stale_and_missing_links() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("kea-cli");
        let target = dir.path().join("kea");
        std::fs::write(&source, "#!/bin/sh\n").unwrap();
        assert!(shim_status(&source, &target).starts_with("Not installed"));
        std::os::unix::fs::symlink(dir.path().join("old"), &target).unwrap();
        assert!(shim_status(&source, &target).starts_with("Stale link"));
        install_shim_at(&source, dir.path()).unwrap();
        assert!(shim_status(&source, &target).starts_with("Already linked"));
        std::fs::remove_file(&target).unwrap();
        std::fs::write(&target, "user tool").unwrap();
        assert!(install_shim_at(&source, dir.path())
            .unwrap_err()
            .contains("not a symlink"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user tool");
    }

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
