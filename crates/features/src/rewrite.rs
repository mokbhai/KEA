use kea_core::resolve::SlotResolver;
use kea_core::rewrite::{build_llm_request, RewriteInput};
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::conversations::{ConversationRepo, MessageRole, NewConversation, NewMessage};
use kea_core::store::usage::{NewUsageEvent, UsageRepo};
use kea_engines::traits::{LlmRequest, TokenUsage};
use kea_engines::EngineRegistry;
use kea_platform::TextIo;

use crate::feature::{ActionGuard, CapKind, CapSlot, Command, Feature, ProfileOverrides};

/// What a run is allowed to write down about its LLM calls.
///
/// Two independent records, which is why they are two fields rather than one
/// flag:
///
/// * the **conversation** — the text that went to the provider and came back —
///   is gated by `store_content`, because it is the user's own words;
/// * the **usage ledger** is not, because someone who would rather their words
///   were not kept still wants to know what they spent. It is written whenever
///   a repo is here at all.
///
/// `Default` is "write nothing", which is what the callers that persist
/// neither pass.
#[derive(Clone, Copy, Default)]
pub struct ContentStorageOpts<'a> {
    pub store_content: bool,
    pub conversations: Option<&'a ConversationRepo>,
    pub usage: Option<&'a UsageRepo>,
}

impl std::fmt::Debug for ContentStorageOpts<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentStorageOpts")
            .field("store_content", &self.store_content)
            .field("conversations", &self.conversations.is_some())
            .field("usage", &self.usage.is_some())
            .finish()
    }
}

impl<'a> ContentStorageOpts<'a> {
    pub fn enabled(repo: &'a ConversationRepo) -> Self {
        Self {
            store_content: true,
            conversations: Some(repo),
            usage: None,
        }
    }

    pub fn disabled() -> Self {
        Self::default()
    }

    /// Adds the usage ledger, independently of whether content is stored.
    pub fn with_usage(mut self, usage: &'a UsageRepo) -> Self {
        self.usage = Some(usage);
        self
    }
}

/// Everything one finished LLM call leaves behind: the conversation for
/// History, and a row in the usage ledger.
///
/// One function rather than two calls at every site, because the two records
/// describe the same call and a site that remembered one and forgot the other
/// is exactly how `messages.token_count` sat unwritten since it was added.
///
/// `usage` is `None` whenever the provider reported nothing, and it stays
/// `None` all the way down — no zero-filling, no estimating. See
/// [`kea_core::store::usage`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_llm_call(
    storage: ContentStorageOpts<'_>,
    action_id: i64,
    feature_id: &str,
    engine_id: &str,
    model: Option<String>,
    provider_ref: Option<String>,
    user_content: &str,
    assistant_content: &str,
    usage: Option<TokenUsage>,
) -> Result<(), String> {
    if let Some(repo) = storage.usage {
        repo.record(&NewUsageEvent {
            action_id: Some(action_id),
            model: model.clone(),
            provider_ref: provider_ref.clone(),
            prompt_tokens: usage.map(|u| i64::from(u.prompt)),
            completion_tokens: usage.map(|u| i64::from(u.completion)),
            ..NewUsageEvent::new(feature_id, engine_id)
        })
        .await
        .map_err(|e| e.to_string())?;
    }

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

    // The prompt count belongs to what was sent and the completion count to
    // what came back, which is what the two message rows already are.
    repo.append_message(&NewMessage {
        conversation_id: conv_id,
        role: MessageRole::User,
        content: user_content.into(),
        token_count: usage.map(|u| i64::from(u.prompt)),
    })
    .await
    .map_err(|e| e.to_string())?;

    repo.append_message(&NewMessage {
        conversation_id: conv_id,
        role: MessageRole::Assistant,
        content: assistant_content.into(),
        token_count: usage.map(|u| i64::from(u.completion)),
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

    /// Three commands, one feature, one `llm` slot.
    ///
    /// The palette and the screen-capture shortcut are rewrites with an
    /// instruction typed for the occasion, not a second feature: giving them a
    /// `Feature` of their own would give them a *second* `llm` slot binding
    /// for the user to configure, and picking a writer for "Rewrite" while the
    /// palette silently used another is not a distinction anyone asked for.
    /// [`crate::feature::ProfileOverrides`], the preset list and the prompt
    /// overrides all follow the same seam for the same reason.
    fn commands(&self) -> Vec<Command> {
        vec![
            Command {
                id: "rewrite_selection".into(),
                title: "Rewrite Selection".into(),
                default_accelerator: Some(default_rewrite_accelerator().into()),
            },
            Command {
                id: PALETTE_COMMAND.into(),
                title: "Prompt Palette".into(),
                default_accelerator: Some(default_palette_accelerator().into()),
            },
            Command {
                id: OCR_COMMAND.into(),
                title: "Capture Screen Text".into(),
                default_accelerator: Some(crate::feature::platform_accelerator('O')),
            },
            Command {
                id: UNDO_COMMAND.into(),
                title: "Undo Last Rewrite".into(),
                default_accelerator: Some(crate::feature::platform_accelerator('U')),
            },
        ]
    }
}

/// Command id of the prompt palette, shared with the app layer's hotkey table.
pub const PALETTE_COMMAND: &str = "prompt_palette";

/// Command id of the screenshot-OCR shortcut, which opens the palette
/// prefilled with whatever text was recognised.
pub const OCR_COMMAND: &str = "ocr_capture";

/// Command id of "put my own words back", the fourth rewrite command.
///
/// A rewrite command rather than a feature of its own, for the same reason the
/// palette is one: it acts on the rewrite this feature just made, and giving
/// it a `Feature` would give it an `llm` slot it never calls.
///
/// **`U`, not `Z`.** `Cmd+Z` and `Cmd+Shift+Z` are every app's own undo and
/// redo; registering either globally would take them away from every other app
/// on the Mac for the sake of a shortcut used a few times a day. The app's own
/// undo is also the *right* first thing to try — a clipboard paste is usually
/// one `Cmd+Z` away — and this exists for the cases where it is not: an
/// Accessibility insertion, an editor whose undo stack does not see it, or a
/// document edited since.
pub const UNDO_COMMAND: &str = "undo_rewrite";

/// `Cmd+Shift+R/D/T/M` and now `O` are taken by the other four, and the
/// screenshot keys `Cmd+Shift+3..6` belong to macOS. Space is spelled out
/// rather than going through [`crate::feature::platform_accelerator`], which
/// takes a `char`.
fn default_palette_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+Space"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+Space"
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

/// What a finished rewrite put in the document, and what it took out.
///
/// The second half is the whole reason this is a struct: the selection is
/// gone by the time anyone wants it back, so unless the run hands the original
/// text to its caller, nothing above can offer to restore it. See
/// `commands::UndoOffer` in the app layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// What the provider wrote — now in the user's document.
    pub text: String,
    /// What it replaced. Empty when there was no selection to begin with.
    pub source_text: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_rewrite(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    textio: &dyn TextIo,
    input: RewriteInput,
    profile: &ProfileOverrides,
) -> Result<String, String> {
    run_rewrite_with_storage(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        textio,
        input,
        profile,
        ContentStorageOpts::default(),
    )
    .await
    .map(|outcome| outcome.text)
}

#[allow(clippy::too_many_arguments)]
pub async fn run_rewrite_with_storage(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    textio: &dyn TextIo,
    mut input: RewriteInput,
    profile: &ProfileOverrides,
    storage: ContentStorageOpts<'_>,
) -> Result<RewriteOutcome, String> {
    if input.source_text.is_empty() {
        input.source_text = textio
            .capture_selection()
            .await
            .map_err(|e| e.to_string())?;
    }

    let (text, action_id) = complete_rewrite(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        "rewrite_selection",
        &input,
        profile,
        storage,
    )
    .await?;

    // The ledger row is still open — `complete_rewrite` released it — because
    // a rewrite is not done until the text is back in the user's document.
    // Taking it back into a guard is what keeps the "every exit closes the
    // row" rule in one type rather than two copies of `actions.finish`.
    let guard = ActionGuard::new(actions, action_id, "rewrite");
    match textio
        .replace_with_mode(&text, profile.replace_mode())
        .await
    {
        Ok(()) => {
            guard.succeed().await;
            Ok(RewriteOutcome {
                text,
                source_text: input.source_text,
            })
        }
        Err(e) => Err(guard.fail(e).await),
    }
}

/// Everything between an input and a finished LLM response: resolve the
/// binding, build the request, open the ledger row, call the engine, record
/// the conversation. Returns the text and the **still-open** row id.
///
/// `command` says which of [`RewriteFeature`]'s commands this run is, so
/// History can tell a palette ask from a plain rewrite; the feature id stays
/// `rewrite` either way.
///
/// The seam exists because the two callers deliver differently and at
/// different times. A hotkey rewrite writes the answer straight back over the
/// selection; the palette hides its window, reactivates the app the user came
/// from, and only then decides between replace, insert and the clipboard — and
/// may find that the user dismissed it while the request was in flight, in
/// which case the row closes without anything being delivered at all. Neither
/// half may re-capture the selection: for the palette that would fire a second
/// ⌘C into whatever is frontmost *now*, which is KEA's own window.
///
/// The row is handed over open (via [`ActionGuard::release`], as
/// `run_tts_synthesize` does) rather than closed here and reopened: History
/// would otherwise show a rewrite that succeeded a second before the paste
/// that failed.
#[allow(clippy::too_many_arguments)]
pub async fn complete_rewrite(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    command: &str,
    input: &RewriteInput,
    profile: &ProfileOverrides,
    storage: ContentStorageOpts<'_>,
) -> Result<(String, i64), String> {
    // Three places may name the LLM, and `llm_binding_over` is the one that
    // says in which order. A profile or a preset substitutes for the resolver
    // rather than patching its result: `llm_binding()` is None unless an
    // engine id is set, so a half-filled override inherits the global binding
    // instead of half-applying one.
    let preset_binding = preset_llm_binding(presets, input.preset_id.as_deref()).await;
    let binding = match profile.llm_binding_over(preset_binding) {
        Some(binding) => binding,
        None => SlotResolver::new(engines, bindings)
            .require_llm("rewrite")
            .await
            .map_err(|e| e.to_string())?,
    };
    let engine_id = binding.engine_id.clone();

    let mut llm_req = build_llm_request(input, presets, overrides)
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
            command: command.into(),
            engine_id: engine_id.clone(),
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    // From here the ledger row exists, so every exit closes it.
    let guard = ActionGuard::new(actions, action_id, "rewrite");
    match run_completion(engines, storage, &binding, input, llm_req, action_id).await {
        // Released, not closed: the caller delivers the text and owns the
        // outcome. See this function's doc comment.
        Ok(text) => Ok((text, guard.release())),
        Err(e) => Err(guard.fail(e).await),
    }
}

/// The LLM a preset asks for, if this run is using one that asks.
///
/// A preset that cannot be read is treated as a preset with no override: the
/// rewrite itself is about to fail on the same missing row inside
/// `build_llm_request`, with a message that names it, and failing here first
/// would replace that message with a vaguer one.
async fn preset_llm_binding(presets: &PresetRepo, preset_id: Option<&str>) -> Option<Binding> {
    presets
        .get(preset_id?)
        .await
        .ok()
        .flatten()
        .and_then(|preset| preset.llm_binding())
}

/// The LLM call and the optional conversation record — everything that is the
/// same whether the answer ends up replacing a selection or on the clipboard.
async fn run_completion(
    engines: &EngineRegistry,
    storage: ContentStorageOpts<'_>,
    binding: &Binding,
    input: &RewriteInput,
    llm_req: LlmRequest,
    action_id: i64,
) -> Result<String, String> {
    let engine_id = &binding.engine_id;
    let engine = engines
        .llm(engine_id)
        .ok_or_else(|| format!("no llm engine '{engine_id}'"))?;

    let response = engine.complete(llm_req).await.map_err(|e| e.to_string())?;

    record_llm_call(
        storage,
        action_id,
        "rewrite",
        engine_id,
        binding.model.clone(),
        binding.provider_ref.clone(),
        &input.source_text,
        &response.text,
        response.usage,
    )
    .await?;

    Ok(response.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::rewrite::RewriteMode;
    use kea_core::store::actions::ActionStatus;
    use kea_core::store::conversations::ConversationRepo;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopLlmEngine;
    use kea_platform::{ReplaceMode, TextIoError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
        replaced: Mutex<Option<String>>,
        /// How many times the selection was read. The palette path must never
        /// bump this: a second synthetic Cmd+C would go to KEA's own window,
        /// which is frontmost while the palette is up.
        captures: AtomicUsize,
    }

    impl FakeTextIo {
        fn with_selection(selection: &str) -> Self {
            Self {
                selection: selection.into(),
                replaced: Mutex::new(None),
                captures: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            Ok(self.selection.clone())
        }

        async fn replace_with_mode(
            &self,
            text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            *self.replaced.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    /// The four repos every test here builds, over two in-memory pools.
    struct Harness {
        engines: EngineRegistry,
        bindings: BindingRepo,
        actions: ActionRepo,
        presets: PresetRepo,
        overrides: PromptOverrideRepo,
        conversations: ConversationRepo,
    }

    impl Harness {
        async fn new() -> Self {
            let mut engines = EngineRegistry::default();
            engines.register_llm(Arc::new(NoopLlmEngine));
            let config_pool = open_pool("sqlite::memory:").await.unwrap();
            run_config_migrations(&config_pool).await.unwrap();
            let data_pool = open_pool("sqlite::memory:").await.unwrap();
            run_data_migrations(&data_pool).await.unwrap();
            Self {
                engines,
                bindings: BindingRepo::new(config_pool.clone()),
                actions: ActionRepo::new(data_pool.clone()),
                presets: PresetRepo::new(config_pool.clone()),
                overrides: PromptOverrideRepo::new(config_pool),
                conversations: ConversationRepo::new(data_pool),
            }
        }
    }

    #[test]
    fn rewrite_declares_llm_slot_and_command() {
        let f = RewriteFeature;
        assert_eq!(f.id(), "rewrite");
        assert_eq!(f.required_caps()[0].name, "llm");
        assert_eq!(f.required_caps()[0].kind, CapKind::Llm);
        let cmds = f.commands();
        // The selection rewrite stays first: it is the feature's headline
        // command and the one the Rewrite page's own hotkey row names.
        assert_eq!(cmds[0].id, "rewrite_selection");
        assert_eq!(cmds[0].title, "Rewrite Selection");
        assert!(cmds[0].default_accelerator.is_some());
    }

    #[tokio::test]
    async fn run_rewrite_calls_llm_and_textio() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo::with_selection("bad text"));

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
            &ProfileOverrides::default(),
        )
        .await
        .unwrap();

        assert!(out.contains("echo:"));
        assert!(out.contains("bad text"));
        assert_eq!(
            textio.replaced.lock().unwrap().as_deref(),
            Some(out.as_str())
        );

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "rewrite");
        assert_eq!(rows[0].command, "rewrite_selection");
        assert_eq!(rows[0].engine_id, "noop");
        assert_eq!(rows[0].status, ActionStatus::Ok);
    }

    #[tokio::test]
    async fn run_rewrite_translates_into_the_requested_language() {
        // Same seam as the test above, one mode along: the target reaches the
        // prompt (the noop engine echoes it back) and the result is written to
        // the selection, so nothing between the descriptor and TextIo drops it.
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo::with_selection("guten tag"));

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
                mode: RewriteMode::Translate,
                preset_id: None,
                custom_instruction: Some("de".into()),
            },
            &ProfileOverrides::default(),
        )
        .await
        .unwrap();

        assert!(out.contains("German"));
        assert!(out.contains("guten tag"));
        assert_eq!(
            textio.replaced.lock().unwrap().as_deref(),
            Some(out.as_str())
        );
    }

    #[tokio::test]
    async fn run_rewrite_records_conversation_when_storage_enabled() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo::with_selection("bad text"));

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
            &ProfileOverrides::default(),
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
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "bad text");
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert!(messages[1].content.contains("echo:"));
    }

    #[tokio::test]
    async fn run_rewrite_skips_conversation_when_storage_disabled() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo::with_selection("bad text"));

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
            &ProfileOverrides::default(),
            ContentStorageOpts::disabled(),
        )
        .await
        .unwrap();

        assert!(conversations.list_recent(1).await.unwrap().is_empty());
    }

    /// The palette's seam: it has already captured the selection (before its
    /// own window took focus) and passes it in. A capture here would fire a
    /// synthetic Cmd+C into KEA's own text field.
    #[tokio::test]
    async fn complete_rewrite_never_reads_the_selection() {
        let h = Harness::new().await;
        let textio = FakeTextIo::with_selection("MUST NOT BE READ");

        let (text, action_id) = complete_rewrite(
            &h.engines,
            &h.bindings,
            &h.actions,
            &h.presets,
            &h.overrides,
            PALETTE_COMMAND,
            &RewriteInput {
                source_text: "the selection the palette already had".into(),
                mode: RewriteMode::AskKea,
                preset_id: None,
                custom_instruction: Some("make it shorter".into()),
            },
            &ProfileOverrides::default(),
            ContentStorageOpts::default(),
        )
        .await
        .unwrap();

        assert_eq!(textio.captures.load(Ordering::SeqCst), 0);
        assert!(text.contains("make it shorter"), "{text}");
        assert!(
            textio.replaced.lock().unwrap().is_none(),
            "nothing delivered"
        );

        // The row is handed back OPEN: the caller has not delivered yet.
        let detail = h.actions.get(action_id).await.unwrap().unwrap();
        assert_eq!(detail.status, ActionStatus::Started);
        assert_eq!(detail.command, PALETTE_COMMAND);
    }

    /// An empty source is the palette's "ask KEA anything" case and must reach
    /// the engine as an instruction-only prompt, not as a rewrite of nothing.
    #[tokio::test]
    async fn complete_rewrite_asks_without_a_source() {
        let h = Harness::new().await;
        let (text, _) = complete_rewrite(
            &h.engines,
            &h.bindings,
            &h.actions,
            &h.presets,
            &h.overrides,
            PALETTE_COMMAND,
            &RewriteInput {
                source_text: String::new(),
                mode: RewriteMode::AskKea,
                preset_id: None,
                custom_instruction: Some("what is 9 factorial".into()),
            },
            &ProfileOverrides::default(),
            ContentStorageOpts::default(),
        )
        .await
        .unwrap();

        // The noop engine echoes the prompt back, so this reads the prompt.
        assert!(text.contains("what is 9 factorial"), "{text}");
        assert!(!text.contains("Source text:"), "{text}");
    }

    /// A failure before delivery closes the row itself — the caller is never
    /// handed a released id it does not know it owns.
    #[tokio::test]
    async fn complete_rewrite_closes_its_own_row_on_failure() {
        let h = Harness::new().await;
        let err = complete_rewrite(
            &h.engines,
            &h.bindings,
            &h.actions,
            &h.presets,
            &h.overrides,
            PALETTE_COMMAND,
            &RewriteInput {
                source_text: "hi".into(),
                // Ask KEA with no instruction cannot render its template.
                mode: RewriteMode::AskKea,
                preset_id: None,
                custom_instruction: None,
            },
            &ProfileOverrides::default(),
            ContentStorageOpts::default(),
        )
        .await
        .unwrap_err();

        assert!(err.contains("missing custom instruction"), "{err}");
        // The row never opened (the render fails before `actions.record`), so
        // nothing is left pending either way.
        assert!(h.actions.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn complete_rewrite_records_the_conversation_once() {
        let h = Harness::new().await;
        complete_rewrite(
            &h.engines,
            &h.bindings,
            &h.actions,
            &h.presets,
            &h.overrides,
            PALETTE_COMMAND,
            &RewriteInput {
                source_text: "some selected prose".into(),
                mode: RewriteMode::AskKea,
                preset_id: None,
                custom_instruction: Some("shorten".into()),
            },
            &ProfileOverrides::default(),
            ContentStorageOpts::enabled(&h.conversations),
        )
        .await
        .unwrap();

        let recent = h.conversations.list_recent(2).await.unwrap();
        assert_eq!(recent.len(), 1);
        let messages = h.conversations.list_messages(recent[0].id).await.unwrap();
        assert_eq!(messages[0].content, "some selected prose");
    }

    #[test]
    fn the_palette_and_ocr_commands_declare_defaults() {
        let cmds = RewriteFeature.commands();
        let ids: Vec<&str> = cmds.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&PALETTE_COMMAND));
        assert!(ids.contains(&OCR_COMMAND));
        // `resolve_accelerator` falls back to these, so a None here would
        // register an empty accelerator and the key would silently be dead.
        for cmd in &cmds {
            assert!(
                cmd.default_accelerator.is_some(),
                "{} has no default accelerator",
                cmd.id
            );
        }
        // Distinct, or `check_hotkey_collision` refuses the second one.
        let mut accels: Vec<&str> = cmds
            .iter()
            .filter_map(|c| c.default_accelerator.as_deref())
            .collect();
        accels.sort_unstable();
        let count = accels.len();
        accels.dedup();
        assert_eq!(accels.len(), count, "two commands share a default");
    }

    #[tokio::test]
    async fn post_process_failure_leaves_no_pending_row() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo::with_selection("bad text"));

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
            &ProfileOverrides::default(),
            ContentStorageOpts::enabled(&conversations),
        )
        .await
        .unwrap_err();

        assert!(
            err.contains("no such table"),
            "expected table-missing error, got: {err}"
        );

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActionStatus::Error);

        let detail = actions.get(rows[0].id).await.unwrap().unwrap();
        assert!(
            detail.error.is_some(),
            "action row should carry an error message"
        );
        assert!(
            detail
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no such table"),
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
    use kea_engines::http::{Auth, HttpClient, MultipartPart};
    use kea_engines::provider::{CredentialSource, ProviderConfig, ProviderConfigSource};
    use kea_engines::traits::EngineError;
    use kea_engines::OpenAiCompatibleLlmEngine;
    use kea_platform::{ReplaceMode, TextIo, TextIoError};
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

        async fn replace_with_mode(
            &self,
            text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
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
            auth: Auth<'_>,
            _body: serde_json::Value,
        ) -> Result<String, EngineError> {
            let bearer = match auth {
                Auth::Bearer(key) => key.to_string(),
                Auth::None => String::new(),
            };
            self.calls.lock().unwrap().push((url.to_string(), bearer));
            Ok(r#"{"choices":[{"message":{"content":"polished"}}]}"#.to_string())
        }

        async fn post_multipart(
            &self,
            _url: &str,
            _auth: Auth<'_>,
            _parts: Vec<MultipartPart>,
        ) -> Result<String, EngineError> {
            unreachable!("rewrite never uploads multipart")
        }

        async fn post_binary(
            &self,
            _url: &str,
            _auth: Auth<'_>,
            _body: serde_json::Value,
        ) -> Result<Vec<u8>, EngineError> {
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
            &ProfileOverrides::default(),
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
