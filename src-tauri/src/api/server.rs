//! The loopback HTTP server: one unix socket, six routes, one loop.
//!
//! Modelled on [`crate::hotkeys`] — a table of routes and a dispatch loop —
//! for the same reason that module exists: the per-route parts of a request
//! belong next to the table that names them, and the composition root stays
//! composition.
//!
//! The security argument for the whole feature lives in [`super::auth`]. The
//! short version is that this listens on a **unix domain socket** at mode
//! `0600` under the app data directory, never on a TCP port: a socket file
//! gets filesystem access control for free and has no URL, so no web page can
//! reach it and DNS rebinding has nothing to rebind. `curl --unix-socket`
//! covers every scripting client that a port would have.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use serde::Deserialize;
use tauri::AppHandle;
use tokio::sync::watch;

use super::actions::{DictationVerb, KeaAction, RewriteRequest};
use super::auth::{authorize, Denied};
use super::exec::{execute, spends_llm_credits, ActionError, ActionOutcome};
use super::ratelimit::TokenBucket;
use super::token;
use crate::AppState;

/// Biggest request body accepted. A rewrite of a whole document is a few
/// hundred kilobytes; a megabyte is generous and stops a local process from
/// making KEA buffer its way out of memory.
const MAX_BODY: usize = 1024 * 1024;

/// One route: the method and path that name it, and the verb it produces.
///
/// A table rather than a `match` on `(method, path)` tuples so the route list
/// is one thing to read, and so a test can assert the paths without a socket.
struct Route {
    method: Method,
    path: &'static str,
    kind: RouteKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteKind {
    Rewrite,
    DictationStart,
    DictationStop,
    Speak,
    Transcribe,
    Status,
}

fn routes() -> Vec<Route> {
    vec![
        Route {
            method: Method::POST,
            path: "/v1/rewrite",
            kind: RouteKind::Rewrite,
        },
        Route {
            method: Method::POST,
            path: "/v1/dictation/start",
            kind: RouteKind::DictationStart,
        },
        Route {
            method: Method::POST,
            path: "/v1/dictation/stop",
            kind: RouteKind::DictationStop,
        },
        Route {
            method: Method::POST,
            path: "/v1/speak",
            kind: RouteKind::Speak,
        },
        Route {
            method: Method::POST,
            path: "/v1/transcribe",
            kind: RouteKind::Transcribe,
        },
        Route {
            // `status` needs the token like everything else: an
            // unauthenticated status endpoint tells any local process exactly
            // when the microphone is live.
            method: Method::GET,
            path: "/v1/status",
            kind: RouteKind::Status,
        },
    ]
}

fn match_route(method: &Method, path: &str) -> Option<RouteKind> {
    // Trailing slashes are what a script that joined paths produces.
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    routes()
        .into_iter()
        .find(|r| r.method == method && r.path == path)
        .map(|r| r.kind)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RewriteBody {
    text: Option<String>,
    mode: Option<String>,
    preset_id: Option<String>,
    instruction: Option<String>,
    #[serde(default)]
    insert: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeakBody {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscribeBody {
    path: String,
}

/// An empty body means "the defaults", which is what `curl -X POST` with no
/// `-d` sends and what a Shortcuts action produces.
fn parse_body<T: Default + for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ActionError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|e| ActionError::Refused(format!("bad JSON body: {e}")))
}

/// A JSON body into the same [`KeaAction`] the URL scheme parses into.
fn action_for(kind: RouteKind, body: &[u8]) -> Result<KeaAction, ActionError> {
    match kind {
        RouteKind::Rewrite => {
            let body: RewriteBody = parse_body(body)?;
            // The mode parser is `RewriteMode::from_str`, here as everywhere:
            // it already rejects an unknown value, and a second one would
            // eventually disagree with it.
            let mode = match body.mode.filter(|m| !m.is_empty()) {
                None => None,
                Some(raw) => Some(kea_core::rewrite::RewriteMode::from_str(&raw).ok_or_else(
                    || {
                        ActionError::Refused(
                            super::actions::ParseError::UnknownMode(raw).to_string(),
                        )
                    },
                )?),
            };
            let request = RewriteRequest {
                text: body.text.filter(|t| !t.is_empty()),
                mode,
                preset_id: body.preset_id.filter(|p| !p.is_empty()),
                instruction: body.instruction.filter(|i| !i.is_empty()),
                insert: body.insert,
            };
            request
                .validate()
                .map_err(|e| ActionError::Refused(e.to_string()))?;
            Ok(KeaAction::Rewrite(request))
        }
        RouteKind::DictationStart => Ok(KeaAction::Dictation(DictationVerb::Start)),
        RouteKind::DictationStop => Ok(KeaAction::Dictation(DictationVerb::Stop)),
        RouteKind::Speak => {
            let body: SpeakBody = parse_body(body)?;
            // Refused rather than ignored. Read-aloud runs the TTS feature
            // over the user's *selection*; there is no entry point that
            // speaks supplied text, and silently reading the selection
            // instead would be the wrong text out loud.
            if body.text.is_some() {
                return Err(ActionError::Refused(
                    "/v1/speak reads the current selection; it cannot speak supplied text yet"
                        .into(),
                ));
            }
            Ok(KeaAction::ReadAloud)
        }
        RouteKind::Transcribe => {
            let body: TranscribeBody = serde_json::from_slice(body)
                .map_err(|e| ActionError::Refused(format!("bad JSON body: {e}")))?;
            Ok(KeaAction::Transcribe {
                path: PathBuf::from(body.path),
            })
        }
        RouteKind::Status => Ok(KeaAction::Status),
    }
}

/// Everything one connection needs. Cheap to clone — the state and the app
/// handle are handles, and the limiter is shared on purpose.
#[derive(Clone)]
struct Runtime {
    state: Arc<AppState>,
    app: AppHandle,
    token: Arc<String>,
    limiter: Arc<Mutex<TokenBucket>>,
}

/// A running server. Dropping the handle does not stop it; [`Self::stop`]
/// does, so turning the API off is an explicit act with a tidy-up.
pub struct ServerHandle {
    shutdown: watch::Sender<bool>,
    pub socket_path: PathBuf,
}

impl ServerHandle {
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
        // Best effort: the accept loop unlinks too, but a listener that is
        // already gone leaves the file behind and the next bind would have to
        // clear it.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn json(status: StatusCode, body: serde_json::Value) -> Response<Full<Bytes>> {
    // **No CORS headers, not even a reflected origin.** A browser preflight
    // therefore has nothing to succeed against, which is the point.
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header("x-content-type-options", "nosniff")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::from("{}"))))
}

fn error_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    json(status, serde_json::json!({ "error": message }))
}

fn ok_response(outcome: ActionOutcome) -> Response<Full<Bytes>> {
    match outcome {
        ActionOutcome::Done => json(StatusCode::OK, serde_json::json!({ "ok": true })),
        ActionOutcome::Text(text) => json(StatusCode::OK, serde_json::json!({ "text": text })),
        ActionOutcome::Status(report) => json(
            StatusCode::OK,
            serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({})),
        ),
    }
}

fn status_code(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Everything a request settles before anything is carried out:
/// authorization, routing and body parsing.
///
/// Split from [`handle`] because this half needs no [`AppState`] — which is
/// what lets the socket test below drive the real auth and routing code over a
/// real unix socket instead of a stand-in.
///
/// The refusal is boxed: a `Response` is a few words of headers wider than a
/// `KeaAction`, and an unboxed `Err` would make every `Ok` pay for it.
fn plan_request(
    method: &Method,
    path: &str,
    headers: &http::HeaderMap,
    token: &[u8],
    body: &[u8],
) -> Result<KeaAction, Box<Response<Full<Bytes>>>> {
    if let Err(denied) = authorize(headers, token) {
        // The log line names the reason; the body does not. Telling a caller
        // which check it failed is free reconnaissance.
        tracing::warn!(%method, %path, reason = denied.reason(), "local API: request denied");
        return Err(Box::new(error_response(
            status_code(Denied::status(denied)),
            "unauthorized",
        )));
    }

    let Some(kind) = match_route(method, path) else {
        return Err(Box::new(error_response(
            StatusCode::NOT_FOUND,
            "no such endpoint",
        )));
    };

    action_for(kind, body).map_err(|e| {
        tracing::info!(%method, %path, error = %e, "local API: request refused");
        Box::new(error_response(status_code(e.status()), &e.to_string()))
    })
}

async fn handle(rt: Runtime, req: Request<Incoming>) -> Response<Full<Bytes>> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();

    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body is too large or could not be read",
            )
        }
    };

    let action = match plan_request(&method, &path, &headers, rt.token.as_bytes(), &body) {
        Ok(action) => action,
        Err(response) => return *response,
    };

    if spends_llm_credits(&action) {
        let (allowed, retry_after) = {
            let now = Instant::now();
            let mut bucket = match rt.limiter.lock() {
                Ok(b) => b,
                Err(poisoned) => poisoned.into_inner(),
            };
            (bucket.try_take(now), bucket.retry_after(now))
        };
        if !allowed {
            // Logged, always: a runaway script spending LLM credits is the
            // thing this limit exists for, and the log line is how the user
            // finds out which one it was.
            tracing::warn!(
                %method, %path,
                "local API: rate limited (see the api.max_rewrites_per_minute setting)"
            );
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "too many rewrites; see the API rate limit on the General settings page",
            );
            if let Ok(value) =
                http::HeaderValue::from_str(&retry_after.as_secs().max(1).to_string())
            {
                response
                    .headers_mut()
                    .insert(http::header::RETRY_AFTER, value);
            }
            return response;
        }
    }

    let verb = action.label();
    match execute(&rt.state, &rt.app, action).await {
        Ok(outcome) => {
            tracing::info!(%method, %path, verb, outcome = "ok", "local API");
            ok_response(outcome)
        }
        Err(e) => {
            tracing::warn!(%method, %path, verb, outcome = "error", error = %e, "local API");
            error_response(status_code(e.status()), &e.to_string())
        }
    }
}

/// Bind the socket and start accepting.
///
/// Returns once the socket is listening, so the settings page can report a
/// bind failure rather than claiming the API is on.
#[cfg(unix)]
pub async fn start(
    state: Arc<AppState>,
    app: AppHandle,
    app_data_dir: &std::path::Path,
    token: String,
    rewrites_per_minute: u32,
) -> Result<ServerHandle, String> {
    use hyper_util::rt::TokioIo;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    let socket_path = token::prepare_socket(app_data_dir)?;
    let listener = UnixListener::bind(&socket_path)
        .map_err(|e| format!("could not bind {}: {e}", socket_path.display()))?;
    // Explicit, not left to the umask: the filesystem permission *is* the
    // access control here, so a permissive umask must not be able to publish
    // the API to every account on the Mac.
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("could not restrict {}: {e}", socket_path.display()))?;

    let (shutdown, mut stop_rx) = watch::channel(false);
    let rt = Runtime {
        state,
        app,
        token: Arc::new(token),
        limiter: Arc::new(Mutex::new(TokenBucket::per_minute(
            rewrites_per_minute,
            Instant::now(),
        ))),
    };

    let listen_path = socket_path.clone();
    tauri::async_runtime::spawn(async move {
        tracing::info!(path = %listen_path.display(), "local API listening");
        loop {
            tokio::select! {
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() { break; }
                }
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(pair) => pair,
                        Err(e) => {
                            tracing::warn!(error = %e, "local API: accept failed");
                            continue;
                        }
                    };
                    let rt = rt.clone();
                    tauri::async_runtime::spawn(async move {
                        let service = hyper::service::service_fn(move |req| {
                            let rt = rt.clone();
                            async move { Ok::<_, Infallible>(handle(rt, req).await) }
                        });
                        if let Err(e) = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await
                        {
                            tracing::debug!(error = %e, "local API: connection ended");
                        }
                    });
                }
            }
        }
        // The listener drops here; the socket file goes with it so a restart
        // has nothing stale to clear.
        let _ = std::fs::remove_file(&listen_path);
        tracing::info!(path = %listen_path.display(), "local API stopped");
    });

    Ok(ServerHandle {
        shutdown,
        socket_path,
    })
}

/// The API is a unix-socket feature, deliberately (see [`super::auth`]). On a
/// platform without one there is nothing to fall back to that would still be
/// worth shipping, so it stays off rather than quietly becoming a TCP port.
#[cfg(not(unix))]
pub async fn start(
    _state: Arc<AppState>,
    _app: AppHandle,
    _app_data_dir: &std::path::Path,
    _token: String,
    _rewrites_per_minute: u32,
) -> Result<ServerHandle, String> {
    Err("the local API needs a unix domain socket, which this platform does not have".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// One raw HTTP/1.1 exchange over the socket, so the test drives hyper's
    /// framing rather than a client that shares its assumptions.
    #[cfg(unix)]
    async fn request(path: &std::path::Path, raw: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(path).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }

    /// The socket half, end to end: a stale file is cleared, hyper's `server`
    /// feature really does serve over a `tokio::net::UnixStream`, and the auth
    /// and routing a real request goes through are the ones under test.
    ///
    /// It stops short of executing an action, which needs an `AppState` and a
    /// microphone; [`plan_request`] is exactly the part that does not.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_socket_serves_authorized_requests_and_survives_a_stale_file() {
        use hyper_util::rt::TokioIo;

        let dir = tempfile::tempdir().unwrap();
        // A crash left a socket file behind; binding over it would fail with
        // EADDRINUSE, so the server has to unlink first.
        std::fs::write(token::socket_path(dir.path()), b"stale").unwrap();
        let socket = token::prepare_socket(dir.path()).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::task::spawn(async move {
                    let service = hyper::service::service_fn(|req: Request<Incoming>| async move {
                        let method = req.method().clone();
                        let path = req.uri().path().to_string();
                        let headers = req.headers().clone();
                        let body = Limited::new(req.into_body(), MAX_BODY)
                            .collect()
                            .await
                            .map(|c| c.to_bytes())
                            .unwrap_or_default();
                        let response = match plan_request(
                            &method,
                            &path,
                            &headers,
                            TEST_TOKEN.as_bytes(),
                            &body,
                        ) {
                            Ok(action) => json(
                                StatusCode::OK,
                                serde_json::json!({ "verb": action.label() }),
                            ),
                            Err(denied) => *denied,
                        };
                        Ok::<_, Infallible>(response)
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });

        let authorized = request(
            &socket,
            &format!(
                "GET /v1/status HTTP/1.1\r\nHost: localhost\r\nx-kea-token: {TEST_TOKEN}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");
        assert!(authorized.contains("\"verb\":\"status\""), "{authorized}");
        // No CORS headers, ever — that is what makes a browser preflight fail.
        assert!(
            !authorized.to_ascii_lowercase().contains("access-control-"),
            "{authorized}"
        );

        let anonymous = request(
            &socket,
            "GET /v1/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous}");
        // The body says nothing about which check failed.
        assert!(!anonymous.contains("token"), "{anonymous}");

        let from_a_web_page = request(
            &socket,
            &format!(
                "GET /v1/status HTTP/1.1\r\nHost: localhost\r\nOrigin: https://evil.example\r\nx-kea-token: {TEST_TOKEN}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(
            from_a_web_page.starts_with("HTTP/1.1 401"),
            "{from_a_web_page}"
        );

        let unknown = request(
            &socket,
            &format!(
                "GET /v1/nope HTTP/1.1\r\nHost: localhost\r\nx-kea-token: {TEST_TOKEN}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
    }

    #[test]
    fn every_documented_route_matches_and_nothing_else_does() {
        for route in routes() {
            assert_eq!(
                match_route(&route.method, route.path),
                Some(route.kind),
                "{} {}",
                route.method,
                route.path
            );
        }
        assert_eq!(match_route(&Method::GET, "/v1/rewrite"), None);
        assert_eq!(match_route(&Method::POST, "/v1/status"), None);
        assert_eq!(match_route(&Method::POST, "/rewrite"), None);
        assert_eq!(match_route(&Method::GET, "/"), None);
    }

    #[test]
    fn a_trailing_slash_is_the_same_route() {
        assert_eq!(
            match_route(&Method::GET, "/v1/status/"),
            Some(RouteKind::Status)
        );
    }

    #[test]
    fn an_empty_post_body_means_the_defaults() {
        let action = action_for(RouteKind::Rewrite, b"").unwrap();
        assert_eq!(
            action,
            KeaAction::Rewrite(RewriteRequest {
                insert: false,
                ..RewriteRequest::default()
            })
        );
    }

    #[test]
    fn a_rewrite_body_maps_onto_the_same_request_the_url_parses() {
        let json = br#"{"mode":"professional","preset_id":"p1","insert":true}"#;
        let action = action_for(RouteKind::Rewrite, json).unwrap();
        let super::KeaAction::Rewrite(req) = action else {
            panic!("expected a rewrite")
        };
        assert_eq!(req.mode, Some(kea_core::rewrite::RewriteMode::Professional));
        assert_eq!(req.preset_id.as_deref(), Some("p1"));
        assert!(req.insert);
    }

    #[test]
    fn an_unknown_mode_in_a_body_is_a_400_not_a_default() {
        let err = action_for(RouteKind::Rewrite, br#"{"mode":"Improve"}"#).unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(err.to_string().contains("unknown rewrite mode"), "{err}");
    }

    #[test]
    fn text_with_insert_is_refused_over_http_too() {
        let err = action_for(RouteKind::Rewrite, br#"{"text":"hi","insert":true}"#).unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        // A caller that spelled `preset` instead of `preset_id` should hear
        // about it, not get the default preset silently.
        let err = action_for(RouteKind::Rewrite, br#"{"preset":"p1"}"#).unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn speak_refuses_supplied_text_instead_of_reading_the_selection() {
        assert!(action_for(RouteKind::Speak, br#"{"text":"hello"}"#).is_err());
        assert_eq!(
            action_for(RouteKind::Speak, b"{}").unwrap(),
            KeaAction::ReadAloud
        );
    }

    #[test]
    fn transcribe_requires_a_path() {
        assert!(action_for(RouteKind::Transcribe, b"{}").is_err());
        assert_eq!(
            action_for(RouteKind::Transcribe, br#"{"path":"/Users/x/a.m4a"}"#).unwrap(),
            KeaAction::Transcribe {
                path: PathBuf::from("/Users/x/a.m4a")
            }
        );
    }

    #[test]
    fn the_dictation_routes_produce_directed_verbs_not_a_toggle() {
        assert_eq!(
            action_for(RouteKind::DictationStart, b"").unwrap(),
            KeaAction::Dictation(DictationVerb::Start)
        );
        assert_eq!(
            action_for(RouteKind::DictationStop, b"").unwrap(),
            KeaAction::Dictation(DictationVerb::Stop)
        );
    }
}
