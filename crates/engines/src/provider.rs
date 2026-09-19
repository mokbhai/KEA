use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::http::Auth;
use crate::traits::EngineError;

/// The branded OpenAI endpoint. Every engine that defaults to it reads it
/// from here rather than spelling the URL out again.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Providers the app knows the endpoint of, so the user only supplies a key.
///
/// This is the answer to "does this provider earn its code?". Groq serves
/// OpenAI's own wire format for both chat and transcription, so supporting it
/// is not an engine at all — it is a base URL and a default model, and a row
/// here is the entire implementation. The same is true of any other
/// OpenAI-compatible host.
///
/// It is consulted by [`resolve`] *after* the user's stored configuration and
/// *before* the engine's own defaults, which makes it a starting point rather
/// than a policy: anyone who edits the base URL of one of these providers
/// keeps their edit.
///
/// Deepgram and ElevenLabs appear here too even though their engines are
/// bespoke — the table is "where does this provider live", not "which wire
/// format does it speak".
pub static WELL_KNOWN_PROVIDERS: &[(&str, &str, &str)] = &[
    // (provider_ref, base_url, default_model)
    ("openai", OPENAI_BASE_URL, "gpt-4o-mini"),
    (
        "anthropic",
        "https://api.anthropic.com/v1",
        "claude-sonnet-4-5",
    ),
    // Groq's OpenAI-compatible surface. `whisper-large-v3-turbo` is the
    // transcription model; a Groq provider bound to the *chat* slot supplies
    // its own model id, which is why the default here names the one that
    // makes this provider worth having.
    (
        "groq",
        "https://api.groq.com/openai/v1",
        "whisper-large-v3-turbo",
    ),
    ("deepgram", "https://api.deepgram.com/v1", "nova-3"),
    ("elevenlabs", "https://api.elevenlabs.io/v1", "scribe_v1"),
];

/// The published endpoint for a provider the app ships knowledge of.
pub fn well_known(provider_ref: &str) -> Option<ProviderConfig> {
    WELL_KNOWN_PROVIDERS
        .iter()
        .find(|(id, _, _)| *id == provider_ref)
        .map(|(_, base_url, model)| ProviderConfig {
            base_url: (*base_url).to_string(),
            default_model: (*model).to_string(),
        })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub default_model: String,
}

#[async_trait]
pub trait CredentialSource: Send + Sync {
    /// Returns the API key/secret for a provider_ref.
    ///
    /// `Ok(None)` means the key has never been set.
    /// `Err(...)` means the credential store (e.g. OS keyring) failed.
    async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String>;
}

#[async_trait]
pub trait ProviderConfigSource: Send + Sync {
    /// Returns the base_url + default_model for a provider_ref, if configured.
    async fn config(&self, provider_ref: &str) -> Option<ProviderConfig>;
}

/// What an engine falls back to when the user has configured nothing for the
/// provider. Passed in by each engine because the fallback is the engine's
/// own policy, not the resolver's.
#[derive(Clone, Copy, Debug)]
pub struct Defaults<'a> {
    pub base_url: &'a str,
    pub model: &'a str,
}

/// The provider one call ended up pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProvider {
    pub provider_ref: String,
    /// `None` when no key is stored — which is a valid setup for a local
    /// server, so whether that is fatal is the engine's call, not the
    /// resolver's. See [`ResolvedProvider::require_key`].
    pub api_key: Option<String>,
    pub base_url: String,
    pub default_model: String,
}

impl ResolvedProvider {
    /// Bearer auth when a key is stored, unauthenticated when none is.
    pub fn auth(&self) -> Auth<'_> {
        Auth::from_optional_key(self.api_key.as_deref())
    }

    /// For endpoints that genuinely cannot work without a credential: fail
    /// before the round-trip rather than on the server's 401.
    pub fn require_key(&self) -> Result<&str, EngineError> {
        self.api_key
            .as_deref()
            .ok_or_else(|| EngineError::Auth("missing api key".into()))
    }
}

/// Resolves which provider a call uses, and its key, base URL and model.
///
/// `requested_ref` is the ref the resolved binding carried; an engine is
/// registered once under its own id, so a single `openai-compatible` instance
/// serves *every* user-added provider and `fallback_ref` (the engine's own
/// registered ref) only covers a binding that names none.
///
/// `defaults` of `None` means the engine has no legitimate default — there is
/// no "the" local server — so an unconfigured provider fails closed.
pub async fn resolve(
    credentials: &dyn CredentialSource,
    configs: &dyn ProviderConfigSource,
    requested_ref: Option<&str>,
    fallback_ref: &str,
    defaults: Option<Defaults<'_>>,
) -> Result<ResolvedProvider, EngineError> {
    let provider_ref = requested_ref.unwrap_or(fallback_ref);
    let api_key = credentials
        .api_key(provider_ref)
        .await
        .map_err(|e| EngineError::Auth(format!("keychain access failed: {e}")))?;
    let cfg = match configs.config(provider_ref).await {
        Some(cfg) => cfg,
        // The user has configured nothing for this ref. If the app knows
        // where the provider lives, that is a far better answer than "missing
        // provider config" — it is the difference between pasting a key and
        // also having to know that Groq's compatible surface hangs off
        // `/openai/v1`. Stored config still wins, so this never overrides an
        // edit.
        None => match well_known(provider_ref) {
            Some(cfg) => cfg,
            None => {
                let defaults = defaults.ok_or_else(|| {
                    EngineError::Config(format!("missing provider config for {provider_ref}"))
                })?;
                ProviderConfig {
                    base_url: defaults.base_url.into(),
                    default_model: defaults.model.into(),
                }
            }
        },
    };
    Ok(ResolvedProvider {
        provider_ref: provider_ref.to_string(),
        api_key,
        base_url: cfg.base_url,
        default_model: cfg.default_model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MapCredentials(HashMap<String, String>);

    #[async_trait]
    impl CredentialSource for MapCredentials {
        async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String> {
            Ok(self.0.get(provider_ref).cloned())
        }
    }

    struct FailingCredentials;

    #[async_trait]
    impl CredentialSource for FailingCredentials {
        async fn api_key(&self, _provider_ref: &str) -> Result<Option<String>, String> {
            Err("keyring locked".into())
        }
    }

    struct MapConfigs(HashMap<String, ProviderConfig>);

    #[async_trait]
    impl ProviderConfigSource for MapConfigs {
        async fn config(&self, provider_ref: &str) -> Option<ProviderConfig> {
            self.0.get(provider_ref).cloned()
        }
    }

    fn empty_creds() -> MapCredentials {
        MapCredentials(HashMap::new())
    }

    fn empty_configs() -> MapConfigs {
        MapConfigs(HashMap::new())
    }

    fn openai_defaults() -> Option<Defaults<'static>> {
        Some(Defaults {
            base_url: OPENAI_BASE_URL,
            model: "gpt-4o-mini",
        })
    }

    #[test]
    fn provider_config_constructs() {
        let cfg = ProviderConfig {
            base_url: OPENAI_BASE_URL.into(),
            default_model: "gpt-4o-mini".into(),
        };
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.default_model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn request_ref_beats_the_registered_fallback() {
        let creds = MapCredentials(HashMap::from([("omni".into(), "omni-key".into())]));
        let configs = MapConfigs(HashMap::from([(
            "omni".to_string(),
            ProviderConfig {
                base_url: "https://omni.example/v1".into(),
                default_model: "omni-large".into(),
            },
        )]));
        let resolved = resolve(&creds, &configs, Some("omni"), "local-llm", None)
            .await
            .unwrap();
        assert_eq!(resolved.provider_ref, "omni");
        assert_eq!(resolved.base_url, "https://omni.example/v1");
        assert_eq!(resolved.api_key.as_deref(), Some("omni-key"));
    }

    #[tokio::test]
    async fn falls_back_to_defaults_when_unconfigured() {
        let resolved = resolve(
            &empty_creds(),
            &empty_configs(),
            None,
            "openai",
            openai_defaults(),
        )
        .await
        .unwrap();
        assert_eq!(resolved.base_url, OPENAI_BASE_URL);
        assert_eq!(resolved.default_model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn fails_closed_without_defaults() {
        let err = resolve(&empty_creds(), &empty_configs(), None, "local-llm", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing provider config"));
    }

    /// The keyless-local-server case: no credential is not an error here, and
    /// the request simply goes out unauthenticated.
    #[tokio::test]
    async fn a_missing_key_is_not_an_error() {
        let configs = MapConfigs(HashMap::from([(
            "local-llm".to_string(),
            ProviderConfig {
                base_url: "http://127.0.0.1:11434/v1".into(),
                default_model: "llama3".into(),
            },
        )]));
        let resolved = resolve(&empty_creds(), &configs, None, "local-llm", None)
            .await
            .unwrap();
        assert_eq!(resolved.api_key, None);
        assert_eq!(resolved.auth(), Auth::None);
        assert!(resolved.require_key().is_err());
    }

    /// The point of the table: a provider the app knows resolves with nothing
    /// stored and no engine defaults — a key is all the user supplies.
    #[tokio::test]
    async fn a_well_known_provider_needs_no_stored_config() {
        let creds = MapCredentials(HashMap::from([("groq".into(), "gsk-test".into())]));
        let resolved = resolve(&creds, &empty_configs(), Some("groq"), "openai", None)
            .await
            .unwrap();
        assert_eq!(resolved.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(resolved.default_model, "whisper-large-v3-turbo");
        assert_eq!(resolved.api_key.as_deref(), Some("gsk-test"));
    }

    /// And it is a starting point, not a policy: an edited base URL survives.
    #[tokio::test]
    async fn a_stored_config_still_beats_the_well_known_table() {
        let configs = MapConfigs(HashMap::from([(
            "groq".to_string(),
            ProviderConfig {
                base_url: "https://gateway.internal/v1".into(),
                default_model: "whisper-large-v3".into(),
            },
        )]));
        let resolved = resolve(&empty_creds(), &configs, Some("groq"), "openai", None)
            .await
            .unwrap();
        assert_eq!(resolved.base_url, "https://gateway.internal/v1");
    }

    /// A ref nobody knows still fails closed when the engine has no default.
    #[tokio::test]
    async fn an_unknown_ref_is_unaffected_by_the_table() {
        let err = resolve(&empty_creds(), &empty_configs(), Some("nowhere"), "x", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing provider config"));
    }

    /// Every row has to be usable as-is: a typo'd URL here is a provider that
    /// fails for everyone who selects it, with nothing in the UI to explain.
    #[test]
    fn every_well_known_row_is_a_complete_https_endpoint() {
        let mut seen = std::collections::HashSet::new();
        for (id, base_url, model) in WELL_KNOWN_PROVIDERS {
            assert!(seen.insert(*id), "duplicate provider ref {id}");
            assert!(base_url.starts_with("https://"), "{id}: {base_url}");
            assert!(!base_url.ends_with('/'), "{id}: {base_url}");
            assert!(!model.is_empty(), "{id}");
        }
    }

    #[tokio::test]
    async fn keychain_failure_is_an_auth_error() {
        let err = resolve(
            &FailingCredentials,
            &empty_configs(),
            None,
            "openai",
            openai_defaults(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("keychain access failed"));
    }
}
