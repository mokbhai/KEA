//! Deepgram's `/v1/listen`.
//!
//! Three things make this a bespoke engine rather than a base-URL row beside
//! Groq's:
//!
//! * the credential is `Authorization: Token <key>`, not a bearer;
//! * the audio is the request *body*, not a multipart field and not JSON —
//!   which is the only reason [`crate::http::HttpClient::post_bytes`] exists;
//! * options (model, punctuation, keyterms) are query parameters, and the
//!   transcript comes back at `results.channels[0].alternatives[0]`.
//!
//! What it buys: Nova-3 is a different accuracy/latency point from
//! whisper-large-v3 and it reports word-level timing, which the subtitle
//! writers use directly.

use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient};
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource};
use crate::stt::audio::pcm_to_wav_bytes;
use crate::traits::{
    AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, SttSegment, Transcript,
};

pub const DEEPGRAM_STT_ENGINE_ID: &str = "deepgram-stt";

pub const DEEPGRAM_BASE_URL: &str = "https://api.deepgram.com/v1";

const DEFAULT_MODEL: &str = "nova-3";

pub struct DeepgramSttEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

/// Builds the `/listen` URL with this call's options in the query string.
///
/// Pure, so the parameter assembly — the part that silently stops being
/// honoured — is testable without a server. `smart_format` is on
/// unconditionally: it is what supplies punctuation and capitalisation, and
/// dictated text without either is not usable output.
fn listen_url(base_url: &str, model: &str, language: Option<&str>, keyterms: &[String]) -> String {
    let mut url = format!(
        "{}/listen?model={}&smart_format=true",
        base_url.trim_end_matches('/'),
        encode(model)
    );
    // Deepgram's default is English; naming a language it does not have for
    // this model is an error from the far end, so it is only sent when the
    // caller actually chose one.
    if let Some(language) = language.map(str::trim).filter(|l| !l.is_empty()) {
        url.push_str(&format!("&language={}", encode(language)));
    }
    // Nova-3's term boosting. Repeated, one parameter per term, which is the
    // shape the API takes — a comma-joined list arrives as one long phrase
    // that matches nothing.
    for term in keyterms {
        url.push_str(&format!("&keyterm={}", encode(term)));
    }
    url
}

/// Minimal percent-encoding for a query value.
///
/// Hand-rolled rather than pulling in a URL crate for five characters: model
/// ids and language tags are alphanumeric, and the one input that is not is
/// the user's vocabulary, which routinely contains spaces and ampersands.
/// Everything outside the unreserved set is escaped, so nothing can inject a
/// second parameter.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Reads the first alternative of the first channel.
///
/// Deepgram always answers with that shape for a single-channel upload; a
/// body that does not have it is a gateway or an error page, and the caller
/// gets "no transcript" rather than an empty string that looks like silence.
fn read_response(parsed: &serde_json::Value) -> Result<Transcript, EngineError> {
    let alternative = &parsed["results"]["channels"][0]["alternatives"][0];
    let text = alternative["transcript"]
        .as_str()
        .ok_or_else(|| EngineError::Other("missing transcript".into()))?;
    Ok(Transcript {
        text: text.to_string(),
        segments: read_words(alternative),
    })
}

/// Groups Deepgram's per-word timing into one segment per sentence.
///
/// One cue per word is not a subtitle, which is the same conclusion
/// `group_tokens_into_segments` reached for sherpa. Deepgram makes the
/// grouping easy: with `smart_format` on, each word carries a
/// `punctuated_word`, so a sentence ends where one of those ends in terminal
/// punctuation. A response with no words at all leaves `segments` empty
/// rather than inventing one span for the whole buffer.
fn read_words(alternative: &serde_json::Value) -> Vec<SttSegment> {
    let Some(words) = alternative["words"].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<SttSegment> = Vec::new();
    let mut current: Option<(u64, u64, String)> = None;
    for word in words {
        let text = word["punctuated_word"]
            .as_str()
            .or_else(|| word["word"].as_str())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            continue;
        }
        let start = seconds_to_ms(&word["start"]);
        let end = seconds_to_ms(&word["end"]).max(start);
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
    // A trailing clause with no terminal punctuation is still a cue.
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
impl SttEngine for DeepgramSttEngine {
    fn id(&self) -> &str {
        DEEPGRAM_STT_ENGINE_ID
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["nova-3".into(), "nova-2".into()],
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            opts.provider_ref.as_deref(),
            &self.provider_ref,
            Some(Defaults {
                base_url: DEEPGRAM_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await?;
        // Nothing here can serve an unauthenticated call, so fail before
        // uploading the audio.
        let api_key = provider.require_key()?;
        let model = opts.model.as_deref().unwrap_or(&provider.default_model);
        let url = listen_url(
            &provider.base_url,
            model,
            opts.language.as_deref(),
            &opts.vocabulary,
        );
        let wav = pcm_to_wav_bytes(&audio)?;
        // `Token`, not `Bearer` — Deepgram rejects a bearer outright, which
        // is why the scheme is spelled out in a header rather than left to
        // `Auth`.
        let authorization = format!("Token {api_key}");
        let text = self
            .http
            .post_bytes(
                &url,
                Auth::None,
                &[("authorization", authorization.as_str())],
                "audio/wav",
                wav,
            )
            .await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
        read_response(&parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
    use crate::provider::ProviderConfig;
    use std::collections::HashMap;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn engine(uri: &str, key: Option<&str>) -> DeepgramSttEngine {
        DeepgramSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: Arc::new(MapCredentials(match key {
                Some(key) => HashMap::from([("deepgram".to_string(), key.to_string())]),
                None => HashMap::new(),
            })),
            configs: Arc::new(MapConfigs(HashMap::from([(
                "deepgram".to_string(),
                ProviderConfig {
                    base_url: format!("{uri}/v1"),
                    default_model: "nova-3".into(),
                },
            )]))),
            provider_ref: "deepgram".into(),
        }
    }

    fn one_second() -> AudioPcm {
        AudioPcm {
            samples: vec![0.0; 16_000],
            sample_rate_hz: 16_000,
        }
    }

    const BODY: &str = r#"{"results":{"channels":[{"alternatives":[{
        "transcript":"Hello there. How are you",
        "words":[
          {"word":"hello","punctuated_word":"Hello","start":0.1,"end":0.4},
          {"word":"there","punctuated_word":"there.","start":0.4,"end":0.8},
          {"word":"how","punctuated_word":"How","start":1.0,"end":1.2},
          {"word":"are","punctuated_word":"are","start":1.2,"end":1.4},
          {"word":"you","punctuated_word":"you","start":1.4,"end":1.6}
        ]}]}]}}"#;

    #[tokio::test]
    async fn posts_the_audio_as_the_body_with_a_token_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/listen"))
            .and(query_param("model", "nova-3"))
            .and(query_param("smart_format", "true"))
            .and(header("authorization", "Token dg-test"))
            .and(header("content-type", "audio/wav"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
            .mount(&server)
            .await;

        let out = engine(&server.uri(), Some("dg-test"))
            .transcribe(one_second(), SttOpts::default())
            .await
            .unwrap();
        assert_eq!(out.text, "Hello there. How are you");
    }

    /// One cue per word is not a subtitle; sentences are.
    #[test]
    fn words_group_into_sentences() {
        let parsed: serde_json::Value = serde_json::from_str(BODY).unwrap();
        let transcript = read_response(&parsed).unwrap();
        let segments = transcript.segments;
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "Hello there.");
        assert_eq!(segments[0].start_ms, 100);
        assert_eq!(segments[0].end_ms, 800);
        // The trailing clause has no terminal punctuation and is still a cue.
        assert_eq!(segments[1].text, "How are you");
        assert_eq!(segments[1].end_ms, 1_600);
    }

    /// A response with no word array is a transcript with no timing, not an
    /// error and not one span covering the whole buffer.
    #[test]
    fn a_response_without_words_has_no_segments() {
        let parsed: serde_json::Value = serde_json::from_str(
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"hi"}]}]}}"#,
        )
        .unwrap();
        let transcript = read_response(&parsed).unwrap();
        assert_eq!(transcript.text, "hi");
        assert!(transcript.segments.is_empty());
    }

    /// An error page or a gateway reply must not read as silence.
    #[test]
    fn a_body_that_is_not_a_transcript_is_an_error() {
        let parsed: serde_json::Value = serde_json::from_str(r#"{"err_msg":"nope"}"#).unwrap();
        assert!(read_response(&parsed).is_err());
    }

    /// Vocabulary terms become one `keyterm` each, and anything that could
    /// forge a second query parameter is escaped.
    #[test]
    fn keyterms_are_one_parameter_each_and_escaped() {
        let url = listen_url(
            "https://api.deepgram.com/v1",
            "nova-3",
            Some("en-GB"),
            &["KittyClaw".to_string(), "R&D team".to_string()],
        );
        assert!(url.contains("&keyterm=KittyClaw"), "{url}");
        assert!(url.contains("&keyterm=R%26D%20team"), "{url}");
        assert!(url.contains("&language=en-GB"), "{url}");
        // Exactly the parameters asked for: the escaped ampersand did not
        // become a separator.
        assert_eq!(url.matches('&').count(), 4);
    }

    /// No language chosen means the provider's default, not an empty
    /// parameter it has to reject.
    #[test]
    fn an_absent_language_is_simply_not_sent() {
        let url = listen_url("https://api.deepgram.com/v1", "nova-3", Some("  "), &[]);
        assert!(!url.contains("language="), "{url}");
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
