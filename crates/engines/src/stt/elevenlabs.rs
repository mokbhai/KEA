//! ElevenLabs Scribe (`/v1/speech-to-text`).
//!
//! Closer to the OpenAI engine than Deepgram is — it is a multipart upload —
//! but two details keep it from being a base-URL row: the credential is an
//! `xi-api-key` header rather than a bearer, and the response is flat
//! (`text` plus a `words` array with `start`/`end` in seconds and a `type`
//! that distinguishes words from the spacing between them).

use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient, MultipartPart};
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource};
use crate::stt::audio::pcm_to_wav_bytes;
use crate::traits::{
    AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, SttSegment, Transcript,
};

pub const ELEVENLABS_STT_ENGINE_ID: &str = "elevenlabs-stt";

pub const ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io/v1";

const DEFAULT_MODEL: &str = "scribe_v1";

pub struct ElevenLabsSttEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

fn field(name: &str, value: &str) -> MultipartPart {
    MultipartPart {
        name: name.to_string(),
        filename: None,
        content_type: None,
        data: value.as_bytes().to_vec(),
    }
}

/// Groups Scribe's per-token timing into sentence cues.
///
/// The `words` array interleaves `type: "word"` entries with `type:
/// "spacing"` ones carrying the literal whitespace; joining everything
/// blindly produces doubled spaces, and skipping the spacing entries loses
/// nothing because the words are re-joined with a single space anyway. An
/// entry with an unrecognised type is skipped rather than guessed at.
fn read_words(parsed: &serde_json::Value) -> Vec<SttSegment> {
    let Some(items) = parsed["words"].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<SttSegment> = Vec::new();
    let mut current: Option<(u64, u64, String)> = None;
    for item in items {
        if item["type"].as_str().unwrap_or("word") != "word" {
            continue;
        }
        let text = item["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let start = seconds_to_ms(&item["start"]);
        let end = seconds_to_ms(&item["end"]).max(start);
        match current.as_mut() {
            Some((_, cur_end, buffer)) => {
                buffer.push(' ');
                buffer.push_str(text);
                *cur_end = end.max(*cur_end);
            }
            None => current = Some((start, end, text.to_string())),
        }
        if text.ends_with(['.', '!', '?']) {
            let (start, end, buffer) = current.take().expect("just assigned");
            out.push(SttSegment::new(start, end, buffer));
        }
    }
    if let Some((start, end, buffer)) = current {
        out.push(SttSegment::new(start, end, buffer));
    }
    out
}

fn seconds_to_ms(value: &serde_json::Value) -> u64 {
    value
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| (v * 1000.0).round() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl SttEngine for ElevenLabsSttEngine {
    fn id(&self) -> &str {
        ELEVENLABS_STT_ENGINE_ID
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["scribe_v1".into()],
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            opts.provider_ref.as_deref(),
            &self.provider_ref,
            Some(Defaults {
                base_url: ELEVENLABS_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await?;
        let api_key = provider.require_key()?;
        let model = opts.model.as_deref().unwrap_or(&provider.default_model);
        let url = format!("{}/speech-to-text", provider.base_url.trim_end_matches('/'));
        let mut parts = vec![
            MultipartPart {
                name: "file".into(),
                filename: Some("audio.wav".into()),
                content_type: Some("audio/wav".into()),
                data: pcm_to_wav_bytes(&audio)?,
            },
            field("model_id", model),
        ];
        // Scribe detects the language itself; naming one is a narrowing the
        // caller asked for, so an absent setting stays absent rather than
        // becoming an empty field the API has to reject.
        if let Some(language) = opts
            .language
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            // A BCP-47 tag from our settings ("en-US"); the API takes the
            // ISO-639 part, and sending the region makes it 422.
            let code = language.split('-').next().unwrap_or(language);
            parts.push(field("language_code", code));
        }
        let text = self
            .http
            .post_multipart_with_headers(
                &url,
                // The credential is a header, so the bearer slot is genuinely
                // empty rather than merely unset.
                Auth::None,
                &[("xi-api-key", api_key)],
                parts,
            )
            .await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
        let content = parsed["text"]
            .as_str()
            .ok_or_else(|| EngineError::Other("missing text field".into()))?;
        Ok(Transcript {
            text: content.to_string(),
            segments: read_words(&parsed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
    use crate::provider::ProviderConfig;
    use std::collections::HashMap;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    /// A multipart body holding a WAV file is not valid UTF-8, so wiremock's
    /// `body_string_contains` never matches it. These assertions are about
    /// the *form fields*, which are ASCII, so the search runs over raw bytes.
    struct BodyHasBytes(&'static str);

    impl Match for BodyHasBytes {
        fn matches(&self, request: &Request) -> bool {
            request
                .body
                .windows(self.0.len())
                .any(|window| window == self.0.as_bytes())
        }
    }

    struct MapCredentials(HashMap<String, String>);

    #[async_trait]
    impl CredentialSource for MapCredentials {
        async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String> {
            Ok(self.0.get(provider_ref).cloned())
        }
    }

    struct MapConfigs(HashMap<String, ProviderConfig>);

    #[async_trait]
    impl ProviderConfigSource for MapConfigs {
        async fn config(&self, provider_ref: &str) -> Option<ProviderConfig> {
            self.0.get(provider_ref).cloned()
        }
    }

    fn engine(uri: &str, key: Option<&str>) -> ElevenLabsSttEngine {
        ElevenLabsSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: Arc::new(MapCredentials(match key {
                Some(key) => HashMap::from([("elevenlabs".to_string(), key.to_string())]),
                None => HashMap::new(),
            })),
            configs: Arc::new(MapConfigs(HashMap::from([(
                "elevenlabs".to_string(),
                ProviderConfig {
                    base_url: format!("{uri}/v1"),
                    default_model: "scribe_v1".into(),
                },
            )]))),
            provider_ref: "elevenlabs".into(),
        }
    }

    fn one_second() -> AudioPcm {
        AudioPcm {
            samples: vec![0.0; 16_000],
            sample_rate_hz: 16_000,
        }
    }

    const BODY: &str = r#"{"language_code":"eng","text":"Hello there. How are you",
        "words":[
          {"text":"Hello","type":"word","start":0.1,"end":0.4},
          {"text":" ","type":"spacing","start":0.4,"end":0.4},
          {"text":"there.","type":"word","start":0.4,"end":0.8},
          {"text":"How","type":"word","start":1.0,"end":1.2},
          {"text":"are","type":"word","start":1.2,"end":1.4},
          {"text":"you","type":"word","start":1.4,"end":1.6}
        ]}"#;

    #[tokio::test]
    async fn uploads_multipart_with_the_xi_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/speech-to-text"))
            .and(header("xi-api-key", "el-test"))
            .and(BodyHasBytes("scribe_v1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
            .mount(&server)
            .await;

        let out = engine(&server.uri(), Some("el-test"))
            .transcribe(one_second(), SttOpts::default())
            .await
            .unwrap();
        assert_eq!(out.text, "Hello there. How are you");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.segments[0].text, "Hello there.");
        assert_eq!(out.segments[0].start_ms, 100);
        assert_eq!(out.segments[1].text, "How are you");
    }

    /// The API takes ISO-639; sending our stored BCP-47 tag verbatim is a 422.
    #[tokio::test]
    async fn a_region_tag_is_narrowed_to_the_language_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/speech-to-text"))
            .and(BodyHasBytes("language_code"))
            // The body carries "en", never "en-US".
            .and(BodyHasBytes("\r\n\r\nen\r\n"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"hi"}"#))
            .mount(&server)
            .await;

        let out = engine(&server.uri(), Some("el-test"))
            .transcribe(
                one_second(),
                SttOpts {
                    language: Some("en-US".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out.text, "hi");
    }

    /// Spacing entries carry literal whitespace and are not words; folding
    /// them in doubles every space in the cue.
    #[test]
    fn spacing_entries_are_not_words() {
        let parsed: serde_json::Value = serde_json::from_str(BODY).unwrap();
        let segments = read_words(&parsed);
        assert_eq!(segments[0].text, "Hello there.");
        assert!(!segments[0].text.contains("  "));
    }

    #[test]
    fn a_response_without_words_has_no_segments() {
        let parsed: serde_json::Value = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert!(read_words(&parsed).is_empty());
    }

    #[tokio::test]
    async fn a_missing_key_fails_before_uploading_audio() {
        let server = MockServer::start().await;
        let err = engine(&server.uri(), None)
            .transcribe(one_second(), SttOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing api key"), "{err}");
    }
}
