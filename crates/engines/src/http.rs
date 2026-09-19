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

/// Provider-specific request headers, beyond the credential.
///
/// A separate parameter rather than a new [`Auth`] variant: `Auth` is matched
/// exhaustively by fake clients in other crates, and a third variant would
/// break every one of them for the sake of one provider. Anthropic is that
/// provider — `/v1/messages` carries its key in `x-api-key` and *requires*
/// an `anthropic-version` on every call, neither of which a bearer can
/// express.
pub type HeaderPairs<'a> = &'a [(&'a str, &'a str)];

/// One streamed chunk of a response body.
///
/// Bytes rather than parsed events: the two wire formats that matter here
/// (OpenAI's `data:` SSE and Anthropic's named SSE) frame differently, and
/// the port has no business knowing which one it is carrying.
pub type ByteStream = std::pin::Pin<
    Box<dyn futures_util::Stream<Item = Result<Vec<u8>, EngineError>> + Send + 'static>,
>;

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        auth: Auth<'_>,
        body: serde_json::Value,
    ) -> Result<String, EngineError>;

    /// POST JSON with extra headers.
    ///
    /// Defaulted rather than required so the fake clients that already
    /// implement this port keep compiling. The default forwards a header-less
    /// call and *refuses* one with headers rather than dropping them
    /// silently: a client that cannot send `anthropic-version` cannot talk to
    /// Anthropic, and saying so is better than a 400 from the far end.
    async fn post_json_with_headers(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        body: serde_json::Value,
    ) -> Result<String, EngineError> {
        if headers.is_empty() {
            return self.post_json(url, auth, body).await;
        }
        Err(EngineError::Other(
            "this HTTP client cannot send provider headers".into(),
        ))
    }

    /// POST JSON and read the response body as it arrives.
    ///
    /// Defaulted to "the whole body, as one chunk", which is a correct
    /// stream and not a pretend one: a consumer that renders each chunk as it
    /// lands renders the finished answer once. That keeps the four engines
    /// already on this port untouched while [`ReqwestHttpClient`] does the
    /// real incremental read.
    async fn post_json_stream(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        body: serde_json::Value,
    ) -> Result<ByteStream, EngineError> {
        let whole = self
            .post_json_with_headers(url, auth, headers, body)
            .await?;
        Ok(Box::pin(futures_util::stream::once(async move {
            Ok(whole.into_bytes())
        })))
    }

    async fn post_multipart(
        &self,
        url: &str,
        auth: Auth<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError>;

    /// POST a multipart form with extra headers. See
    /// [`HttpClient::post_json_with_headers`] for why it is defaulted and why
    /// the default refuses rather than dropping the headers.
    async fn post_multipart_with_headers(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError> {
        if headers.is_empty() {
            return self.post_multipart(url, auth, parts).await;
        }
        Err(EngineError::Other(
            "this HTTP client cannot send provider headers".into(),
        ))
    }

    /// POST a raw body of `content_type`; response body is text.
    ///
    /// Deepgram takes the audio file as the request body itself — not JSON
    /// around it and not a multipart field — so none of the other three
    /// methods can express the call.
    async fn post_bytes(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<String, EngineError> {
        let _ = (url, auth, headers, content_type, body);
        Err(EngineError::Other(
            "this HTTP client cannot send a raw body".into(),
        ))
    }

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

    /// The single place a multipart body is assembled.
    fn build_form(parts: Vec<MultipartPart>) -> Result<reqwest::multipart::Form, EngineError> {
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
        Ok(form)
    }

    /// And the single place provider headers are.
    fn with_headers(
        mut req: reqwest::RequestBuilder,
        headers: HeaderPairs<'_>,
    ) -> reqwest::RequestBuilder {
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        req
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

    async fn post_json_with_headers(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        body: serde_json::Value,
    ) -> Result<String, EngineError> {
        let resp = Self::with_headers(Self::authenticate(self.client.post(url), auth), headers)
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

    async fn post_json_stream(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        body: serde_json::Value,
    ) -> Result<ByteStream, EngineError> {
        use futures_util::StreamExt;

        let resp = Self::with_headers(Self::authenticate(self.client.post(url), auth), headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        // A failed stream still has an error body, and it is short — read it
        // whole so the user gets the provider's own message rather than
        // "stream ended". `map_status` classifies it exactly as it does for
        // every other method here, and it always returns `Err` for a non-2xx,
        // so the `unwrap_or_else` can only produce a defensive fallback.
        if !(200..300).contains(&status) {
            let text = resp
                .text()
                .await
                .map_err(|e| EngineError::Other(e.to_string()))?;
            return Err(map_status(status, &text)
                .err()
                .unwrap_or_else(|| EngineError::http(status, text)));
        }
        Ok(Box::pin(resp.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|e| EngineError::Other(e.to_string()))
        })))
    }

    async fn post_multipart(
        &self,
        url: &str,
        auth: Auth<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError> {
        let form = Self::build_form(parts)?;
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

    async fn post_multipart_with_headers(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        parts: Vec<MultipartPart>,
    ) -> Result<String, EngineError> {
        let form = Self::build_form(parts)?;
        let resp = Self::with_headers(Self::authenticate(self.client.post(url), auth), headers)
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

    async fn post_bytes(
        &self,
        url: &str,
        auth: Auth<'_>,
        headers: HeaderPairs<'_>,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<String, EngineError> {
        let resp = Self::with_headers(Self::authenticate(self.client.post(url), auth), headers)
            .header("content-type", content_type)
            .body(body)
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
