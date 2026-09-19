pub mod anthropic;
pub mod compatible;
pub mod discovery;
pub mod openai;
pub mod stream;

pub use anthropic::{AnthropicLlmEngine, ANTHROPIC_BASE_URL, ANTHROPIC_LLM_ENGINE_ID};
pub use compatible::OpenAiCompatibleLlmEngine;
pub use discovery::{
    discover_local_llms, LocalLlmProbe, LocalLlmServer, ModelListShape, ProbeTransport,
    ReqwestProbe, LOCAL_LLM_PROBES,
};
pub use openai::OpenAiLlmEngine;
pub use stream::{LlmStream, StreamingLlmEngine};

use crate::http::{Auth, HttpClient};
use crate::traits::{EngineError, LlmResponse, TokenUsage};

/// Reads the `usage` block every OpenAI-shaped server *may* send.
///
/// `None` rather than zeros when it is absent: llama.cpp and some compatible
/// servers omit it entirely, and "0 tokens" in a cost view is a claim we
/// cannot make. Both counts must be present for the pair to mean anything.
pub(crate) fn openai_usage(parsed: &serde_json::Value) -> Option<TokenUsage> {
    let usage = parsed.get("usage")?;
    let prompt = usage.get("prompt_tokens")?.as_u64()?;
    let completion = usage.get("completion_tokens")?.as_u64()?;
    Some(TokenUsage {
        prompt: prompt.min(u32::MAX as u64) as u32,
        completion: completion.min(u32::MAX as u64) as u32,
    })
}

/// Posts one OpenAI-shaped chat completion with `stream: true` and decodes
/// the SSE frames into display text.
///
/// Shared by the branded and the compatible engine because the wire format
/// is the same one; only the base URL and the credential differ, and both
/// arrive as parameters.
pub(crate) async fn stream_chat_completion(
    http: &dyn HttpClient,
    base_url: &str,
    model: &str,
    auth: Auth<'_>,
    prompt: &str,
) -> Result<stream::LlmStream, crate::traits::EngineError> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": true,
    });
    let bytes = http.post_json_stream(&url, auth, &[], body).await?;
    Ok(stream::decode_sse(bytes, stream::openai_event))
}

pub(crate) async fn post_chat_completion(
    http: &dyn HttpClient,
    base_url: &str,
    model: &str,
    auth: Auth<'_>,
    prompt: &str,
) -> Result<LlmResponse, EngineError> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
    });
    let text = http.post_json(&url, auth, body).await?;
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
    let content = parsed["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| EngineError::Other("missing content".into()))?;
    Ok(LlmResponse {
        text: content.to_string(),
        usage: openai_usage(&parsed),
    })
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn reads_a_reported_usage_block() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"usage":{"prompt_tokens":12,"completion_tokens":34}}"#)
                .unwrap();
        assert_eq!(
            openai_usage(&v),
            Some(TokenUsage {
                prompt: 12,
                completion: 34
            })
        );
    }

    /// A server that omits the block gets a blank, not a zero: the cost view
    /// must be able to say "unknown" rather than "free".
    #[test]
    fn a_missing_block_is_none_not_zero() {
        let v: serde_json::Value = serde_json::from_str(r#"{"choices":[]}"#).unwrap();
        assert_eq!(openai_usage(&v), None);
    }

    #[test]
    fn half_a_block_is_none() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"usage":{"prompt_tokens":12}}"#).unwrap();
        assert_eq!(openai_usage(&v), None);
    }
}
