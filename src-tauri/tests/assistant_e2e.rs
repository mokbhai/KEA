//! One spoken request, all the way from a transcript to what the user saw,
//! heard, and can find in History afterwards.
//!
//! **These drive the real decision path with fake engines, not a real window.**
//! The alternative was to boot the Tauri app and press the binding. That was
//! rejected on two counts. It asserts less — a window that appears proves
//! nothing about which action ran — and it is unreachable anyway: `kea-app` is
//! a binary with no library target, so nothing in `src-tauri/src` can be named
//! from an integration test. What *is* reachable is every seam the session is
//! built out of, and those are the parts that can be wrong:
//! [`route`] against a real `LlmEngine`, [`plan`], [`Dispatcher`] over real
//! handlers, the `actions` ledger against a real SQLite database, [`Session`],
//! and the real synthesis path in `kea_features::tts` down to a fake engine
//! that records the exact string it was asked to speak.
//!
//! [`Assistant::ask`] below composes those in the order
//! `src-tauri/src/assistant.rs` composes them. It is deliberately the thinnest
//! thing that can be: every rule these tests are about — what routing does with
//! a malformed reply, what the dispatcher will and will not run, what the
//! ledger records — lives under the seams, not in the composition.
//!
//! The load-bearing one is [`a_malformed_routing_reply_invokes_nothing_and_says_so`].
//! The JSON contract of `design.md` buys uniform routing across every engine a
//! user might bind, at the cost of accuracy against models tuned for native
//! tool-calling, and the whole trade rests on one property: **degradation is a
//! refusal, never a wrong action.** That is what the table in that test pins
//! down, and what the tripwire dispatcher makes impossible to pass by accident.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kea_core::assistant::routing::route;
use kea_core::assistant::{
    plan, ActionFailure, ActionHandler, ActionOutcome, ActionRegistry, ActionSpec,
    AssistantSettings, Disclosure, Dispatcher, RequestPlan, ResolvedArgs, Session, SessionEvent,
    SessionState, CATALOG,
};
use kea_core::store::actions::{ActionRepo, ActionStatus, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
use kea_core::TtsSettings;
use kea_engines::traits::{
    AudioPcm, EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, TtsEngine, TtsOpts,
};
use kea_engines::EngineRegistry;
use kea_features::{ActionGuard, ASSISTANT_FEATURE_ID};
use kea_platform::screen::{ScreenError, ScreenReader};
use kea_platform::textio::{ReplaceMode, TextIo, TextIoError};

// ===========================================================================
// Fakes
// ===========================================================================

/// The routing engine, answering as a function of what was said.
///
/// A function rather than a single canned body because the interesting
/// scripts are about *which* reply a given utterance draws — and because the
/// refusal table below runs the same pipeline against a dozen different
/// malformed bodies without a dozen engine types.
struct ScriptedLlm {
    reply: Box<dyn Fn(&str) -> String + Send + Sync>,
}

/// The utterance as `build_routing_prompt` writes it into the prompt.
///
/// Reading it back out is what lets the script above be written in terms of
/// what the user said rather than in terms of prompt text.
fn utterance_of(prompt: &str) -> String {
    prompt
        .rsplit_once("User said: ")
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_default()
}

#[async_trait]
impl LlmEngine for ScriptedLlm {
    fn id(&self) -> &str {
        "scripted"
    }
    fn capabilities(&self) -> EngineCaps {
        EngineCaps { models: vec![] }
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        Ok(LlmResponse::untracked((self.reply)(&utterance_of(
            &req.prompt,
        ))))
    }
}

/// A voice that records the exact string it was handed.
///
/// Registered in a real [`EngineRegistry`] and reached through
/// `kea_features::tts::speak_text_for`, so "the answer was sent to the TTS
/// engine" is a claim about the shipping synthesis path — slot resolution,
/// binding and voice settings included — rather than about a stub standing in
/// for it.
struct RecordingTts {
    said: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl TtsEngine for RecordingTts {
    fn id(&self) -> &str {
        "recording-tts"
    }
    fn capabilities(&self) -> EngineCaps {
        EngineCaps { models: vec![] }
    }
    async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        self.said.lock().unwrap().push(text.to_string());
        Ok(AudioPcm {
            samples: vec![0.0; 16],
            sample_rate_hz: 24_000,
        })
    }
}

/// What the session put in front of the user, in the order it did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    State(String),
    Answer {
        request: String,
        text: String,
        disclosure: Option<Disclosure>,
    },
    /// The request could not be completed, and this is what the user was told.
    Failure(String),
}

#[derive(Default)]
struct RecordingSurface {
    shown: Mutex<Vec<Shown>>,
}

impl RecordingSurface {
    fn push(&self, entry: Shown) {
        self.shown.lock().unwrap().push(entry);
    }

    fn shown(&self) -> Vec<Shown> {
        self.shown.lock().unwrap().clone()
    }

    fn answers(&self) -> Vec<Shown> {
        self.shown()
            .into_iter()
            .filter(|s| matches!(s, Shown::Answer { .. }))
            .collect()
    }

    /// The answer text, as displayed.
    fn displayed(&self) -> Vec<String> {
        self.shown()
            .into_iter()
            .filter_map(|s| match s {
                Shown::Answer { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn failures(&self) -> Vec<String> {
        self.shown()
            .into_iter()
            .filter_map(|s| match s {
                Shown::Failure(message) => Some(message),
                _ => None,
            })
            .collect()
    }
}

/// A handler that must never run, installed over every action a test does not
/// expect to see.
///
/// It succeeds rather than failing, on purpose. A tripwire that returned an
/// error would surface as a failed request, which is exactly what the refusal
/// tests expect to see anyway — so a wrongly invoked action would hide inside
/// the assertion that the request failed. Succeeding quietly means the only
/// thing that can catch it is the invocation list, which is the thing under
/// test.
struct Tripwire {
    id: &'static str,
    tripped: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ActionHandler for Tripwire {
    fn id(&self) -> &'static str {
        self.id
    }
    async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
        self.tripped.lock().unwrap().push(self.id.to_string());
        Ok(ActionOutcome {
            text: Some(format!("{} ran", self.id)),
            disclosure: None,
        })
    }
}

/// A frontmost application with a selection in it, and a count of how often it
/// was read.
struct FakeTextIo {
    selection: String,
    reads: Arc<Mutex<usize>>,
}

#[async_trait]
impl TextIo for FakeTextIo {
    async fn capture_selection(&self) -> Result<String, TextIoError> {
        *self.reads.lock().unwrap() += 1;
        Ok(self.selection.clone())
    }
    async fn replace_with_mode(&self, _text: &str, _mode: ReplaceMode) -> Result<(), TextIoError> {
        // Nothing the assistant can reach writes text back, which is the
        // registry's no-side-effects rule. Reaching this is a test failure in
        // itself.
        panic!("the assistant must not write into the focused application");
    }
}

/// Screen reading, refused — the tier `read_focused` falls through to only
/// when Accessibility came back with nothing.
struct RefusingScreen;

#[async_trait]
impl ScreenReader for RefusingScreen {
    async fn read_focused_window(&self) -> Result<String, ScreenError> {
        Err(ScreenError::Unavailable("no screen in a test".into()))
    }
}

// ===========================================================================
// The pipeline under test
// ===========================================================================

/// Everything one request runs against, owned together.
struct Assistant {
    registry: ActionRegistry,
    llm: ScriptedLlm,
    llm_binding: Binding,
    dispatcher: Dispatcher,
    actions: ActionRepo,
    engines: EngineRegistry,
    bindings: BindingRepo,
    tts_settings: TtsSettings,
    settings: AssistantSettings,
    surface: RecordingSurface,
    spoken: Arc<Mutex<Vec<String>>>,
    tripped: Arc<Mutex<Vec<String>>>,
}

impl Assistant {
    /// An assistant whose router answers `reply` for a given utterance, with a
    /// tripwire installed over the whole catalog.
    ///
    /// Tripwires first, so a test that registers a real handler overwrites
    /// exactly one of them and every other action stays armed.
    async fn new(reply: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
        let config = open_pool("sqlite::memory:").await.expect("config pool");
        run_config_migrations(&config).await.expect("config schema");
        let data = open_pool("sqlite::memory:").await.expect("data pool");
        run_data_migrations(&data).await.expect("data schema");

        let spoken = Arc::new(Mutex::new(Vec::new()));
        let mut engines = EngineRegistry::default();
        engines.register_tts(Arc::new(RecordingTts {
            said: spoken.clone(),
        }));

        let bindings = BindingRepo::new(config);
        bindings
            .set(
                ASSISTANT_FEATURE_ID,
                "tts",
                Binding {
                    engine_id: "recording-tts".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .expect("the assistant has a voice bound");

        let tripped = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = Dispatcher::new();
        for spec in CATALOG {
            dispatcher.register(Arc::new(Tripwire {
                id: spec.id,
                tripped: tripped.clone(),
            }));
        }

        Self {
            registry: ActionRegistry::default(),
            llm: ScriptedLlm {
                reply: Box::new(reply),
            },
            llm_binding: Binding {
                engine_id: "scripted".into(),
                model: None,
                provider_ref: None,
            },
            dispatcher,
            actions: ActionRepo::new(data),
            engines,
            bindings,
            tts_settings: TtsSettings::default(),
            settings: AssistantSettings::default(),
            surface: RecordingSurface::default(),
            spoken,
            tripped,
        }
    }

    /// Install a real handler in place of that action's tripwire.
    fn handling(mut self, handler: Arc<dyn ActionHandler>) -> Self {
        self.dispatcher.register(handler);
        self
    }

    /// One turn: what the user said, through routing, to what they saw and
    /// heard.
    ///
    /// The order mirrors `src-tauri/src/assistant.rs`: the request is recorded
    /// before routing so it is visible alongside whatever happens next, the
    /// answer reaches the surface before speech is attempted, and the session
    /// only learns of an answer once there is one.
    async fn ask(&self, session: &mut Session, utterance: &str) -> Result<(), String> {
        session.set_request(utterance);
        session.apply(SessionEvent::RequestEnded);
        self.note_state(session);

        let outcome = match route(
            &self.llm,
            &self.llm_binding,
            &self.registry,
            utterance,
            session.history(),
        )
        .await
        {
            Ok(outcome) => outcome,
            // An unreachable provider is a different thing from one that
            // answered badly, and both invoke nothing.
            Err(error) => return Err(self.fail(session, error.to_string())),
        };

        match plan(outcome, &self.registry) {
            RequestPlan::Answer { text } => self.present(session, utterance, text, None).await,

            // A question, not a failure: the assistant understood enough to
            // ask, and nothing ran.
            RequestPlan::Clarify { question } => {
                self.present(session, utterance, question, None).await
            }

            RequestPlan::Fail { message } => Err(self.fail(session, message)),

            RequestPlan::Invoke { spec, args } => match self.invoke_recorded(spec, &args).await {
                Ok(ActionOutcome { text, disclosure }) => {
                    let answer = text.unwrap_or_else(|| format!("Done: {}.", spec.title));
                    self.present(session, utterance, answer, disclosure).await
                }
                Err(message) => Err(self.fail(session, message)),
            },
        }
    }

    /// Run the action and write the ledger row that says it happened.
    async fn invoke_recorded(
        &self,
        spec: &'static ActionSpec,
        args: &ResolvedArgs,
    ) -> Result<ActionOutcome, String> {
        let id = self
            .actions
            .record(NewAction {
                feature_id: ASSISTANT_FEATURE_ID.to_string(),
                command: spec.id.to_string(),
                engine_id: self.llm_binding.engine_id.clone(),
                model: self.llm_binding.model.clone(),
                provider_ref: self.llm_binding.provider_ref.clone(),
            })
            .await
            .map_err(|e| e.to_string())?;
        let guard = ActionGuard::new(&self.actions, id, ASSISTANT_FEATURE_ID);

        match self.dispatcher.invoke(spec, args).await {
            Ok(outcome) => {
                guard.succeed().await;
                Ok(outcome)
            }
            Err(error) => Err(guard.fail(error).await),
        }
    }

    async fn present(
        &self,
        session: &mut Session,
        request: &str,
        answer: String,
        disclosure: Option<Disclosure>,
    ) -> Result<(), String> {
        if self.settings.show_answers {
            self.surface.push(Shown::Answer {
                request: request.to_string(),
                text: answer.clone(),
                disclosure,
            });
        }

        let spoken = if self.settings.speak_answers {
            match kea_features::tts::speak_text_for(
                &self.engines,
                &self.bindings,
                &self.tts_settings,
                ASSISTANT_FEATURE_ID,
                &answer,
            )
            .await
            {
                Ok(_pcm) => true,
                // Speech is attempted, never required: the text stays up.
                Err(error) => {
                    self.surface.push(Shown::Failure(error));
                    false
                }
            }
        } else {
            false
        };

        session.apply(SessionEvent::Answered {
            text: answer,
            spoken,
        });
        session.apply(SessionEvent::SpeechFinished);
        self.note_state(session);
        Ok(())
    }

    /// Tell the user it could not be done, and leave the session where it can
    /// be retried.
    fn fail(&self, session: &mut Session, message: String) -> String {
        self.surface.push(Shown::Failure(message.clone()));
        session.apply(SessionEvent::Failed {
            message: message.clone(),
        });
        self.note_state(session);
        message
    }

    fn note_state(&self, session: &Session) {
        self.surface.push(Shown::State(state_name(session.state())));
    }

    /// The ledger, newest first.
    async fn ledger(&self) -> Vec<kea_core::store::actions::ActionRow> {
        self.actions
            .recent(20)
            .await
            .expect("the ledger is readable")
    }

    fn spoken(&self) -> Vec<String> {
        self.spoken.lock().unwrap().clone()
    }

    /// The actions that ran when nothing should have.
    fn tripped(&self) -> Vec<String> {
        self.tripped.lock().unwrap().clone()
    }
}

fn state_name(state: &SessionState) -> String {
    match state {
        SessionState::Listening => "listening".into(),
        SessionState::Processing => "processing".into(),
        SessionState::Presenting { .. } => "presenting".into(),
        SessionState::Failed { .. } => "failed".into(),
    }
}

fn answers_with(text: &'static str) -> impl Fn(&str) -> String + Send + Sync + 'static {
    move |_utterance: &str| format!(r#"{{"answer": "{text}"}}"#)
}

fn always(body: &'static str) -> impl Fn(&str) -> String + Send + Sync + 'static {
    move |_utterance: &str| body.to_string()
}

// ===========================================================================
// 9.2 — a spoken answer
// ===========================================================================

/// Task 9.2. The default outcome of the whole feature: a question that no
/// action covers is answered, the one answer is both shown and spoken, and
/// nothing is invoked on the user's behalf.
#[tokio::test]
async fn a_question_is_answered_in_one_text_that_is_both_shown_and_spoken() {
    let assistant = Assistant::new(answers_with("Paris.")).await;
    let mut session = Session::new();

    assistant
        .ask(&mut session, "what is the capital of france")
        .await
        .expect("answering is a success, not a routing failure");

    assert_eq!(
        assistant.surface.displayed(),
        vec!["Paris."],
        "the answer is displayed"
    );
    assert_eq!(
        assistant.spoken(),
        vec!["Paris."],
        "the same answer reaches the speech engine"
    );
    assert_eq!(
        assistant.surface.displayed(),
        assistant.spoken(),
        "shown and spoken are one text: a brief spoken version diverging from a \
         longer written one is two answers that can disagree"
    );
    assert_eq!(
        session.state(),
        &SessionState::Presenting { speaking: false },
        "playback was awaited, so the answer is on screen and no longer speaking"
    );
}

/// Task 9.2. The other half of the claim, and the one a passing answer could
/// otherwise hide: answering must reach no handler and leave no invocation
/// behind for the user to find in History.
#[tokio::test]
async fn answering_invokes_no_action_and_leaves_no_invocation_in_the_ledger() {
    let assistant = Assistant::new(answers_with("Rust is a programming language.")).await;
    let mut session = Session::new();

    assistant
        .ask(&mut session, "what is rust")
        .await
        .expect("the request completes");

    assert!(
        assistant.tripped().is_empty(),
        "an answer must reach no handler; these ran: {:?}",
        assistant.tripped()
    );
    assert!(
        assistant.ledger().await.is_empty(),
        "answering is not a catalog entry, so it opens no ledger row"
    );
}

/// Task 9.2. Speech is a channel, not a prerequisite — with nothing bound to
/// speak through, the answer is still displayed and the user is told why they
/// did not hear it.
#[tokio::test]
async fn an_answer_with_no_usable_voice_is_still_displayed_and_the_silence_explained() {
    let mut assistant = Assistant::new(answers_with("Paris.")).await;
    // No TTS engine at all, which is what an unresolvable slot looks like from
    // the session's side.
    assistant.engines = EngineRegistry::default();
    let mut session = Session::new();

    assistant
        .ask(&mut session, "what is the capital of france")
        .await
        .expect("a voiceless answer is still an answer");

    assert_eq!(assistant.surface.displayed(), vec!["Paris."]);
    assert!(
        assistant.spoken().is_empty(),
        "there was no engine to speak through"
    );
    assert_eq!(
        assistant.surface.failures().len(),
        1,
        "the user is told the answer could not be spoken: {:?}",
        assistant.surface.failures()
    );
    assert!(
        matches!(
            session.state(),
            SessionState::Presenting { speaking: false }
        ),
        "a voice that failed must not take the answer down with it"
    );
}

// ===========================================================================
// 9.3 — a completed action
// ===========================================================================

/// Task 9.3. A request that names an action runs it once, records what
/// happened where the user can review it, and presents what came back —
/// including the disclosure that their selection was read and sent onward.
#[tokio::test]
async fn a_routed_action_runs_once_is_recorded_as_completed_and_presents_its_result() {
    let reads = Arc::new(Mutex::new(0usize));
    let assistant = Assistant::new(always(r#"{"action": "read_focused"}"#))
        .await
        .handling(Arc::new(kea_features::assistant::ReadFocused::new(
            Arc::new(FakeTextIo {
                selection: "Error: the disk is full".into(),
                reads: reads.clone(),
            }),
            Arc::new(RefusingScreen),
        )));
    let mut session = Session::new();

    assistant
        .ask(&mut session, "what does this say")
        .await
        .expect("the action completes");

    assert_eq!(*reads.lock().unwrap(), 1, "the action ran exactly once");
    assert!(
        assistant.tripped().is_empty(),
        "one request resolves to at most one action; these also ran: {:?}",
        assistant.tripped()
    );

    let ledger = assistant.ledger().await;
    assert_eq!(ledger.len(), 1, "one invocation, one row");
    let row = &ledger[0];
    assert_eq!(row.feature_id, ASSISTANT_FEATURE_ID);
    assert_eq!(
        row.command, "read_focused",
        "History speaks the same vocabulary as the registry and the router"
    );
    assert_eq!(
        row.engine_id, "scripted",
        "the row names the engine that decided, which is the only honest answer \
         to what caused this"
    );
    assert_eq!(
        row.status,
        ActionStatus::Ok,
        "a completed run is recorded as completed"
    );

    assert_eq!(
        assistant.surface.answers(),
        vec![Shown::Answer {
            request: "what does this say".into(),
            text: "Error: the disk is full".into(),
            disclosure: Some(Disclosure {
                read: "your selected text".into(),
                sent_externally: true,
            }),
        }],
        "the request text, the result, and what was read and sent are all in \
         front of the user"
    );
    assert_eq!(
        assistant.spoken(),
        vec!["Error: the disk is full"],
        "an action's result is delivered the same way an answer is"
    );
}

/// Task 9.3, the failing half. An action that could not run is recorded as a
/// failure rather than a completion, so History cannot claim something
/// happened that did not.
#[tokio::test]
async fn an_action_that_could_not_run_is_recorded_as_a_failure_and_reported() {
    let assistant = Assistant::new(always(r#"{"action": "read_focused"}"#))
        .await
        .handling(Arc::new(kea_features::assistant::ReadFocused::new(
            // Nothing selected, no focused text, no screen: every tier refuses.
            Arc::new(FakeTextIo {
                selection: "   ".into(),
                reads: Arc::new(Mutex::new(0)),
            }),
            Arc::new(RefusingScreen),
        )));
    let mut session = Session::new();

    let error = assistant
        .ask(&mut session, "what does this say")
        .await
        .expect_err("an unavailable target is not a completed request");

    assert!(
        error.contains("unavailable"),
        "the user is told what was missing, got {error:?}"
    );
    let ledger = assistant.ledger().await;
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger[0].status,
        ActionStatus::Error,
        "a run that failed is recorded as failed"
    );
    assert!(
        assistant.surface.displayed().is_empty(),
        "nothing may be presented as a result when there was none"
    );
}

// ===========================================================================
// 9.4 — the refusal path
// ===========================================================================

/// Task 9.4, and the property the JSON-contract design rests on.
///
/// Routing prompts for JSON rather than using a provider's tool API so that a
/// hosted model, a local server and an arbitrary OpenAI-compatible endpoint
/// route identically. That trade is only defensible because a reply this
/// pipeline cannot interpret invokes **nothing** — the cost of a bad completion
/// is a request the user repeats, never an action they did not ask for.
///
/// Every row below is a way a model can degrade, including the one that is not
/// malformed at all: a perfectly well-formed object naming an action outside
/// the registry. An invented name is not a lesser fault than broken syntax, and
/// it is the one a naive parser would happily dispatch.
#[tokio::test]
async fn a_malformed_routing_reply_invokes_nothing_and_says_so() {
    let degradations: &[(&str, &'static str)] = &[
        (
            "prose with no JSON at all",
            "Sure — I'll open Mail for you!",
        ),
        (
            "JSON that does not parse",
            r#"{"action": "open_app", "args":"#,
        ),
        (
            "an action outside the registry",
            r#"{"action": "delete_all_mail", "args": {}}"#,
        ),
        (
            "an object naming nothing we recognise",
            r#"{"thoughts": "the user wants their mail opened"}"#,
        ),
        ("an empty answer", r#"{"answer": "   "}"#),
        (
            "ambiguity over names that do not exist",
            r#"{"ambiguous": ["send_email", "delete_file"]}"#,
        ),
    ];

    for &(what, body) in degradations {
        let assistant = Assistant::new(always(body)).await;
        let mut session = Session::new();

        let error = match assistant.ask(&mut session, "open mail").await {
            Ok(()) => panic!("{what}: a reply we cannot read must not resolve to anything"),
            Err(error) => error,
        };

        assert!(
            assistant.tripped().is_empty(),
            "{what}: nothing may be invoked, but these ran: {:?}",
            assistant.tripped()
        );
        assert!(
            assistant.ledger().await.is_empty(),
            "{what}: a refusal must not even open a ledger row — a row claims \
             something was attempted on the user's behalf"
        );
        assert!(
            assistant.spoken().is_empty(),
            "{what}: nothing was decided, so there is nothing to read aloud"
        );
        assert!(
            assistant.surface.displayed().is_empty(),
            "{what}: a refusal must not be dressed up as an answer"
        );
        assert_eq!(
            assistant.surface.failures(),
            vec![error],
            "{what}: the failure is reported to the user, not swallowed"
        );
        assert!(
            matches!(session.state(), SessionState::Failed { .. }),
            "{what}: the session says it failed, got {:?}",
            session.state()
        );
    }
}

/// Task 9.4. Naming the thing it would not do is the difference between a
/// refusal the user can act on and a shrug: "I can't delete mail" tells them
/// the request was understood and is out of scope, where a bare failure reads
/// as a broken assistant.
#[tokio::test]
async fn a_refusal_names_what_it_could_not_do() {
    let assistant = Assistant::new(always(r#"{"action": "delete_all_mail"}"#)).await;
    let mut session = Session::new();

    let error = assistant
        .ask(&mut session, "delete all my mail")
        .await
        .expect_err("an invented action is refused");

    assert!(
        error.contains("delete_all_mail"),
        "the refusal names the action it would not run, got {error:?}"
    );
}

/// Task 9.4. A failed turn does not end the conversation: the session stays
/// where the user can rephrase without activating the assistant again, and the
/// turn they never got an answer to is not carried into the next prompt.
#[tokio::test]
async fn a_failed_turn_leaves_the_session_open_to_be_retried() {
    // The first utterance degrades; the rephrased one routes to an answer, so
    // one assistant serves both turns exactly as one session would.
    let assistant = Assistant::new(|utterance: &str| {
        if utterance.contains("thingy") {
            "no idea what you mean".to_string()
        } else {
            r#"{"answer": "It's 4pm."}"#.to_string()
        }
    })
    .await;
    let mut session = Session::new();

    assistant
        .ask(&mut session, "what about the thingy")
        .await
        .expect_err("the first turn fails");
    assert!(
        matches!(session.state(), SessionState::Failed { .. }),
        "the failure is visible"
    );
    assert!(
        session.history().is_empty(),
        "a turn that was never answered must not become context — it invites \
         the model to answer the old question instead"
    );

    session.apply(SessionEvent::FollowUpStarted);
    assert_eq!(
        session.state(),
        &SessionState::Listening,
        "the session is still open and listening for the rephrase"
    );

    assistant
        .ask(&mut session, "what time is it")
        .await
        .expect("the retry succeeds without re-activating the assistant");

    assert_eq!(assistant.surface.displayed(), vec!["It's 4pm."]);
    assert_eq!(
        session.history(),
        [("what time is it".to_string(), "It's 4pm.".to_string())],
        "only the answered turn is context for what comes next"
    );
    assert!(
        assistant.tripped().is_empty(),
        "neither turn invoked anything"
    );
}
