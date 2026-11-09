//! Provider model discovery, isolated from feature command wiring.
use crate::AppState;
use kea_core::{rewrite::ProviderConfigRepo, store::settings::SettingsRepo};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::State;

type Cache = HashMap<String, (Instant, Vec<String>)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
const TTL: Duration = Duration::from_secs(180);

fn relevant(id: &str, capability: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let stt = id.contains("whisper") || id.contains("transcribe") || id.contains("scribe_");
    let tts = id.contains("tts") || id.contains("text-to-speech");
    match capability {
        "llm" => {
            !stt && !tts
                && ![
                    "embedding",
                    "embed-",
                    "moderation",
                    "dall-e",
                    "image",
                    "rerank",
                    "realtime",
                ]
                .iter()
                .any(|s| id.contains(s))
        }
        "stt" => !tts && !id.contains("embed"),
        "tts" => !stt && !id.contains("embed"),
        _ => false,
    }
}

fn parse_models(body: &str, provider: &str, capability: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| "Server returned an invalid model list".to_string())?;
    let entries = if provider == "deepgram" {
        value.get("stt")
    } else if value.is_array() {
        Some(&value)
    } else {
        value.get("data")
    };
    let entries = entries
        .and_then(Value::as_array)
        .ok_or("Server returned an unrecognized model list")?;
    let mut ids = Vec::new();
    for entry in entries {
        if provider == "elevenlabs"
            && capability == "stt"
            && entry.get("can_do_speech_to_text").and_then(Value::as_bool) == Some(false)
        {
            continue;
        }
        let id = entry
            .get("id")
            .or_else(|| entry.get("model_id"))
            .and_then(Value::as_str);
        let candidates: Vec<&str> = if provider == "deepgram" {
            entry
                .get("canonical_name")
                .and_then(Value::as_str)
                .into_iter()
                .collect()
        } else {
            id.into_iter().collect()
        };
        for id in candidates {
            if id.is_empty() || !relevant(id, capability) {
                continue;
            }
            // OpenAI's shared catalog identifies speech models by name. Unknown
            // custom-server ids remain visible because they may be multimodal.
            if provider == "openai"
                && capability == "stt"
                && !(id.contains("whisper") || id.contains("transcribe"))
            {
                continue;
            }
            if provider == "openai" && capability == "tts" && !id.contains("tts") {
                continue;
            }
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn status_error(status: u16, name: &str) -> String {
    match status {
        401 | 403 => format!("{name}: key rejected"),
        429 => format!("{name}: rate limited; retry shortly"),
        404 => format!("{name}: model listing is unavailable at this base URL"),
        _ => format!("{name}: model listing failed (HTTP {status})"),
    }
}

async fn fetch_models(
    client: &reqwest::Client,
    base: &str,
    provider: &str,
    capability: &str,
    key: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    // Anthropic paginates its catalog. Bound the loop if a broken server
    // repeats a cursor; never accept a URL supplied by a remote response.
    for _ in 0..100 {
        let mut request = client.get(format!("{}/models", base.trim_end_matches('/')));
        if let Some(cursor) = &after {
            request = request.query(&[("after_id", cursor.as_str())]);
        }
        if provider == "anthropic" {
            request = request
                .header("anthropic-version", "2023-06-01")
                .query(&[("limit", "1000")]);
            if let Some(key) = key {
                request = request.header("x-api-key", key);
            }
        } else if provider == "deepgram" {
            if let Some(key) = key {
                request = request.header("Authorization", format!("Token {key}"));
            }
        } else if provider == "elevenlabs" {
            if let Some(key) = key {
                request = request.header("xi-api-key", key);
            }
        } else if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|e| {
            format!(
                "Couldn't reach {provider}: {}",
                if e.is_timeout() {
                    "request timed out"
                } else {
                    "connection failed"
                }
            )
        })?;
        if !response.status().is_success() {
            return Err(status_error(response.status().as_u16(), provider));
        }
        // Never echo remote error bodies: they can contain credentials.
        let body = response
            .text()
            .await
            .map_err(|_| format!("Couldn't read {provider}'s model list"))?;
        models.extend(parse_models(&body, provider, capability)?);
        let page: Value = serde_json::from_str(&body).map_err(|_| "Invalid model list")?;
        if provider != "anthropic" || page.get("has_more").and_then(Value::as_bool) != Some(true) {
            models.sort();
            models.dedup();
            return Ok(models);
        }
        let cursor = page
            .get("last_id")
            .and_then(Value::as_str)
            .ok_or("Missing model page cursor")?
            .to_string();
        if after.as_ref() == Some(&cursor) {
            return Err("Server repeated a model page".into());
        }
        after = Some(cursor);
    }
    Err("Model catalog exceeded the pagination limit".into())
}

#[tauri::command]
pub async fn list_provider_models(
    state: State<'_, Arc<AppState>>,
    provider_ref: String,
    capability: String,
) -> Result<Vec<String>, String> {
    if !["llm", "stt", "tts"].contains(&capability.as_str()) {
        return Err("Unknown capability".into());
    }
    if (provider_ref == "anthropic" && capability != "llm")
        || (["deepgram", "elevenlabs"].contains(&provider_ref.as_str()) && capability != "stt")
    {
        return Ok(Vec::new());
    }
    let name = crate::commands::list_providers(state.clone())
        .await?
        .into_iter()
        .find(|p| p.provider_ref == provider_ref)
        .map(|p| p.name)
        .unwrap_or_else(|| provider_ref.clone());
    let config = ProviderConfigRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get(&provider_ref)
        .await
        .map_err(|e| e.to_string())?;
    let base = config
        .map(|c| c.base_url)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| kea_engines::well_known(&provider_ref).map(|c| c.base_url))
        .ok_or_else(|| format!("Set a base URL for {name} in AI Providers first"))?;
    let key = state
        .credentials
        .get(&provider_ref)
        .await
        .map_err(|_| "Couldn't read the saved API key")?;
    let cache_key = format!(
        "{provider_ref}:{capability}:{base}:{:x}",
        Sha256::digest(key.as_deref().unwrap_or_default())
    );
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((at, models)) = cache
        .lock()
        .map_err(|_| "Model cache unavailable")?
        .get(&cache_key)
    {
        if at.elapsed() < TTL {
            return Ok(models.clone());
        }
    }
    let models = if provider_ref == "local-llm"
        && key.is_none()
        && kea_engines::LOCAL_LLM_PROBES
            .iter()
            .any(|p| p.base_url == base.trim_end_matches('/'))
    {
        let servers = kea_engines::discover_local_llms(&kea_engines::ReqwestProbe::new()).await;
        let server = servers
            .into_iter()
            .find(|s| s.base_url == base.trim_end_matches('/'))
            .ok_or("Couldn't reach Local server")?;
        server
            .models
            .into_iter()
            .filter(|id| relevant(id, &capability))
            .collect()
    } else {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Couldn't create model client")?;
        fetch_models(&client, &base, &provider_ref, &capability, key.as_deref())
            .await
            .map_err(|message| message.replace(&provider_ref, &name))?
    };
    // Empty catalogs are deliberately not cached, so Retry can discover a
    // just-installed model immediately. Config/key changes have distinct keys.
    if !models.is_empty() {
        let mut cache = cache.lock().map_err(|_| "Model cache unavailable")?;
        cache.retain(|_, (at, _)| at.elapsed() < TTL);
        cache.insert(cache_key, (Instant::now(), models.clone()));
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_and_filters_openai() {
        let body = r#"{"data":[{"id":"gpt-4o"},{"id":"whisper-1"},{"id":"tts-1"},{"id":"text-embedding-3-small"},{"id":"custom-chat"}]}"#;
        assert_eq!(
            parse_models(body, "openai", "llm").unwrap(),
            ["custom-chat", "gpt-4o"]
        );
        assert_eq!(parse_models(body, "openai", "stt").unwrap(), ["whisper-1"]);
        assert_eq!(parse_models(body, "openai", "tts").unwrap(), ["tts-1"]);
        assert!(parse_models("<html>", "openai", "llm").is_err());
    }
    #[test]
    fn parses_vendor_shapes() {
        assert_eq!(
            parse_models(
                r#"{"data":[{"id":"claude-sonnet","type":"model"}]}"#,
                "anthropic",
                "llm"
            )
            .unwrap(),
            ["claude-sonnet"]
        );
        assert_eq!(
            parse_models(
                r#"{"stt":[{"canonical_name":"nova-3","uuid":"abc"}]}"#,
                "deepgram",
                "stt"
            )
            .unwrap(),
            ["nova-3"]
        );
        assert_eq!(parse_models(r#"[{"model_id":"scribe_v1","can_do_speech_to_text":true},{"model_id":"eleven_multilingual_v2","can_do_speech_to_text":false}]"#, "elevenlabs", "stt").unwrap(), ["scribe_v1"]);
    }
    #[tokio::test]
    async fn openai_catalog_uses_saved_auth_and_filters_speech() {
        use wiremock::{
            matchers::{header, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(path("/models"))
            .and(header("authorization", "Bearer saved-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data": [{"id": "gpt-4o"}, {"id": "whisper-1"}]}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let models = fetch_models(
            &reqwest::Client::new(),
            &server.uri(),
            "openai",
            "llm",
            Some("saved-key"),
        )
        .await
        .unwrap();
        assert_eq!(models, ["gpt-4o"]);
    }

    #[tokio::test]
    async fn anthropic_follows_model_cursors() {
        use wiremock::{
            matchers::{path, query_param},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [{"id": "claude-a"}], "last_id": "claude-a", "has_more": true})))
            .up_to_n_times(1).expect(1).mount(&server).await;
        Mock::given(path("/models"))
            .and(query_param("after_id", "claude-a"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data": [{"id": "claude-b"}], "has_more": false}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let models = fetch_models(
            &reqwest::Client::new(),
            &server.uri(),
            "anthropic",
            "llm",
            Some("saved-key"),
        )
        .await
        .unwrap();
        assert_eq!(models, ["claude-a", "claude-b"]);
    }

    #[tokio::test]
    async fn sends_anthropic_headers_and_maps_rejected_key() {
        use wiremock::{
            matchers::{header, path},
            Mock, MockServer, ResponseTemplate,
        };
        let server = MockServer::start().await;
        Mock::given(path("/v1/models"))
            .and(header("x-api-key", "secret"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(401).set_body_string("secret"))
            .mount(&server)
            .await;
        let err = fetch_models(
            &reqwest::Client::new(),
            &format!("{}/v1", server.uri()),
            "anthropic",
            "llm",
            Some("secret"),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "anthropic: key rejected");
        assert!(status_error(429, "OpenWebUI").contains("rate limited"));
        assert!(status_error(404, "OpenWebUI").contains("base URL"));
    }
}
