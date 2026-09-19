use async_trait::async_trait;

use crate::traits::EngineError;

pub struct MultipartPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// How one request authenticates.
///
/// `None` is a real, supported case, not an error: an OpenAI-compatible
/// server running on the user's own machine (Ollama, LM Studio, llama.cpp)
/// takes no credential, and the app's provider UI already labels it "No key
/// needed". Demanding a bearer at this port is what made that setup fail with
/// "missing api key" for a key the app said was not required.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Auth<'a> {
    #[default]
    None,
    Bearer(&'a str),
}

impl<'a> Auth<'a> {
    /// `Some(key)` becomes a bearer, `None` an unauthenticated request.
    pub fn from_optional_key(key: Option<&'a str>) -> Self {
        match key {
            Some(key) => Auth::Bearer(key),
            None => Auth::None,
        }
    }
}

/// The one place a response status becomes an [`EngineError`].
///
/// Every caller used to invent its own rule — `!(200..300)` here, `status !=
/// 200` there — so a 202 from a compatible transcription endpoint was an
/// error in one engine and a success in another, and a server 401 (the most
/// common wrong-key case) surfaced as a bare `HTTP 401` instead of the
/// [`EngineError::Auth`] message that tells the user to fix their key.
fn map_status(status: u16, body: &str) -> Result<(), EngineError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(match status {
        401 | 403 => EngineError::auth_rejected(status, body.to_string()),
        429 | 500..=599 => EngineError::retryable(status, body.to_string()),
        _ => EngineError::http(status, body.to_string()),
    })
}

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        auth: Auth<'_>,
        body: serde_json::Value,
    ) -> Result<String, EngineError>;

    async fn post_multipart(
        &self,
        url: &str,
        auth: Auth<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError>;

    /// POST JSON body; response body is raw bytes (e.g. TTS audio).
    async fn post_binary(
        &self,
        url: &str,
        auth: Auth<'_>,
        body: serde_json::Value,
    ) -> Result<Vec<u8>, EngineError>;
}

pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// The single place a credential is attached to a request.
    fn authenticate(req: reqwest::RequestBuilder, auth: Auth<'_>) -> reqwest::RequestBuilder {
        match auth {
            Auth::None => req,
            Auth::Bearer(key) => req.bearer_auth(key),
        }
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn post_json(
        &self,
        url: &str,
        auth: Auth<'_>,
        body: serde_json::Value,
    ) -> Result<String, EngineError> {
        let resp = Self::authenticate(self.client.post(url), auth)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        map_status(status, &text)?;
        Ok(text)
    }

    async fn post_multipart(
        &self,
        url: &str,
        auth: Auth<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError> {
        let mut form = reqwest::multipart::Form::new();
        for part in parts {
            let mut builder = reqwest::multipart::Part::bytes(part.data);
            if let Some(filename) = part.filename {
                builder = builder.file_name(filename);
            }
            if let Some(content_type) = part.content_type {
                builder = builder
                    .mime_str(&content_type)
                    .map_err(|e| EngineError::Other(e.to_string()))?;
            }
            form = form.part(part.name, builder);
        }
        let resp = Self::authenticate(self.client.post(url), auth)
            .multipart(form)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        map_status(status, &text)?;
        Ok(text)
    }

    async fn post_binary(
        &self,
        url: &str,
        auth: Auth<'_>,
        body: serde_json::Value,
    ) -> Result<Vec<u8>, EngineError> {
        let resp = Self::authenticate(self.client.post(url), auth)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        // An error body is text even when the success body is audio.
        map_status(status, &String::from_utf8_lossy(&bytes))?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn posts_json_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"ok"}}]}"#),
            )
            .mount(&server)
            .await;

        let http = ReqwestHttpClient::new();
        let body = http
            .post_json(
                &format!("{}/v1/chat/completions", server.uri()),
                Auth::Bearer("sk-test"),
                serde_json::json!({"model": "gpt-4o-mini", "messages": []}),
            )
            .await
            .unwrap();
        assert!(body.contains("ok"));
    }

    /// A keyless local server must be reachable without an Authorization
    /// header at all — sending an empty bearer is not the same thing.
    #[tokio::test]
    async fn auth_none_sends_no_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500).set_body_string("unexpected auth"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"local"}}]}"#),
            )
            .mount(&server)
            .await;

        let http = ReqwestHttpClient::new();
        let body = http
            .post_json(
                &format!("{}/v1/chat/completions", server.uri()),
                Auth::None,
                serde_json::json!({"model": "llama3", "messages": []}),
            )
            .await
            .unwrap();
        assert!(body.contains("local"));
    }

    #[tokio::test]
    async fn posts_multipart_audio_transcription() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"hello"}"#))
            .mount(&server)
            .await;

        let http = ReqwestHttpClient::new();
        let body = http
            .post_multipart(
                &format!("{}/v1/audio/transcriptions", server.uri()),
                Auth::Bearer("sk-test"),
                vec![
                    MultipartPart {
                        name: "file".into(),
                        filename: Some("audio.wav".into()),
                        content_type: Some("audio/wav".into()),
                        data: vec![0x52, 0x49, 0x46, 0x46],
                    },
                    MultipartPart {
                        name: "model".into(),
                        filename: None,
                        content_type: None,
                        data: b"whisper-1".to_vec(),
                    },
                ],
            )
            .await
            .unwrap();
        assert!(body.contains("hello"));
    }

    #[tokio::test]
    async fn post_binary_returns_binary_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(b"FAKEAUDIO"),
            )
            .mount(&server)
            .await;

        let client = ReqwestHttpClient::new();
        let bytes = client
            .post_binary(
                &format!("{}/v1/audio/speech", server.uri()),
                Auth::Bearer("sk-test"),
                serde_json::json!({
                    "model": "tts-1",
                    "input": "hi",
                    "voice": "alloy"
                }),
            )
            .await
            .unwrap();
        assert_eq!(bytes, b"FAKEAUDIO");
    }

    /// The point of finding 6: a server 401 is a wrong key, and the user is
    /// told so instead of being shown a bare status line.
    #[test]
    fn server_rejection_becomes_an_auth_error() {
        for status in [401, 403] {
            let err = map_status(status, r#"{"error":"invalid_api_key"}"#).unwrap_err();
            match &err {
                EngineError::Auth(msg) => assert!(msg.contains(&status.to_string())),
                other => panic!("expected EngineError::Auth, got {other:?}"),
            }
            assert!(err
                .to_string()
                .contains("check the API key in provider settings"));
        }
    }

    #[test]
    fn rate_limit_and_server_faults_are_retryable() {
        for status in [429, 500, 503] {
            match map_status(status, "busy").unwrap_err() {
                EngineError::Retryable { status: got, .. } => assert_eq!(got, status),
                other => panic!("expected EngineError::Retryable, got {other:?}"),
            }
        }
    }

    #[test]
    fn other_failures_keep_their_status() {
        match map_status(404, "no such model").unwrap_err() {
            EngineError::Http { status, body } => {
                assert_eq!(status, 404);
                assert!(body.contains("no such model"));
            }
            other => panic!("expected EngineError::Http, got {other:?}"),
        }
    }

    /// A compatible transcription endpoint may answer 202; the old
    /// `status != 200` rule in `stt/openai.rs` turned that into an error.
    #[test]
    fn every_2xx_is_a_success() {
        for status in [200, 201, 202, 204, 299] {
            assert!(map_status(status, "").is_ok());
        }
    }
}
