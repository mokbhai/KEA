use kea_core::resolve::{Resolution, SlotResolver};
use kea_core::rewrite::{RewriteInput, build_llm_request};
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::BindingRepo;
use kea_core::store::conversations::{ConversationRepo, NewConversation, NewMessage};
use kea_engines::EngineRegistry;
use kea_platform::TextIo;

use crate::feature::{CapKind, CapSlot, Command, Feature};

/// Optional conversation persistence for History (gated by `store_content`).
#[derive(Clone, Copy, Default)]
pub struct ContentStorageOpts<'a> {
    pub store_content: bool,
    pub conversations: Option<&'a ConversationRepo>,
}

impl std::fmt::Debug for ContentStorageOpts<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentStorageOpts")
            .field("store_content", &self.store_content)
            .field("conversations", &self.conversations.is_some())
            .finish()
    }
}

impl<'a> ContentStorageOpts<'a> {
    pub fn enabled(repo: &'a ConversationRepo) -> Self {
        Self {
            store_content: true,
            conversations: Some(repo),
        }
    }

    pub fn disabled() -> Self {
        Self {
            store_content: false,
            conversations: None,
        }
    }
}

pub(crate) async fn maybe_record_conversation(
    storage: ContentStorageOpts<'_>,
    action_id: i64,
    feature_id: &str,
    engine_id: &str,
    model: Option<String>,
    provider_ref: Option<String>,
    user_content: &str,
    assistant_content: &str,
) -> Result<(), String> {
    if !storage.store_content {
        return Ok(());
    }
    let Some(repo) = storage.conversations else {
        return Ok(());
    };

    let conv_id = repo
        .start(&NewConversation {
            action_id: Some(action_id),
            feature_id: feature_id.into(),
            engine_id: engine_id.into(),
            model,
            provider_ref,
        })
        .await
        .map_err(|e| e.to_string())?;

    repo.append_message(&NewMessage {
        conversation_id: conv_id,
        role: "user".into(),
        content: user_content.into(),
        token_count: None,
    })
    .await
    .map_err(|e| e.to_string())?;

    repo.append_message(&NewMessage {
        conversation_id: conv_id,
        role: "assistant".into(),
        content: assistant_content.into(),
        token_count: None,
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

pub struct RewriteFeature;

impl Feature for RewriteFeature {
    fn id(&self) -> &str {
        "rewrite"
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot {
            name: "llm",
            kind: CapKind::Llm,
        }]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "rewrite_selection".into(),
            title: "Rewrite Selection".into(),
            default_accelerator: Some(default_rewrite_accelerator().into()),
        }]
    }
}

fn default_rewrite_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+R"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+R"
    }
}

pub async fn run_rewrite(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    textio: &dyn TextIo,
    input: RewriteInput,
) -> Result<String, String> {
    run_rewrite_with_storage(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        textio,
        input,
        ContentStorageOpts::default(),
    )
    .await
}

pub async fn run_rewrite_with_storage(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    textio: &dyn TextIo,
    mut input: RewriteInput,
    storage: ContentStorageOpts<'_>,
) -> Result<String, String> {
    if input.source_text.is_empty() {
        input.source_text = textio
            .capture_selection()
            .await
            .map_err(|e| e.to_string())?;
    }

    let resolver = SlotResolver::new(engines, bindings);
    let binding = match resolver.resolve_llm("rewrite", "llm").await {
        Ok(Resolution::Bound(b)) => b,
        Ok(Resolution::NeedsChoice(_)) => {
            return Err("multiple llm engines available; bind the rewrite llm slot".into());
        }
        Ok(Resolution::Unresolvable) => return Err("no llm engine available".into()),
        Err(e) => return Err(e.to_string()),
    };
    let engine_id = binding.engine_id.clone();

    let mut llm_req = build_llm_request(&input, presets, overrides)
        .await
        .map_err(|e| e.to_string())?;

    llm_req.model = binding.model.clone();
    // The binding names the provider; without forwarding it the engine falls
    // back to whatever ref it was registered with, so a user-added provider's
    // key is never read and the call fails as "missing api key".
    llm_req.provider_ref = binding.provider_ref.clone();

    let action_id = actions
        .record(NewAction {
            feature_id: "rewrite".into(),
            command: "rewrite_selection".into(),
            engine_id: engine_id.clone(),
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    let engine = match engines.llm(&engine_id) {
        Some(eng) => eng,
        None => {
            let msg = format!("no llm engine '{engine_id}'");
            if let Err(inner) = actions.finish(action_id, "error", Some(&msg)).await {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "rewrite: failed to finish action as error in DB"
                );
            }
            return Err(msg);
        }
    };

    let response = match engine.complete(llm_req).await {
        Ok(resp) => resp,
        Err(e) => {
            if let Err(inner) = actions
                .finish(action_id, "error", Some(&e.to_string()))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "rewrite: failed to finish action as error in DB"
                );
            }
            return Err(e.to_string());
        }
    };

    if let Err(e) = textio.replace(&response.text).await {
        if let Err(inner) = actions.finish(action_id, "error", Some(&e.to_string())).await {
            tracing::warn!(
                error = %inner,
                action_id = %action_id,
                "rewrite: failed to finish action as error in DB"
            );
        }
        return Err(e.to_string());
    }

    if let Err(e) = maybe_record_conversation(
        storage,
        action_id,
        "rewrite",
        &engine_id,
        binding.model.clone(),
        binding.provider_ref.clone(),
        &input.source_text,
        &response.text,
    )
    .await
    {
        if let Err(inner) = actions.finish(action_id, "error", Some(&e)).await {
            tracing::warn!(
                error = %inner,
                action_id = %action_id,
                "rewrite: failed to finish action as error in DB"
            );
        }
        return Err(e);
    }

    if let Err(e) = actions
        .finish(action_id, "ok", None)
        .await
    {
        tracing::warn!(
            error = %e,
            action_id = %action_id,
            "rewrite: failed to finish action as ok in DB"
        );
    }

    Ok(response.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::rewrite::RewriteMode;
    use kea_core::store::conversations::ConversationRepo;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopLlmEngine;
    use kea_platform::TextIoError;
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
        replaced: Mutex<Option<String>>,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace(&self, text: &str) -> Result<(), TextIoError> {
            *self.replaced.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    #[test]
    fn rewrite_declares_llm_slot_and_command() {
        let f = RewriteFeature;
        assert_eq!(f.id(), "rewrite");
        assert_eq!(f.required_caps()[0].name, "llm");
        assert_eq!(f.required_caps()[0].kind, CapKind::Llm);
        let cmds = f.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "rewrite_selection");
        assert_eq!(cmds[0].title, "Rewrite Selection");
        assert!(cmds[0].default_accelerator.is_some());
    }

    #[tokio::test]
    async fn run_rewrite_calls_llm_and_textio() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            selection: "bad text".into(),
            replaced: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        let out = run_rewrite(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            textio.as_ref(),
            RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::Improve,
                preset_id: None,
                custom_instruction: None,
            },
        )
        .await
        .unwrap();

        assert!(out.contains("echo:"));
        assert!(out.contains("bad text"));
        assert_eq!(textio.replaced.lock().unwrap().as_deref(), Some(out.as_str()));

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "rewrite");
        assert_eq!(rows[0].command, "rewrite_selection");
        assert_eq!(rows[0].engine_id, "noop");
        assert_eq!(rows[0].status, "ok");
    }

    #[tokio::test]
    async fn run_rewrite_records_conversation_when_storage_enabled() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            selection: "bad text".into(),
            replaced: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool.clone());
        let conversations = ConversationRepo::new(data_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        run_rewrite_with_storage(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            textio.as_ref(),
            RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::Improve,
                preset_id: None,
                custom_instruction: None,
            },
            ContentStorageOpts::enabled(&conversations),
        )
        .await
        .unwrap();

        let recent = conversations.list_recent(1).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].feature_id, "rewrite");
        assert_eq!(recent[0].engine_id, "noop");

        let messages = conversations.list_messages(recent[0].id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "bad text");
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].content.contains("echo:"));
    }

    #[tokio::test]
    async fn run_rewrite_skips_conversation_when_storage_disabled() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            selection: "bad text".into(),
            replaced: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool.clone());
        let conversations = ConversationRepo::new(data_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        run_rewrite_with_storage(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            textio.as_ref(),
            RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::Improve,
                preset_id: None,
                custom_instruction: None,
            },
            ContentStorageOpts::disabled(),
        )
        .await
        .unwrap();

        assert!(conversations.list_recent(1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_process_failure_leaves_no_pending_row() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            selection: "bad text".into(),
            replaced: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();
        // Separate pool WITHOUT data migrations so conversations table doesn't exist.
        let no_migrations_pool = open_pool("sqlite::memory:").await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool);
        let conversations = ConversationRepo::new(no_migrations_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        let err = run_rewrite_with_storage(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            textio.as_ref(),
            RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::Improve,
                preset_id: None,
                custom_instruction: None,
            },
            ContentStorageOpts::enabled(&conversations),
        )
        .await
        .unwrap_err();

        assert!(err.contains("no such table"), "expected table-missing error, got: {err}");

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "error");

        let detail = actions.get(rows[0].id).await.unwrap().unwrap();
        assert!(
            detail.error.is_some(),
            "action row should carry an error message"
        );
        assert!(
            detail.error.as_deref().unwrap_or("").contains("no such table"),
            "error message should mention the table issue, got: {:?}",
            detail.error
        );
    }
}

/// End-to-end cover for the bug where a user-added OpenAI-compatible provider
/// ("omni": custom base URL + its own key, selected as the capability default)
/// failed every rewrite with "missing api key".
///
/// The whole chain matters here, which is why this is not an engine unit test:
/// the registry is keyed by *engine* id, so `register_phase1_engines` puts a
/// single compatible engine in it under the built-in `local-llm` provider ref.
/// The only thing that says "omni" is the resolved binding. If `run_rewrite`
/// does not forward `binding.provider_ref` into the request, the engine reads
/// local-llm's (non-existent) credential and the user is told a key they just
/// saved — and that "Test connection" just accepted — is missing.
#[cfg(test)]
mod custom_provider_tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::rewrite::{RewriteInput, RewriteMode};
    use kea_core::store::bindings::Binding;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::http::{HttpClient, MultipartPart};
    use kea_engines::provider::{CredentialSource, ProviderConfig, ProviderConfigSource};
    use kea_engines::traits::EngineError;
    use kea_engines::OpenAiCompatibleLlmEngine;
    use kea_platform::{TextIo, TextIoError};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
        replaced: Mutex<Option<String>>,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace(&self, text: &str) -> Result<(), TextIoError> {
            *self.replaced.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    /// Records what the engine actually put on the wire, so the test can
    /// assert *which* provider's URL and key were used rather than merely
    /// that some call succeeded.
    #[derive(Default)]
    struct RecordingHttp {
        calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl HttpClient for RecordingHttp {
        async fn post_json(
            &self,
            url: &str,
            bearer: &str,
            _body: serde_json::Value,
        ) -> Result<(u16, String), EngineError> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), bearer.to_string()));
            Ok((
                200,
                r#"{"choices":[{"message":{"content":"polished"}}]}"#.to_string(),
            ))
        }

        async fn post_multipart(
            &self,
            _url: &str,
            _bearer: &str,
            _parts: Vec<MultipartPart>,
        ) -> Result<(u16, String), EngineError> {
            unreachable!("rewrite never uploads multipart")
        }

        async fn post_binary(
            &self,
            _url: &str,
            _bearer: &str,
            _body: serde_json::Value,
        ) -> Result<(u16, Vec<u8>), EngineError> {
            unreachable!("rewrite never asks for binary")
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

    #[tokio::test]
    async fn rewrite_uses_the_custom_providers_key_and_base_url() {
        // State after the user adds "omni", saves its base URL + key, and
        // picks it as the default writer. Nothing is stored for local-llm —
        // that is the point: the engine must not fall back to it.
        let http = Arc::new(RecordingHttp::default());
        let creds = Arc::new(MapCredentials(HashMap::from([(
            "omni".to_string(),
            "omni-secret".to_string(),
        )])));
        let configs = Arc::new(MapConfigs(HashMap::from([(
            "omni".to_string(),
            ProviderConfig {
                base_url: "https://omni.example/v1".into(),
                default_model: "omni-large".into(),
            },
        )])));

        let mut reg = EngineRegistry::default();
        // Registered exactly as register_phase1_engines does it: one instance,
        // defaulting to the built-in local-llm ref.
        reg.register_llm(Arc::new(OpenAiCompatibleLlmEngine {
            http: http.clone(),
            credentials: creds,
            configs,
            provider_ref: "local-llm".into(),
        }));

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        bindings
            .set(
                kea_core::resolve::DEFAULT_FEATURE_ID,
                "llm",
                Binding {
                    engine_id: "openai-compatible".into(),
                    model: None,
                    provider_ref: Some("omni".into()),
                },
            )
            .await
            .unwrap();

        let actions = ActionRepo::new(data_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);
        let textio = Arc::new(FakeTextIo {
            selection: "bad text".into(),
            replaced: Mutex::new(None),
        });

        let out = run_rewrite(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            textio.as_ref(),
            RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::Improve,
                preset_id: None,
                custom_instruction: None,
            },
        )
        .await
        .expect("rewrite should reach the custom provider");

        assert_eq!(out, "polished");
        let calls = http.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "https://omni.example/v1/chat/completions");
        assert_eq!(calls[0].1, "omni-secret");
    }
}
