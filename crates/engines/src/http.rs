use async_trait::async_trait;

use crate::traits::EngineError;

pub struct MultipartPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: serde_json::Value,
    ) -> Result<(u16, String), EngineError>;

    async fn post_multipart(
        &self,
        url: &str,
        bearer: &str,
        parts: Vec<MultipartPart>,
    ) -> Result<(u16, String), EngineError>;

    /// POST JSON body; response body is raw bytes (e.g. TTS audio).
    async fn post_binary(
        &self,
        url: &str,
        bearer: &str,
        body: serde_json::Value,
    ) -> Result<(u16, Vec<u8>), EngineError>;
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
        bearer: &str,
        body: serde_json::Value,
    ) -> Result<(u16, String), EngineError> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        Ok((status, text))
    }

    async fn post_multipart(
        &self,
        url: &str,
        bearer: &str,
        parts: Vec<MultipartPart>,
    ) -> Result<(u16, String), EngineError> {
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
        let resp = self
            .client
            .post(url)
            .bearer_auth(bearer)
            .multipart(form)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        Ok((status, text))
    }

    async fn post_binary(
        &self,
        url: &str,
        bearer: &str,
        body: serde_json::Value,
    ) -> Result<(u16, Vec<u8>), EngineError> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        Ok((status, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn posts_json_and_returns_status_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"ok"}}]}"#),
            )
            .mount(&server)
            .await;

        let http = ReqwestHttpClient::new();
        let (status, body) = http
            .post_json(
                &format!("{}/v1/chat/completions", server.uri()),
                "sk-test",
                serde_json::json!({"model": "gpt-4o-mini", "messages": []}),
            )
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));
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
        let (status, body) = http
            .post_multipart(
                &format!("{}/v1/audio/transcriptions", server.uri()),
                "sk-test",
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
        assert_eq!(status, 200);
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
        let (status, bytes) = client
            .post_binary(
                &format!("{}/v1/audio/speech", server.uri()),
                "sk-test",
                serde_json::json!({
                    "model": "tts-1",
                    "input": "hi",
                    "voice": "alloy"
                }),
            )
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(bytes, b"FAKEAUDIO");
    }
}
