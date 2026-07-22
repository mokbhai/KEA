pub mod compatible;
pub mod openai;

pub use compatible::OpenAiCompatibleLlmEngine;
pub use openai::OpenAiLlmEngine;

use crate::http::HttpClient;
use crate::traits::{EngineError, LlmResponse};

pub(crate) async fn post_chat_completion(
    http: &dyn HttpClient,
    base_url: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<LlmResponse, EngineError> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
    });
    let (status, text) = http.post_json(&url, api_key, body).await?;
    if !(200..300).contains(&status) {
        return Err(EngineError::http(status, text));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
    let content = parsed["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| EngineError::Other("missing content".into()))?;
    Ok(LlmResponse {
        text: content.to_string(),
    })
}
