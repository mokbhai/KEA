//! Anthropic's native `/v1/messages` API.
//!
//! A separate engine rather than another base URL on the OpenAI-compatible
//! one, because every part of the wire format differs:
//!
//! * the key rides in `x-api-key`, not `Authorization: Bearer`;
//! * `anthropic-version` is required on every request and a call without it
//!   is rejected outright;
//! * the path is `/v1/messages`, not `/chat/completions`;
//! * `max_tokens` is required — there is no "until it stops" default;
//! * the answer is `content: [{type, text}, …]`, not
//!   `choices[0].message.content`;
//! * usage is reported as `input_tokens` / `output_tokens`.
//!
//! Anthropic does publish an OpenAI-compatible shim, and pointing the
//! existing compatible engine at it would have been one catalog line. It is
//! not used here: the shim silently drops parameters it has no equivalent
//! for, so a rewrite that looked configured would quietly run unconfigured.

use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient};
use crate::llm::stream::{self, LlmStream, StreamingLlmEngine};
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource};
use crate::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, TokenUsage};

pub const ANTHROPIC_LLM_ENGINE_ID: &str = "anthropic";

/// The branded endpoint. `/v1` is part of it, the way `OPENAI_BASE_URL`
/// carries its own, so a user pointing this at a gateway supplies the whole
/// prefix and nothing here has to guess where the version segment goes.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The API version this engine speaks. Anthropic versions its wire format by
/// date and rejects a request that names none; pinning it here means a new
/// version never changes the shape of a response mid-flight.
const ANTHROPIC_VERSION: &str = "2023-06-01";

const DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// The ceiling `max_tokens` takes when the caller has no opinion.
///
/// Required by the API, so there is no way to omit it. This is a rewrite
/// engine — the inputs are a dictated sentence or a selected paragraph — so
/// the cap exists to bound a runaway generation, not to shape the answer.
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct AnthropicLlmEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

impl AnthropicLlmEngine {
    /// Resolves the provider and builds the request every call shares.
    ///
    /// Factored out because `complete` and `stream` differ by exactly one
    /// body field, and the auth header, the version header and the endpoint
    /// path are the parts that are easy to get subtly wrong in only one of
    /// them.
    async fn prepare(&self, req: &LlmRequest) -> Result<Prepared, EngineError> {
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            req.provider_ref.as_deref(),
            &self.provider_ref,
            Some(Defaults {
                base_url: ANTHROPIC_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await?;
        // api.anthropic.com cannot serve an unauthenticated call, so say so
        // before the round-trip rather than relaying its 401.
        let api_key = provider.require_key()?.to_string();
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| provider.default_model.clone());
        Ok(Prepared {
            url: format!("{}/messages", provider.base_url.trim_end_matches('/')),
            api_key,
            body: serde_json::json!({
                "model": model,
                "max_tokens": DEFAULT_MAX_TOKENS,
                "messages": [{"role": "user", "content": req.prompt}],
            }),
        })
    }
}

struct Prepared {
    url: String,
    api_key: String,
    body: serde_json::Value,
}

impl Prepared {
    /// The two headers Anthropic requires. `x-api-key` rather than the
    /// bearer every other engine here uses, which is the whole reason the
    /// HTTP port grew a headers parameter.
    fn headers(&self) -> [(&str, &str); 2] {
        [
            ("x-api-key", self.api_key.as_str()),
            ("anthropic-version", ANTHROPIC_VERSION),
        ]
    }
}

/// Pulls the text out of a `/v1/messages` response.
///
/// The content array can hold blocks that are not text — a tool use, a
/// thinking block — so the text blocks are concatenated rather than
/// `content[0]` being taken on faith. An answer with no text block at all is
/// an error: an empty string would look like a rewrite that deleted the
/// user's sentence.
fn text_from_content(parsed: &serde_json::Value) -> Result<String, EngineError> {
    let blocks = parsed["content"]
        .as_array()
        .ok_or_else(|| EngineError::Other("missing content".into()))?;
    let text: String = blocks
        .iter()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    if text.is_empty() {
        return Err(EngineError::Other("no text in response".into()));
    }
    Ok(text)
}

/// Anthropic counts in `input_tokens`/`output_tokens`, not OpenAI's
/// `prompt_tokens`/`completion_tokens`. `None` when either is absent rather
/// than a zero, for the same reason the OpenAI reader refuses to guess.
pub(crate) fn anthropic_usage(usage: &serde_json::Value) -> Option<TokenUsage> {
    let prompt = usage.get("input_tokens")?.as_u64()?;
    let completion = usage.get("output_tokens")?.as_u64()?;
    Some(TokenUsage {
        prompt: prompt.min(u32::MAX as u64) as u32,
        completion: completion.min(u32::MAX as u64) as u32,
    })
}

#[async_trait]
impl LlmEngine for AnthropicLlmEngine {
    fn id(&self) -> &str {
        ANTHROPIC_LLM_ENGINE_ID
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec![
                "claude-sonnet-4-5".into(),
                "claude-opus-4-5".into(),
                "claude-haiku-4-5".into(),
            ],
        }
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        let prepared = self.prepare(&req).await?;
        let text = self
            .http
            .post_json_with_headers(
                &prepared.url,
                // The credential is a header here, so the bearer slot is
                // genuinely empty rather than merely unset.
                Auth::None,
                &prepared.headers(),
                prepared.body.clone(),
            )
            .await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
        Ok(LlmResponse {
            text: text_from_content(&parsed)?,
            usage: anthropic_usage(&parsed["usage"]),
        })
    }
}

#[async_trait]
impl StreamingLlmEngine for AnthropicLlmEngine {
    fn id(&self) -> &str {
        ANTHROPIC_LLM_ENGINE_ID
    }

    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, EngineError> {
        let prepared = self.prepare(&req).await?;
        let mut body = prepared.body.clone();
        body["stream"] = serde_json::Value::Bool(true);
        let bytes = self
            .http
            .post_json_stream(&prepared.url, Auth::None, &prepared.headers(), body)
            .await?;
        Ok(stream::decode_sse(bytes, stream::anthropic_event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
    use crate::provider::ProviderConfig;
    use crate::traits::LlmRequest;
    use futures_util::StreamExt;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct MapCredentials(HashMap<String, String>);

    #[async_trait]
    impl CredentialSource for MapCredentials {
        async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String> {
            Ok(self.0.get(provider_ref).cloned())
        }
    }

    struct MapConfigs(Mutex<HashMap<String, ProviderConfig>>);

    #[async_trait]
    impl ProviderConfigSource for MapConfigs {
        async fn config(&self, provider_ref: &str) -> Option<ProviderConfig> {
            self.0.lock().unwrap().get(provider_ref).cloned()
        }
    }

    fn engine(server_uri: &str, key: Option<&str>) -> AnthropicLlmEngine {
        let creds = match key {
            Some(key) => MapCredentials(HashMap::from([("anthropic".into(), key.to_string())])),
            None => MapCredentials(HashMap::new()),
        };
        AnthropicLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: Arc::new(creds),
            configs: Arc::new(MapConfigs(Mutex::new(HashMap::from([(
                "anthropic".to_string(),
                ProviderConfig {
                    base_url: format!("{server_uri}/v1"),
                    default_model: "claude-sonnet-4-5".into(),
                },
            )])))),
            provider_ref: "anthropic".into(),
        }
    }

    fn request(prompt: &str) -> LlmRequest {
        LlmRequest {
            prompt: prompt.into(),
            model: None,
            provider_ref: None,
        }
    }

    /// The whole reason this engine exists: the path, the auth header, the
    /// version header and the required `max_tokens` are all things the
    /// OpenAI-compatible engine would get wrong.
    #[tokio::test]
    async fn posts_the_native_messages_shape() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(body_string_contains("max_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"content":[{"type":"text","text":"polished"}],
                    "usage":{"input_tokens":11,"output_tokens":3}}"#,
            ))
            .mount(&server)
            .await;

        let out = engine(&server.uri(), Some("sk-ant-test"))
            .complete(request("rewrite me"))
            .await
            .unwrap();
        assert_eq!(out.text, "polished");
        assert_eq!(
            out.usage,
            Some(TokenUsage {
                prompt: 11,
                completion: 3
            })
        );
    }

    /// A bearer would not authenticate here, so a missing key has to fail
    /// before the round-trip rather than as somebody else's 401.
    #[tokio::test]
    async fn a_missing_key_fails_before_the_request() {
        let server = MockServer::start().await;
        let err = engine(&server.uri(), None)
            .complete(request("rewrite me"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing api key"), "{err}");
        // Nothing was mounted, so a request that went out would have 404'd
        // with a different message than this one.
    }

    /// Content is an array of blocks, only some of which are text. Taking
    /// `content[0]` on faith returns a tool-use block's absent `text` as an
    /// error, or worse, an empty rewrite.
    #[tokio::test]
    async fn text_blocks_are_joined_and_non_text_blocks_ignored() {
        let parsed: serde_json::Value = serde_json::from_str(
            r#"{"content":[{"type":"thinking","thinking":"hmm"},
                           {"type":"text","text":"one "},
                           {"type":"text","text":"two"}]}"#,
        )
        .unwrap();
        assert_eq!(text_from_content(&parsed).unwrap(), "one two");

        let empty: serde_json::Value =
            serde_json::from_str(r#"{"content":[{"type":"tool_use"}]}"#).unwrap();
        assert!(text_from_content(&empty).is_err());
    }

    /// Anthropic names its counters differently from OpenAI; reading the
    /// OpenAI names here would report every call as untracked.
    #[test]
    fn usage_is_read_from_anthropics_own_field_names() {
        let usage: serde_json::Value =
            serde_json::from_str(r#"{"input_tokens":7,"output_tokens":9}"#).unwrap();
        assert_eq!(
            anthropic_usage(&usage),
            Some(TokenUsage {
                prompt: 7,
                completion: 9
            })
        );
        let openai_names: serde_json::Value =
            serde_json::from_str(r#"{"prompt_tokens":7,"completion_tokens":9}"#).unwrap();
        assert_eq!(anthropic_usage(&openai_names), None);
    }

    /// The streaming path is the same request with one field added, decoded
    /// through Anthropic's own event names.
    #[tokio::test]
    async fn streams_content_block_deltas() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_string_contains("\"stream\":true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
                "event: content_block_delta\n",
                "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"pol\"}}\n\n",
                "event: content_block_delta\n",
                "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"ished\"}}\n\n",
                "event: message_stop\n",
                "data: {}\n\n",
            )))
            .mount(&server)
            .await;

        let mut chunks = engine(&server.uri(), Some("sk-ant-test"))
            .stream(request("rewrite me"))
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(chunk) = chunks.next().await {
            text.push_str(&chunk.unwrap());
        }
        assert_eq!(text, "polished");
    }
}
