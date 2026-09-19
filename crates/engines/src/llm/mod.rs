pub mod compatible;
pub mod openai;

pub use compatible::OpenAiCompatibleLlmEngine;
pub use openai::OpenAiLlmEngine;

use crate::http::{Auth, HttpClient};
use crate::traits::{EngineError, LlmResponse};

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
    })
}
