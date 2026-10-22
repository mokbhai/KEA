//! Tests for the `kea` command-line shim (src-tauri/resources/kea-cli).
//!
//! The shim is POSIX sh, so the tests run it with `sh` against a hyper mock of
//! KEA's local HTTP API. The mock serves the model routes over a Unix socket and
//! over TCP on 127.0.0.1, records every request it receives, and answers with
//! fixed data. A test can then assert both what the shim sent (method, path,
//! bearer token, multipart fields, JSON bodies) and what it printed.
//!
//! Run with `cargo test -p kea-app --test kea_cli`. Needs `sh`, `curl`, `awk`,
//! `od` and, for the terminal-guard test, `script`.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::oneshot;

const SHIM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/kea-cli");
const TOKEN: &str = "test-token-123";
const TRANSCRIPT: &str = r#"hello {there} second "quote""#;
/// (id, start, end, text) for each transcript segment.
const SEGMENTS: [(u64, f64, f64, &str); 2] = [
    (0, 0.0, 1.5, "hello {there}"),
    (1, 1.5, 3.0, "second \"quote\""),
];
/// The line in the shim that starts argument handling. Everything above it is
/// definitions only, so the JSON-escaping test can load that part alone.
const SHIM_PRELUDE_END: &str = "# --api-url may come before the command as well as after it.";

// ---------------------------------------------------------------------------
// Mock KEA API
// ---------------------------------------------------------------------------

type Log = Arc<Mutex<Vec<Value>>>;

/// One multipart part: a text field, or a file with its name and bytes.
#[derive(Debug, Clone, PartialEq)]
enum Field {
    Text(String),
    File { filename: String, bytes: Vec<u8> },
}

type Fields = BTreeMap<String, Field>;

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

/// A running mock on both transports. Each test starts its own, so the request
/// log is never shared between tests that run in parallel.
struct MockApi {
    socket_path: PathBuf,
    port: u16,
    log: Log,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl MockApi {
    fn start(root: &Path) -> Self {
        let socket_path = root.join("kea.sock");
        // Bound here, on the test thread, so the port and socket exist before
        // any test runs; the runtime on the server thread adopts them.
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a TCP port");
        tcp.set_nonblocking(true).expect("nonblocking TCP listener");
        let port = tcp.local_addr().expect("TCP local address").port();
        let unix =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind the Unix socket");
        unix.set_nonblocking(true)
            .expect("nonblocking Unix listener");

        let log: Log = Arc::default();
        let (shutdown, stop) = oneshot::channel::<()>();
        let thread_log = log.clone();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build the mock runtime");
            runtime.block_on(async move {
                let tcp = TcpListener::from_std(tcp).expect("adopt the TCP listener");
                let unix = UnixListener::from_std(unix).expect("adopt the Unix listener");
                tokio::select! {
                    () = serve_tcp(tcp, thread_log.clone()) => {}
                    () = serve_unix(unix, thread_log) => {}
                    _ = stop => {}
                }
            });
        });
        MockApi {
            socket_path,
            port,
            log,
            shutdown: Some(shutdown),
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for MockApi {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve_tcp(listener: TcpListener, log: Log) {
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            spawn_connection(stream, "tcp", log.clone());
        }
    }
}

async fn serve_unix(listener: UnixListener, log: Log) {
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            spawn_connection(stream, "unix", log.clone());
        }
    }
}

fn spawn_connection<S>(stream: S, transport: &'static str, log: Log)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let service = service_fn(move |req| handle(req, transport, log.clone()));
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await;
    });
}

async fn handle(
    req: Request<Incoming>,
    transport: &'static str,
    log: Log,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = req.method().as_str().to_string();
    let path = req
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_string(), |p| p.as_str().to_string());
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes().to_vec())
        .unwrap_or_default();

    let mut record = json!({
        "transport": transport,
        "method": method,
        "path": path,
        "authorization": auth,
        "content_type": content_type,
    });
    let mut fields = Fields::new();
    let mut body_json = Value::Null;
    if content_type.starts_with("multipart/form-data") {
        fields = parse_multipart(&content_type, &body);
        record["fields"] = jsonable_fields(&fields);
    } else if !body.is_empty() {
        record["raw_body"] = json!(String::from_utf8_lossy(&body));
        if let Ok(parsed) = serde_json::from_slice::<Value>(&body) {
            record["json"] = parsed.clone();
            body_json = parsed;
        }
    }
    log.lock().expect("request log").push(record);

    let expected = format!("Bearer {TOKEN}");
    let reply = if auth.as_deref() == Some(expected.as_str()) {
        route(&method, &path, &fields, &body_json)
    } else {
        json_reply(
            401,
            error_body(
                "Invalid or missing token",
                "authentication_error",
                Some("invalid_token"),
            ),
        )
    };
    Ok(Response::builder()
        .status(reply.status)
        .header("content-type", reply.content_type)
        .header("content-length", reply.body.len().to_string())
        .body(Full::new(Bytes::from(reply.body)))
        .expect("build the response"))
}

fn json_reply(status: u16, value: Value) -> Reply {
    Reply {
        status,
        content_type: "application/json",
        body: to_json_ascii(&value),
    }
}

/// Serializes like Python's `json.dumps` with its default `ensure_ascii`: every
/// non-ASCII character becomes a `\uXXXX` escape, with surrogate pairs above the
/// BMP, so the shim's decoder sees the escapes it has to handle.
fn to_json_ascii(value: &Value) -> Vec<u8> {
    let mut out = String::new();
    for c in serde_json::to_string(value)
        .expect("serialize JSON")
        .chars()
    {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                write!(out, "\\u{unit:04x}").expect("write to String");
            }
        }
    }
    out.into_bytes()
}

fn error_body(message: &str, error_type: &str, code: Option<&str>) -> Value {
    json!({"error": {"message": message, "type": error_type, "code": code}})
}

fn text<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    match fields.get(name) {
        Some(Field::Text(t)) => Some(t.as_str()),
        _ => None,
    }
}

fn file_bytes<'a>(fields: &'a Fields, name: &str) -> &'a [u8] {
    match fields.get(name) {
        Some(Field::File { bytes, .. }) => bytes,
        _ => &[],
    }
}

fn wav_bytes() -> Vec<u8> {
    // Mono, 16-bit, 16 kHz. The samples include zero bytes, so any text-mode
    // handling of the body would corrupt it.
    let data: Vec<u8> = b"\x00\x00\x01\x00\xff\x7f".repeat(200);
    let data_len = u32::try_from(data.len()).expect("small WAV");
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // channels
    out.extend_from_slice(&16_000u32.to_le_bytes()); // sample rate
    out.extend_from_slice(&32_000u32.to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&data);
    out
}

fn route(method: &str, path: &str, fields: &Fields, body: &Value) -> Reply {
    if method == "GET" && path == "/v1/models" {
        return json_reply(200, models());
    }
    if method == "GET" && path == "/v1/status" {
        return json_reply(200, json!({"version": "test"}));
    }

    if method == "POST" && path == "/v1/audio/transcriptions" {
        if file_bytes(fields, "file").starts_with(b"BAD") {
            return json_reply(
                400,
                error_body(
                    "could not decode the uploaded audio",
                    "invalid_request_error",
                    Some("decode_failed"),
                ),
            );
        }
        if text(fields, "model") == Some("nope") {
            return json_reply(
                404,
                error_body(
                    "The model 'nope' does not exist",
                    "invalid_request_error",
                    Some("model_not_found"),
                ),
            );
        }
        let speakers = text(fields, "speakers") == Some("true");
        return match text(fields, "response_format").unwrap_or("json") {
            "verbose_json" => {
                let segments: Vec<Value> = SEGMENTS
                    .iter()
                    .map(|&(id, start, end, seg_text)| {
                        let mut seg =
                            json!({"id": id, "start": start, "end": end, "text": seg_text});
                        if speakers {
                            seg["speaker"] = json!(format!("Person {}", id + 1));
                        }
                        seg
                    })
                    .collect();
                json_reply(
                    200,
                    json!({
                        "task": "transcribe",
                        "language": "en",
                        "duration": 3.0,
                        "text": TRANSCRIPT,
                        "segments": segments,
                    }),
                )
            }
            "json" => json_reply(200, json!({"text": TRANSCRIPT})),
            "text" => Reply {
                status: 200,
                content_type: "text/plain",
                body: TRANSCRIPT.as_bytes().to_vec(),
            },
            "srt" => Reply {
                status: 200,
                content_type: "application/x-subrip",
                body: concat!(
                    "1\n00:00:00,000 --> 00:00:01,500\nhello {there}\n\n",
                    "2\n00:00:01,500 --> 00:00:03,000\nsecond \"quote\"\n\n",
                )
                .as_bytes()
                .to_vec(),
            },
            "vtt" => Reply {
                status: 200,
                content_type: "text/vtt",
                body: concat!(
                    "WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nhello {there}\n\n",
                    "00:00:01.500 --> 00:00:03.000\nsecond \"quote\"\n\n",
                )
                .as_bytes()
                .to_vec(),
            },
            other => json_reply(
                400,
                error_body(
                    &format!("unknown response_format {other}"),
                    "invalid_request_error",
                    None,
                ),
            ),
        };
    }

    if method == "POST" && path == "/v1/audio/speech" {
        let input = body.get("input").and_then(Value::as_str);
        if input == Some("__409__") {
            return json_reply(
                409,
                error_body(
                    "No text-to-speech model is bound for API use. Choose one on the Models page.",
                    "invalid_request_error",
                    Some("model_not_bound"),
                ),
            );
        }
        if body.get("model").and_then(Value::as_str) == Some("nope") {
            return json_reply(
                404,
                error_body(
                    "The model 'nope' does not exist",
                    "invalid_request_error",
                    Some("model_not_found"),
                ),
            );
        }
        if let Some(speed) = body.get("speed").and_then(Value::as_f64) {
            if !(0.25..=4.0).contains(&speed) {
                return json_reply(
                    400,
                    error_body(
                        "speed must be between 0.25 and 4.0",
                        "invalid_request_error",
                        Some("invalid_speed"),
                    ),
                );
            }
        }
        let format = body
            .get("response_format")
            .and_then(Value::as_str)
            .unwrap_or("wav");
        if format != "wav" {
            return json_reply(
                400,
                error_body(
                    "only wav and pcm are supported",
                    "invalid_request_error",
                    None,
                ),
            );
        }
        return Reply {
            status: 200,
            content_type: "audio/wav",
            body: wav_bytes(),
        };
    }

    if method == "POST" && path == "/v1/chat/completions" {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if messages.iter().any(|m| m["content"].is_array()) {
            return json_reply(
                400,
                error_body(
                    "only text content is supported",
                    "invalid_request_error",
                    None,
                ),
            );
        }
        if body.get("model").and_then(Value::as_str) == Some("nope") {
            return json_reply(
                404,
                error_body(
                    "The model 'nope' does not exist",
                    "invalid_request_error",
                    Some("model_not_found"),
                ),
            );
        }
        let user = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string();
        return json_reply(
            200,
            json!({
                "id": "chatcmpl-test",
                "object": "chat.completion",
                "created": 1,
                "model": "qwen-chat",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": user},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            }),
        );
    }

    json_reply(
        404,
        error_body("no such route", "invalid_request_error", Some("not_found")),
    )
}

fn models() -> Value {
    json!({
        "object": "list",
        "data": [
            {"id": "whisper-large", "object": "model", "kea_capability": "stt", "kea_default": true},
            {"id": "kokoro-en", "object": "model", "kea_capability": "tts", "kea_default": false},
            {"id": "qwen-chat", "object": "model", "kea_capability": "llm", "kea_default": true},
        ],
    })
}

// ---------------------------------------------------------------------------
// Multipart parsing (the server side of `curl -F`)
// ---------------------------------------------------------------------------

fn parse_multipart(content_type: &str, body: &[u8]) -> Fields {
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))
        .unwrap_or("")
        .trim_matches('"');
    let delimiter = format!("--{boundary}");
    let mut fields = Fields::new();
    for part in split_on(body, delimiter.as_bytes()).into_iter().skip(1) {
        if part.starts_with(b"--") {
            break;
        }
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let Some(split) = find(part, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&part[..split]);
        let content = &part[split + 4..];
        let content = content.strip_suffix(b"\r\n").unwrap_or(content);
        let Some(name) = header_param(&head, "name") else {
            continue;
        };
        if let Some(filename) = header_param(&head, "filename") {
            fields.insert(
                name,
                Field::File {
                    filename,
                    bytes: content.to_vec(),
                },
            );
        } else {
            fields.insert(
                name,
                Field::Text(String::from_utf8_lossy(content).into_owned()),
            );
        }
    }
    fields
}

fn split_on<'a>(haystack: &'a [u8], needle: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut rest = haystack;
    while let Some(i) = find(rest, needle) {
        parts.push(&rest[..i]);
        rest = &rest[i + needle.len()..];
    }
    parts.push(rest);
    parts
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The quoted value of `key="..."` in a Content-Disposition header. Only a whole
/// parameter name matches, so `name` does not match inside `filename`.
fn header_param(head: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let mut from = 0;
    while let Some(i) = head[from..].find(&needle) {
        let at = from + i;
        let start = at + needle.len();
        let whole = !head[..at]
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if whole {
            let end = head[start..].find('"')?;
            return Some(head[start..start + end].to_string());
        }
        from = start;
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn jsonable_fields(fields: &Fields) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(name, field)| {
                let value = match field {
                    Field::Text(t) => json!(t),
                    Field::File { filename, bytes } => json!({
                        "filename": filename,
                        "length": bytes.len(),
                        "sha256": sha256_hex(bytes),
                    }),
                };
                (name.clone(), value)
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    home: PathBuf,
    token_file: PathBuf,
    work: PathBuf,
    api: MockApi,
    // Declared last so the mock thread is joined before the directory goes.
    _root: TempDir,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("kea-cli-test-")
            .tempdir()
            .expect("create a temp dir");
        let api = MockApi::start(root.path());
        let token_file = root.path().join("api-token");
        std::fs::write(&token_file, format!("{TOKEN}\n")).expect("write the token file");
        let home = root.path().join("home");
        let work = root.path().join("work");
        std::fs::create_dir_all(&home).expect("create HOME");
        std::fs::create_dir_all(&work).expect("create the work dir");
        Harness {
            home,
            token_file,
            work,
            api,
            _root: root,
        }
    }

    /// Writes a file in the work dir. Returns its path and its SHA-256.
    fn write_file(&self, name: &str, data: &[u8]) -> (String, String) {
        let path = self.work.join(name);
        std::fs::write(&path, data).expect("write the test input");
        (
            path.to_str().expect("UTF-8 path").to_string(),
            sha256_hex(data),
        )
    }

    fn apply_env(&self, cmd: &mut Command, env: &[(&str, Option<&str>)]) {
        cmd.env("HOME", &self.home)
            .env("KEA_API_TOKEN_FILE", &self.token_file)
            .env("KEA_API_SOCKET", &self.api.socket_path)
            .env("KEA_API_URL", self.api.base_url());
        for &(key, value) in env {
            match value {
                Some(v) => {
                    cmd.env(key, v);
                }
                None => {
                    cmd.env_remove(key);
                }
            }
        }
    }

    /// Runs the shim. Stdin is closed unless `stdin` is given. An env value of
    /// `None` removes that variable for the run.
    fn run(&self, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, Option<&str>)]) -> Output {
        let mut cmd = Command::new("sh");
        cmd.arg(SHIM)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_env(&mut cmd, env);
        match stdin {
            Some(data) => {
                cmd.stdin(Stdio::piped());
                let mut child = cmd.spawn().expect("spawn sh");
                // A shim that never reads stdin may close it first; that is fine.
                let _ = child.stdin.take().expect("piped stdin").write_all(data);
                child.wait_with_output().expect("wait for the shim")
            }
            None => {
                cmd.stdin(Stdio::null());
                cmd.output().expect("run the shim")
            }
        }
    }

    fn shim(&self, args: &[&str]) -> Output {
        self.run(args, None, &[])
    }

    fn shim_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        self.run(args, Some(stdin), &[])
    }

    fn shim_env(&self, args: &[&str], env: &[(&str, Option<&str>)]) -> Output {
        self.run(args, None, env)
    }

    fn requests(&self) -> Vec<Value> {
        self.api.log.lock().expect("request log").clone()
    }

    fn last(&self) -> Value {
        self.requests()
            .last()
            .cloned()
            .expect("the mock received no request")
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn out_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn err_text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

// ---------------------------------------------------------------------------
// stt
// ---------------------------------------------------------------------------

mod stt {
    use super::*;

    #[test]
    fn default_text_format_prints_the_transcript() {
        let h = Harness::new();
        let (path, sha) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(out_text(&r), format!("{TRANSCRIPT}\n"));
        let req = h.last();
        assert_eq!(req["method"], "POST");
        assert_eq!(req["path"], "/v1/audio/transcriptions");
        assert_eq!(req["authorization"], bearer());
        assert_eq!(req["fields"]["response_format"], "text");
        assert_eq!(req["fields"]["file"]["sha256"], sha);
        assert_eq!(req["fields"]["file"]["filename"], "call.m4a");
        assert!(req["fields"].get("speakers").is_none());
        assert!(req["fields"].get("language").is_none());
    }

    #[test]
    fn json_format_prints_the_json_body() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "json"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let body: Value = serde_json::from_str(&out_text(&r)).expect("stdout is JSON");
        assert_eq!(body, json!({"text": TRANSCRIPT}));
        assert_eq!(h.last()["fields"]["response_format"], "json");
    }

    #[test]
    fn srt_passes_through_without_speakers() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "srt"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert!(out_text(&r).contains("00:00:00,000 --> 00:00:01,500\nhello {there}\n"));
        assert_eq!(h.last()["fields"]["response_format"], "srt");
    }

    #[test]
    fn stdin_input_is_uploaded() {
        let h = Harness::new();
        let r = h.shim_stdin(&["stt", "-"], b"PIPED-AUDIO");
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(
            h.last()["fields"]["file"]["sha256"],
            sha256_hex(b"PIPED-AUDIO")
        );
    }

    #[test]
    fn srt_with_speakers_prefixes_each_cue() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "srt", "--speakers"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(
            out_text(&r),
            "1\n00:00:00,000 --> 00:00:01,500\nPerson 1: hello {there}\n\n\
             2\n00:00:01,500 --> 00:00:03,000\nPerson 2: second \"quote\"\n\n"
        );
        let req = h.last();
        assert_eq!(req["fields"]["response_format"], "verbose_json");
        assert_eq!(req["fields"]["speakers"], "true");
    }

    #[test]
    fn vtt_with_speakers() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "vtt", "--speakers"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(
            out_text(&r),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nPerson 1: hello {there}\n\n\
             00:00:01.500 --> 00:00:03.000\nPerson 2: second \"quote\"\n\n"
        );
    }

    #[test]
    fn text_with_speakers_prints_one_line_per_segment() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--speakers"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(
            out_text(&r),
            "Person 1: hello {there}\nPerson 2: second \"quote\"\n"
        );
    }

    #[test]
    fn verbose_json_with_speakers_is_the_raw_body() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "verbose_json", "--speakers"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let body: Value = serde_json::from_str(&out_text(&r)).expect("stdout is JSON");
        let speakers: Vec<&str> = body["segments"]
            .as_array()
            .expect("segments")
            .iter()
            .map(|s| s["speaker"].as_str().expect("speaker"))
            .collect();
        assert_eq!(speakers, ["Person 1", "Person 2"]);
    }

    #[test]
    fn language_and_model_are_sent_as_form_fields() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--language", "de", "--model", "whisper-large"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let fields = h.last()["fields"].clone();
        assert_eq!(fields["language"], "de");
        assert_eq!(fields["model"], "whisper-large");
    }

    #[test]
    fn undecodable_upload_prints_the_api_message_and_exits_1() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"BAD-AUDIO");
        let r = h.shim(&["stt", &path]);
        assert_eq!(code(&r), 1);
        assert!(r.stdout.is_empty());
        assert!(err_text(&r).contains("kea: could not decode the uploaded audio"));
    }

    #[test]
    fn a_path_with_separators_curl_would_split_is_uploaded_whole() {
        let h = Harness::new();
        for name in ["a;b.wav", "c,d.wav", "quote\"d.wav"] {
            let (path, sha) = h.write_file(name, b"AUDIO-BYTES");
            let r = h.shim(&["stt", &path]);
            assert_eq!(code(&r), 0, "{name}: {}", err_text(&r));
            assert_eq!(h.last()["fields"]["file"]["sha256"], sha, "{name}");
        }
    }

    #[test]
    fn a_file_that_does_not_exist_exits_66_without_blaming_the_api() {
        let h = Harness::new();
        let missing = h.work.join("nope.m4a");
        let r = h.shim(&["stt", missing.to_str().expect("UTF-8 path")]);
        assert_eq!(code(&r), 66);
        assert!(err_text(&r).contains("cannot read"));
        assert!(!err_text(&r).contains("not running"));
    }

    #[test]
    fn missing_file_argument_is_a_usage_error() {
        let h = Harness::new();
        assert_eq!(code(&h.shim(&["stt"])), 64);
    }

    #[test]
    fn unknown_format_is_a_usage_error() {
        let h = Harness::new();
        let (path, _) = h.write_file("call.m4a", b"AUDIO-BYTES");
        let r = h.shim(&["stt", &path, "--format", "mp3"]);
        assert_eq!(code(&r), 64);
        assert!(h.requests().is_empty());
    }
}

// ---------------------------------------------------------------------------
// tts
// ---------------------------------------------------------------------------

mod tts {
    use super::*;

    #[test]
    fn out_writes_a_valid_wav_and_sends_every_field() {
        let h = Harness::new();
        let out = h.work.join("done.wav");
        let out_str = out.to_str().expect("UTF-8 path");
        let r = h.shim(&[
            "tts",
            "Build finished",
            "--out",
            out_str,
            "--voice",
            "af_bella",
            "--speed",
            "1.25",
            "--model",
            "kokoro-en",
        ]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert!(r.stdout.is_empty());
        assert_eq!(std::fs::read(&out).expect("read the WAV"), wav_bytes());
        let reader = hound::WavReader::open(&out).expect("a valid WAV");
        let spec = reader.spec();
        assert_eq!(
            (spec.channels, spec.bits_per_sample, spec.sample_rate),
            (1, 16, 16_000)
        );
        let req = h.last();
        assert_eq!(req["path"], "/v1/audio/speech");
        assert_eq!(req["authorization"], bearer());
        assert_eq!(
            req["json"],
            json!({
                "input": "Build finished",
                "response_format": "wav",
                "voice": "af_bella",
                "speed": 1.25,
                "model": "kokoro-en",
            })
        );
    }

    #[test]
    fn piped_stdout_receives_the_wav_bytes() {
        let h = Harness::new();
        let r = h.shim(&["tts", "Hello"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(r.stdout, wav_bytes());
        assert_eq!(
            h.last()["json"],
            json!({"input": "Hello", "response_format": "wav"})
        );
    }

    #[test]
    fn stdin_text() {
        let h = Harness::new();
        let out = h.work.join("stdin.wav");
        let r = h.shim_stdin(
            &["tts", "-", "--out", out.to_str().expect("UTF-8 path")],
            b"from stdin",
        );
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(h.last()["json"]["input"], "from stdin");
    }

    #[test]
    fn terminal_guard_refuses_without_a_request() {
        // The guard checks that stdout is a terminal, so the shim runs under
        // `script`, which gives it a real pseudo-terminal. script joins stdout
        // and stderr on that terminal, so both show up in one stream. The
        // shim's exit status is echoed as a marker, because script's own exit
        // status is not reliable across platforms.
        let h = Harness::new();
        let inner = format!("sh {} tts Hello; echo __KEA_STATUS=$?", shell_quote(SHIM));
        let mut cmd = Command::new("script");
        if cfg!(target_os = "macos") {
            // BSD script: `script -q FILE COMMAND...`.
            cmd.args([
                "-q",
                "/dev/null",
                "sh",
                "-c",
                r#"sh "$0" tts Hello; echo "__KEA_STATUS=$?""#,
                SHIM,
            ]);
        } else {
            // util-linux script: `script -q -c COMMAND FILE`.
            cmd.args(["-q", "-c", &inner, "/dev/null"]);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        h.apply_env(&mut cmd, &[]);
        let output = match cmd.output() {
            Ok(output) => output,
            Err(e) => {
                eprintln!("skipping the tts terminal-guard test: cannot run `script` ({e})");
                return;
            }
        };
        let terminal = out_text(&output);
        let status = terminal
            .split("__KEA_STATUS=")
            .nth(1)
            .map(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .unwrap_or_else(|| panic!("no status marker; script said: {}", err_text(&output)));
        assert_eq!(status, "64", "terminal output: {terminal:?}");
        assert!(terminal.contains("--out"), "terminal output: {terminal:?}");
        assert!(!terminal.contains("RIFF"), "audio reached the terminal");
        assert!(
            h.requests().is_empty(),
            "no request should be made to a terminal"
        );
    }

    #[test]
    fn api_error_prints_the_message_and_writes_no_file() {
        let h = Harness::new();
        let out = h.work.join("never.wav");
        let r = h.shim(&["tts", "__409__", "--out", out.to_str().expect("UTF-8 path")]);
        assert_eq!(code(&r), 1);
        assert!(err_text(&r).contains("kea: No text-to-speech model is bound for API use."));
        assert!(!out.exists());
    }

    #[test]
    fn unknown_model_is_an_api_error() {
        let h = Harness::new();
        let r = h.shim(&["tts", "hi", "--model", "nope"]);
        assert_eq!(code(&r), 1);
        assert!(r.stdout.is_empty());
        assert!(err_text(&r).contains("kea: The model 'nope' does not exist"));
    }

    #[test]
    fn speed_outside_the_range_is_refused_by_the_api() {
        let h = Harness::new();
        let r = h.shim(&["tts", "hi", "--speed", "9"]);
        assert_eq!(code(&r), 1);
        assert!(err_text(&r).contains("speed must be between 0.25 and 4.0"));
    }

    #[test]
    fn speed_that_is_not_a_number_is_a_usage_error() {
        let h = Harness::new();
        let r = h.shim(&["tts", "hi", "--speed", "fast"]);
        assert_eq!(code(&r), 64);
        assert!(h.requests().is_empty());
    }

    fn shell_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ---------------------------------------------------------------------------
// llm
// ---------------------------------------------------------------------------

mod llm {
    use super::*;

    // Quotes, backslashes, tab, newline, a control character, a non-BMP
    // character and an accented one: every case the escaper has to get right.
    const PROMPT: &str = "Quote \" back \\ tab\t newline\nctrl\x01 emoji \u{1F600} é 中";

    #[test]
    fn prompt_survives_escaping_and_decoding_unchanged() {
        let h = Harness::new();
        let r = h.shim(&["llm", PROMPT]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(out_text(&r), format!("{PROMPT}\n"));
        assert_eq!(
            h.last()["json"]["messages"],
            json!([{"role": "user", "content": PROMPT}])
        );
    }

    #[test]
    fn piped_stdin_is_appended_after_a_blank_line() {
        let h = Harness::new();
        let r = h.shim_stdin(
            &["llm", "Write a commit message for this"],
            b"diff --git a b\n",
        );
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let last = h.last();
        let messages = last["json"]["messages"].as_array().expect("messages");
        assert_eq!(
            messages.last().expect("a message")["content"],
            "Write a commit message for this\n\ndiff --git a b"
        );
    }

    #[test]
    fn dash_reads_the_prompt_from_stdin() {
        let h = Harness::new();
        let r = h.shim_stdin(&["llm", "-"], b"from stdin\n");
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(out_text(&r), "from stdin\n");
    }

    #[test]
    fn system_and_model_are_sent() {
        let h = Harness::new();
        let r = h.shim(&["llm", "hi", "--system", "Be terse.", "--model", "qwen-chat"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let body = h.last()["json"].clone();
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "Be terse."},
                {"role": "user", "content": "hi"},
            ])
        );
        assert_eq!(body["model"], "qwen-chat");
    }

    #[test]
    fn provider_style_error_is_an_api_error() {
        let h = Harness::new();
        let r = h.shim(&["llm", "hi", "--model", "nope"]);
        assert_eq!(code(&r), 1);
        assert!(err_text(&r).contains("kea: The model 'nope' does not exist"));
    }
}

// ---------------------------------------------------------------------------
// models
// ---------------------------------------------------------------------------

mod models {
    use super::*;

    #[test]
    fn lists_capabilities_and_marks_defaults() {
        let h = Harness::new();
        let r = h.shim(&["models"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(
            out_text(&r),
            "whisper-large  stt  [default]\nkokoro-en  tts\nqwen-chat  llm  [default]\n"
        );
        let req = h.last();
        assert_eq!(req["method"], "GET");
        assert_eq!(req["authorization"], bearer());
    }

    #[test]
    fn json_prints_the_raw_list() {
        let h = Harness::new();
        let r = h.shim(&["models", "--json"]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let body: Value = serde_json::from_str(&out_text(&r)).expect("stdout is JSON");
        assert_eq!(body, models());
    }
}

// ---------------------------------------------------------------------------
// Transport, token and legacy verbs
// ---------------------------------------------------------------------------

mod transport {
    use super::*;

    /// A socket path that does not exist, so a test can prove the Unix socket
    /// was not the transport that was used.
    const NO_SOCKET: &str = "/nonexistent/kea.sock";

    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind a port")
            .local_addr()
            .expect("local address")
            .port()
    }

    #[test]
    fn unix_socket_is_used_when_no_url_is_set() {
        let h = Harness::new();
        let r = h.shim_env(&["models"], &[("KEA_API_URL", None)]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let req = h.last();
        assert_eq!(req["transport"], "unix");
        assert_eq!(req["authorization"], bearer());
    }

    #[test]
    fn api_url_flag_before_the_command_selects_tcp() {
        let h = Harness::new();
        let base = h.api.base_url();
        let r = h.shim_env(
            &["--api-url", &base, "models"],
            &[("KEA_API_URL", None), ("KEA_API_SOCKET", Some(NO_SOCKET))],
        );
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(h.last()["transport"], "tcp");
    }

    #[test]
    fn api_url_flag_after_the_command_selects_tcp() {
        let h = Harness::new();
        let base = h.api.base_url();
        let r = h.shim_env(
            &["models", "--api-url", &base],
            &[("KEA_API_URL", None), ("KEA_API_SOCKET", Some(NO_SOCKET))],
        );
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        assert_eq!(h.last()["transport"], "tcp");
    }

    #[test]
    fn api_off_exits_69_with_the_general_settings_hint() {
        let h = Harness::new();
        let r = h.shim_env(
            &["models"],
            &[("KEA_API_URL", None), ("KEA_API_SOCKET", Some(NO_SOCKET))],
        );
        assert_eq!(code(&r), 69);
        assert!(err_text(&r).contains("General"));
    }

    #[test]
    fn unreadable_token_exits_77() {
        let h = Harness::new();
        let r = h.shim_env(
            &["models"],
            &[("KEA_API_TOKEN_FILE", Some("/nonexistent/api-token"))],
        );
        assert_eq!(code(&r), 77);
        assert!(err_text(&r).contains("cannot read"));
    }

    #[test]
    fn refused_tcp_port_exits_69() {
        let h = Harness::new();
        let url = format!("http://127.0.0.1:{}", closed_port());
        let r = h.shim_env(&["models"], &[("KEA_API_URL", Some(&url))]);
        assert_eq!(code(&r), 69);
        assert!(err_text(&r).contains("nothing is listening"));
    }

    #[test]
    fn wrong_token_is_a_401_api_error() {
        let h = Harness::new();
        let wrong = h.work.join("wrong-token");
        std::fs::write(&wrong, "not-the-token\n").expect("write the wrong token");
        let wrong_str = wrong.to_str().expect("UTF-8 path");
        let r = h.shim_env(&["models"], &[("KEA_API_TOKEN_FILE", Some(wrong_str))]);
        assert_eq!(code(&r), 1);
        assert!(err_text(&r).contains("kea: Invalid or missing token"));
        assert_eq!(h.last()["authorization"], "Bearer not-the-token");
    }

    #[test]
    fn older_verbs_keep_their_error_contract() {
        // `kea status` predates the model commands: an API error still prints
        // the body on stdout and exits 22, as it did with --fail-with-body.
        let h = Harness::new();
        let wrong = h.work.join("wrong-token-legacy");
        std::fs::write(&wrong, "not-the-token\n").expect("write the wrong token");
        let wrong_str = wrong.to_str().expect("UTF-8 path");
        let r = h.shim_env(&["status"], &[("KEA_API_TOKEN_FILE", Some(wrong_str))]);
        assert_eq!(code(&r), 22);
        assert!(out_text(&r).contains("error"));
    }

    #[test]
    fn status_still_works_over_the_socket() {
        let h = Harness::new();
        let r = h.shim_env(&["status"], &[("KEA_API_URL", None)]);
        assert_eq!(code(&r), 0, "{}", err_text(&r));
        let body: Value = serde_json::from_str(&out_text(&r)).expect("stdout is JSON");
        assert_eq!(body, json!({"version": "test"}));
    }
}

// ---------------------------------------------------------------------------
// JSON string escaping
// ---------------------------------------------------------------------------

mod json_escape {
    use super::*;

    const CASES: &[&str] = &[
        "",
        "plain",
        "quote \" here",
        "back\\slash",
        "tab\tnewline\ncr\rend",
        "\x01\x1f\x08\x0c",
        "trailing newline\n",
        "\n\n",
        "é 中 \u{1F600}",
        "\\\"",
        "ends with a backslash \\",
        "{braces} and [brackets]",
    ];

    /// json_string is tested directly: the shim's definitions are loaded on
    /// their own, and the output is parsed by serde_json.
    #[test]
    fn json_string_round_trips_through_a_json_parser() {
        let root = tempfile::tempdir().expect("create a temp dir");
        let prelude = root.path().join("prelude.sh");
        let text = std::fs::read_to_string(SHIM).expect("read the shim");
        assert!(
            text.contains(SHIM_PRELUDE_END),
            "the shim no longer has the prelude marker used by the tests"
        );
        std::fs::write(
            &prelude,
            text.split(SHIM_PRELUDE_END).next().expect("prelude"),
        )
        .expect("write the prelude");

        for value in CASES {
            let out = Command::new("sh")
                .args(["-c", r#". "$1"; json_string "$2""#, "sh"])
                .arg(&prelude)
                .arg(value)
                .output()
                .expect("run json_string");
            assert_eq!(out.status.code(), Some(0), "{value:?}: {}", err_text(&out));
            let parsed: Value = serde_json::from_slice(&out.stdout)
                .unwrap_or_else(|e| panic!("{value:?} did not parse: {e}"));
            assert_eq!(parsed, Value::String((*value).to_string()), "{value:?}");
        }
    }
}
