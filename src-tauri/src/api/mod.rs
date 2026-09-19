//! The scriptable surface: the `kea://` URL scheme and the local HTTP API.
//!
//! Two entry points, one vocabulary. [`actions`] parses both a URL and a JSON
//! body into the same [`KeaAction`]; [`exec`] carries one out through the same
//! busy flags and `*_inner` functions a hotkey press uses. Nothing here is a
//! second path to work that already has one.
//!
//! - [`actions`] — the grammar, and the only parser for it.
//! - [`auth`] — who may call, and the threat model, written down.
//! - [`ratelimit`] — a token bucket over the verbs that spend LLM credits.
//! - [`server`] — the unix socket, the route table and the accept loop.
//! - [`token`] — the bearer token, the socket path, the CLI shim's token file.
//! - [`settings`] — the commands the General settings page drives this with.

pub mod actions;
pub mod auth;
pub mod exec;
pub mod ratelimit;
pub mod server;
pub mod settings;
pub mod token;

use std::sync::Arc;

use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

pub use actions::parse_kea_url;
pub use server::ServerHandle;

use crate::commands::notify_user;
use crate::AppState;

/// Whether the local API is on. Absent reads as `false` with no migration,
/// which is how "off by default" is spelled in a settings table whose rows
/// only exist once written.
pub const ENABLED_SETTING: &str = "api.enabled";

/// The rate limit on the LLM-calling verbs, per minute.
pub const RATE_LIMIT_SETTING: &str = "api.max_rewrites_per_minute";

/// The sender the `RunEvent::Opened` handler pushes URLs into.
///
/// A newtype so it can be `manage`d and found again from the run loop without
/// growing [`AppState`] a field that only one callback reads.
pub struct KeaUrlSender(pub mpsc::UnboundedSender<String>);

/// Hand a `kea://` URL to the drain, if one is running.
///
/// Called from the sync `RunEvent` closure, which must not block or await —
/// pushing into an unbounded channel is the whole point of the drain.
pub fn dispatch_url(app: &AppHandle, url: String) {
    match app.try_state::<KeaUrlSender>() {
        Some(sender) => {
            if sender.0.send(url).is_err() {
                tracing::warn!("a kea:// URL arrived after the URL handler stopped");
            }
        }
        // Possible in principle if a URL is delivered before `setup` finishes
        // managing the sender. Logged rather than queued: there is nowhere to
        // queue it that is not the channel we are looking for.
        None => tracing::warn!("a kea:// URL arrived before the URL handler was ready"),
    }
}

/// Drain `kea://` invocations into [`exec::execute`].
///
/// The sibling of `spawn_macos_rewrite_service`, and the same shape for the
/// same reason: LaunchServices can deliver a URL at a moment the app is not
/// ready to act on it, and a channel plus a task started at the end of `setup`
/// is what decouples arrival from handling. The handler also must not run on
/// the event-loop thread — a rewrite is an HTTP round trip to an LLM.
pub fn spawn_url_service(state: &Arc<AppState>, app: &AppHandle) -> mpsc::UnboundedSender<String> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(raw) = rx.recv().await {
            // One at a time, deliberately: two URLs arriving together are two
            // synthetic ⌘V at the same app, and the busy flags inside
            // `execute` would refuse the second anyway. Awaiting here turns
            // that refusal into an ordering.
            let action = match parse_kea_url(&raw) {
                Ok(action) => action,
                Err(e) => {
                    tracing::warn!(url = %raw, error = %e, "kea:// URL refused");
                    notify_user(&app, &format!("KEA could not run that link: {e}"));
                    continue;
                }
            };
            let verb = action.label();
            tracing::info!(url = %raw, verb, "kea:// URL");
            // Not rate limited: a URL is a user action routed by
            // LaunchServices, not a socket a script can hammer, and the busy
            // flags already serialise it.
            match exec::execute(&state, &app, action).await {
                Ok(_) => tracing::info!(verb, outcome = "ok", "kea:// URL"),
                Err(e) => {
                    tracing::warn!(verb, outcome = "error", error = %e, "kea:// URL");
                    notify_user(&app, &e.to_string());
                }
            }
        }
    });

    tx
}

/// Start the API server if the setting says so.
///
/// Called at the end of `setup` and again whenever the toggle is flipped on.
/// A failure is logged rather than fatal: the app works without the API, and
/// the settings page reports the same error when the user turns it on by hand.
pub async fn start_if_enabled(state: &Arc<AppState>, app: &AppHandle) {
    if !crate::commands::read_bool_setting(&state.config_pool, ENABLED_SETTING, false).await {
        return;
    }
    if let Err(e) = settings::start_server(state, app).await {
        tracing::warn!(error = %e, "the local API is enabled but did not start");
    }
}
