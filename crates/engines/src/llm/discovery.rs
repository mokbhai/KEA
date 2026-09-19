//! Finding the local LLM server the user already has running.
//!
//! The `local-llm` provider has always worked — if you knew that Ollama
//! listens on 11434, that its OpenAI-compatible surface lives under `/v1`,
//! and which of your installed models is spelled `qwen3:8b` rather than
//! `qwen3-8b`. That is three things to get exactly right before the first
//! rewrite, and getting any of them wrong produces the same "missing
//! provider config" either way. This module removes all three: probe the two
//! well-known ports, ask whichever answers what it has installed, and hand
//! the UI a base URL and a model list to pick from.
//!
//! ## Why a probe and not a setting
//!
//! Nothing here is configurable on purpose. The two ports are defaults that
//! neither project encourages changing, and a user who *has* moved one is
//! exactly the user who can type a base URL into the existing provider form.
//! A discovery mechanism with its own settings would be a second way to
//! configure the same provider.
//!
//! ## Why it can never block the app
//!
//! A probe of a port nobody is listening on fails immediately on loopback,
//! but a port held by something that is *not* an LLM server can accept the
//! connection and never answer. Every probe therefore carries its own
//! timeout, and the two run concurrently, so the worst case is one timeout
//! and not two.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long one probe waits before giving up on a port.
///
/// Short: this runs while somebody is looking at a settings panel, and a
/// local server that has not answered in two seconds is not one we can
/// usefully rewrite through anyway.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One server's native model-list endpoint, and the OpenAI-compatible base
/// URL to use once a model is picked.
///
/// A table rather than two near-identical functions: the only things that
/// differ between Ollama and LM Studio are these four strings, and a second
/// function is where the two drift.
pub struct LocalLlmProbe {
    /// Stable id, used as the `provider_ref` suggestion and in the UI.
    pub id: &'static str,
    pub display_name: &'static str,
    /// The OpenAI-compatible base URL a completion is posted to.
    pub base_url: &'static str,
    /// Where to ask what is installed.
    pub models_url: &'static str,
    /// How to read that answer. See [`ModelListShape`].
    pub shape: ModelListShape,
}

/// The two ways a local server reports its models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelListShape {
    /// Ollama's native `/api/tags`: `{"models":[{"name":"qwen3:8b",…}]}`.
    ///
    /// Used in preference to its own `/v1/models` because the native list is
    /// the one that always exists — the compatible shim is newer than a lot
    /// of installed Ollama builds.
    OllamaTags,
    /// The OpenAI shape: `{"data":[{"id":"…"}]}`. LM Studio serves only this.
    OpenAiModels,
}

impl ModelListShape {
    /// Pulls the model ids out of one response body.
    ///
    /// Tolerant by design: a body that does not parse, or parses to
    /// something with no models in it, means "this is not a server we can
    /// use" rather than an error the user has to read. The *port answered*
    /// is not evidence that an LLM runs there.
    pub fn read(self, body: &str) -> Vec<String> {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) else {
            return Vec::new();
        };
        let (array, key) = match self {
            ModelListShape::OllamaTags => (parsed.get("models"), "name"),
            ModelListShape::OpenAiModels => (parsed.get("data"), "id"),
        };
        array
            .and_then(|v| v.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get(key)?.as_str())
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The servers this probes, in the order a picker should offer them.
///
/// Ollama first because it is the one that is usually already running as a
/// service rather than as an app somebody opened.
pub static LOCAL_LLM_PROBES: &[LocalLlmProbe] = &[
    LocalLlmProbe {
        id: "ollama",
        display_name: "Ollama",
        base_url: "http://127.0.0.1:11434/v1",
        models_url: "http://127.0.0.1:11434/api/tags",
        shape: ModelListShape::OllamaTags,
    },
    LocalLlmProbe {
        id: "lm-studio",
        display_name: "LM Studio",
        base_url: "http://127.0.0.1:1234/v1",
        models_url: "http://127.0.0.1:1234/v1/models",
        shape: ModelListShape::OpenAiModels,
    },
];

/// One local server that answered, and what it has installed.
///
/// `models` may be empty: a running Ollama with nothing pulled is a real and
/// common state, and telling the user "Ollama is running, no models
/// installed" is far more useful than not mentioning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalLlmServer {
    pub id: String,
    pub display_name: String,
    /// The base URL to store on the provider — already OpenAI-compatible, so
    /// the existing `openai-compatible` engine serves it unchanged.
    pub base_url: String,
    pub models: Vec<String>,
}

/// What one probe is allowed to do, so the discovery logic is testable
/// without a listening socket.
///
/// Narrower than [`crate::http::HttpClient`] on purpose: discovery only ever
/// GETs, never authenticates, and must treat every failure as "not there".
#[async_trait::async_trait]
pub trait ProbeTransport: Send + Sync {
    /// The body, or `None` for anything that was not a clean answer —
    /// refused, timed out, 404, malformed. The caller cannot act on *why*.
    async fn get(&self, url: &str, timeout: Duration) -> Option<String>;
}

pub struct ReqwestProbe {
    client: reqwest::Client,
}

impl ReqwestProbe {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProbeTransport for ReqwestProbe {
    async fn get(&self, url: &str, timeout: Duration) -> Option<String> {
        let resp = self.client.get(url).timeout(timeout).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.text().await.ok()
    }
}

/// Probes every known local server concurrently and returns the ones that
/// answered, in [`LOCAL_LLM_PROBES`] order.
///
/// Concurrent rather than sequential because the cost of this call is
/// dominated by the *absent* server's timeout, and running them in series
/// would double it for the common case of having exactly one installed.
pub async fn discover_local_llms(transport: &dyn ProbeTransport) -> Vec<LocalLlmServer> {
    let probes = LOCAL_LLM_PROBES.iter().map(|probe| async move {
        let body = transport.get(probe.models_url, PROBE_TIMEOUT).await?;
        Some(LocalLlmServer {
            id: probe.id.to_string(),
            display_name: probe.display_name.to_string(),
            base_url: probe.base_url.to_string(),
            models: probe.shape.read(&body),
        })
    });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeProbe {
        bodies: HashMap<&'static str, &'static str>,
        asked: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ProbeTransport for FakeProbe {
        async fn get(&self, url: &str, _timeout: Duration) -> Option<String> {
            self.asked.lock().unwrap().push(url.to_string());
            self.bodies.get(url).map(|b| b.to_string())
        }
    }

    #[test]
    fn reads_ollamas_native_tag_list() {
        let body = r#"{"models":[{"name":"qwen3:8b","size":1},{"name":"llama3.2:3b"}]}"#;
        assert_eq!(
            ModelListShape::OllamaTags.read(body),
            vec!["qwen3:8b".to_string(), "llama3.2:3b".to_string()]
        );
    }

    #[test]
    fn reads_lm_studios_openai_list() {
        let body = r#"{"data":[{"id":"qwen2.5-7b-instruct","object":"model"}]}"#;
        assert_eq!(
            ModelListShape::OpenAiModels.read(body),
            vec!["qwen2.5-7b-instruct".to_string()]
        );
    }

    /// A port answering is not evidence an LLM is behind it: some other
    /// service on 1234 returning HTML must produce no models, not a panic and
    /// not a fabricated entry.
    #[test]
    fn a_body_that_is_not_a_model_list_yields_nothing() {
        for shape in [ModelListShape::OllamaTags, ModelListShape::OpenAiModels] {
            assert!(shape.read("<html>hello</html>").is_empty());
            assert!(shape.read("{}").is_empty());
            assert!(shape.read(r#"{"models":"not an array"}"#).is_empty());
            // Entries with no usable id are dropped rather than becoming "".
            assert!(shape
                .read(r#"{"models":[{}],"data":[{"id":""}]}"#)
                .is_empty());
        }
    }

    #[tokio::test]
    async fn finds_only_the_server_that_answers() {
        let probe = FakeProbe {
            bodies: HashMap::from([(
                "http://127.0.0.1:11434/api/tags",
                r#"{"models":[{"name":"qwen3:8b"}]}"#,
            )]),
            ..Default::default()
        };
        let found = discover_local_llms(&probe).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ollama");
        // The stored base URL is the compatible one, not the native API the
        // probe asked — pointing the provider at /api/tags would 404 on every
        // rewrite.
        assert_eq!(found[0].base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(found[0].models, vec!["qwen3:8b".to_string()]);
        // Both ports were tried even though only one answered.
        assert_eq!(probe.asked.lock().unwrap().len(), 2);
    }

    /// A running server with nothing pulled is a real state, and reporting it
    /// is the difference between "install a model" and "is Ollama even on?".
    #[tokio::test]
    async fn a_running_server_with_no_models_is_still_found() {
        let probe = FakeProbe {
            bodies: HashMap::from([("http://127.0.0.1:11434/api/tags", r#"{"models":[]}"#)]),
            ..Default::default()
        };
        let found = discover_local_llms(&probe).await;
        assert_eq!(found.len(), 1);
        assert!(found[0].models.is_empty());
    }

    #[tokio::test]
    async fn nothing_running_is_an_empty_list_not_an_error() {
        assert!(discover_local_llms(&FakeProbe::default()).await.is_empty());
    }

    #[tokio::test]
    async fn both_servers_are_reported_in_probe_order() {
        let probe = FakeProbe {
            bodies: HashMap::from([
                (
                    "http://127.0.0.1:11434/api/tags",
                    r#"{"models":[{"name":"qwen3:8b"}]}"#,
                ),
                (
                    "http://127.0.0.1:1234/v1/models",
                    r#"{"data":[{"id":"qwen2.5-7b-instruct"}]}"#,
                ),
            ]),
            ..Default::default()
        };
        let found = discover_local_llms(&probe).await;
        assert_eq!(
            found.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["ollama", "lm-studio"]
        );
    }
}
