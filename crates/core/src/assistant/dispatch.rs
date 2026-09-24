//! Running a chosen action, and deciding what a routed request becomes.
//!
//! **Only one path through this module reaches a handler.** A request may end
//! as words the user hears — an answer, a clarifying question, a report of
//! failure — or as exactly one invocation. [`RequestPlan`] is where that fork
//! is made once, so no caller has to re-derive it and get it subtly wrong; the
//! tests below assert that every non-invoking outcome really does invoke
//! nothing.
//!
//! Arguments are validated *here*, before a handler is chosen, so a handler
//! cannot be reached with input it did not ask for: holding a
//! [`ResolvedArgs`] is evidence the check happened.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::action::ActionSpec;
use super::args::{validate_args, ArgError, ResolvedArgs};
use super::registry::ActionRegistry;
use super::routing::RoutingOutcome;

/// What an action tells the user it read, when it read another application.
///
/// Present on the outcome rather than logged, because the requirement is that
/// the *user* is told — an action that reads someone's mail and sends it to a
/// provider must say so where they can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// What was read, in the user's terms: "the selected text in Mail".
    pub read: String,
    /// Whether the content left the machine.
    pub sent_externally: bool,
}

/// What running an action produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionOutcome {
    /// Text for the session to present and speak. `None` for an action whose
    /// effect is the point and which has nothing to say about it.
    pub text: Option<String>,
    pub disclosure: Option<Disclosure>,
}

/// Why an action did not run, or did not finish.
///
/// Separated by what the user can do about it. "Grant the permission" and "the
/// app isn't running" lead to different next steps, and collapsing them leaves
/// the session with one unhelpful sentence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionFailure {
    #[error("{permission} access is required")]
    PermissionRequired { permission: String },
    #[error("{what} is unavailable")]
    Unavailable { what: String },
    #[error("no handler is installed for {id:?}")]
    NoHandler { id: String },
    #[error("{0}")]
    Failed(String),
}

/// The implementation behind one catalog entry.
#[async_trait]
pub trait ActionHandler: Send + Sync {
    /// Must match a [`ActionSpec::id`] in the registry this dispatcher serves.
    fn id(&self) -> &'static str;
    async fn run(&self, args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure>;
}

/// What the session should do with a routed request.
///
/// Exactly one variant dispatches. The other three are things the user hears.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestPlan {
    /// Speak and show this answer. The default outcome.
    Answer { text: String },
    /// Run this action with these arguments.
    Invoke {
        spec: &'static ActionSpec,
        args: ResolvedArgs,
    },
    /// Ask the user something before anything runs.
    Clarify { question: String },
    /// Tell the user it could not be done. Nothing ran.
    Fail { message: String },
}

impl RequestPlan {
    /// The action this plan will run, if any. `None` for every plan that only
    /// produces words.
    pub fn action(&self) -> Option<&'static ActionSpec> {
        match self {
            RequestPlan::Invoke { spec, .. } => Some(spec),
            _ => None,
        }
    }
}

/// Turn a routing decision into a plan, validating arguments on the way.
///
/// This is where ambiguity and a missing argument become questions rather than
/// invocations. Both are cases where the model produced something coherent but
/// under-specified, and guessing on the user's behalf is exactly the behaviour
/// that makes an assistant feel unsafe to use.
pub fn plan(outcome: RoutingOutcome, registry: &ActionRegistry) -> RequestPlan {
    match outcome {
        RoutingOutcome::Answer { text } => RequestPlan::Answer { text },

        RoutingOutcome::Ambiguous { candidates } => {
            let titles: Vec<&str> = candidates
                .iter()
                .filter_map(|id| registry.get(id).map(|s| s.title))
                .collect();
            RequestPlan::Clarify {
                question: format!("Did you mean {}?", join_or(&titles)),
            }
        }

        RoutingOutcome::Unparseable { reason } => RequestPlan::Fail {
            message: format!("I couldn't work out what to do — {reason}."),
        },

        RoutingOutcome::Action { spec, raw_args } => match validate_args(spec, &raw_args) {
            Ok(args) => RequestPlan::Invoke { spec, args },

            // The user simply did not say which one. Asking is the whole
            // remedy, and it costs them one word.
            Err(ArgError::MissingRequired { name }) => RequestPlan::Clarify {
                question: format!("Which {name}?"),
            },

            // The model was told the type and produced something else, so
            // asking the user to rephrase would not help — this is a fault on
            // our side of the boundary and is reported as one.
            Err(e) => RequestPlan::Fail {
                message: format!("I couldn't use that request — {e}."),
            },
        },
    }
}

fn join_or(items: &[&str]) -> String {
    match items {
        [] => "something else".to_string(),
        [one] => one.to_string(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

/// Runs actions on behalf of the session.
#[derive(Default)]
pub struct Dispatcher {
    handlers: BTreeMap<&'static str, Arc<dyn ActionHandler>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, handler: Arc<dyn ActionHandler>) {
        self.handlers.insert(handler.id(), handler);
    }

    /// Run `spec` with already-validated `args`.
    ///
    /// A catalog entry with no installed handler is a wiring bug, reported as
    /// [`ActionFailure::NoHandler`] rather than silently succeeding — the user
    /// must not be told something happened when nothing did.
    pub async fn invoke(
        &self,
        spec: &ActionSpec,
        args: &ResolvedArgs,
    ) -> Result<ActionOutcome, ActionFailure> {
        let Some(handler) = self.handlers.get(spec.id) else {
            return Err(ActionFailure::NoHandler {
                id: spec.id.to_string(),
            });
        };
        handler.run(args).await
    }

    pub fn has_handler(&self, id: &str) -> bool {
        self.handlers.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::registry::{OPEN_APP, READ_FOCUSED};
    use serde_json::json;

    struct Succeeds;
    #[async_trait]
    impl ActionHandler for Succeeds {
        fn id(&self) -> &'static str {
            "read_focused"
        }
        async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
            Ok(ActionOutcome {
                text: Some("a login form".into()),
                disclosure: Some(Disclosure {
                    read: "the frontmost window".into(),
                    sent_externally: true,
                }),
            })
        }
    }

    struct Fails;
    #[async_trait]
    impl ActionHandler for Fails {
        fn id(&self) -> &'static str {
            "open_app"
        }
        async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
            Err(ActionFailure::Unavailable {
                what: "Mail".into(),
            })
        }
    }

    fn dispatcher() -> Dispatcher {
        let mut d = Dispatcher::new();
        d.register(Arc::new(Succeeds));
        d.register(Arc::new(Fails));
        d
    }

    fn reg() -> ActionRegistry {
        ActionRegistry::default()
    }

    #[tokio::test]
    async fn a_succeeding_action_returns_its_outcome() {
        let out = dispatcher()
            .invoke(&READ_FOCUSED, &ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("a login form"));
        assert!(out.disclosure.unwrap().sent_externally);
    }

    #[tokio::test]
    async fn a_failing_action_reports_why() {
        let err = dispatcher()
            .invoke(&OPEN_APP, &ResolvedArgs::default())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            ActionFailure::Unavailable {
                what: "Mail".into()
            }
        );
    }

    #[tokio::test]
    async fn an_action_with_no_handler_fails_rather_than_pretending() {
        let d = Dispatcher::new();
        let err = d
            .invoke(&READ_FOCUSED, &ResolvedArgs::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ActionFailure::NoHandler { .. }));
    }

    #[test]
    fn an_answer_plans_to_words_and_invokes_nothing() {
        let p = plan(
            RoutingOutcome::Answer {
                text: "Paris.".into(),
            },
            &reg(),
        );
        assert_eq!(p, RequestPlan::Answer { text: "Paris.".into() });
        assert!(p.action().is_none());
    }

    #[test]
    fn ambiguity_asks_and_invokes_nothing() {
        let p = plan(
            RoutingOutcome::Ambiguous {
                candidates: vec!["open_app", "start_meeting"],
            },
            &reg(),
        );
        match &p {
            RequestPlan::Clarify { question } => {
                assert!(question.contains("Open an application"));
                assert!(question.contains("or"));
            }
            other => panic!("expected a question, got {other:?}"),
        }
        assert!(p.action().is_none(), "ambiguity must not reach a handler");
    }

    #[test]
    fn a_missing_required_argument_asks_and_invokes_nothing() {
        let p = plan(
            RoutingOutcome::Action {
                spec: &OPEN_APP,
                raw_args: json!({}),
            },
            &reg(),
        );
        match &p {
            RequestPlan::Clarify { question } => assert!(question.contains("app")),
            other => panic!("expected a question, got {other:?}"),
        }
        assert!(
            p.action().is_none(),
            "a missing argument must not reach a handler"
        );
    }

    #[test]
    fn a_type_mismatch_fails_and_invokes_nothing() {
        let p = plan(
            RoutingOutcome::Action {
                spec: &OPEN_APP,
                raw_args: json!({"app": 7}),
            },
            &reg(),
        );
        assert!(matches!(p, RequestPlan::Fail { .. }));
        assert!(p.action().is_none());
    }

    #[test]
    fn an_unparseable_routing_fails_and_invokes_nothing() {
        let p = plan(
            RoutingOutcome::Unparseable {
                reason: "no JSON".into(),
            },
            &reg(),
        );
        assert!(matches!(p, RequestPlan::Fail { .. }));
        assert!(p.action().is_none());
    }

    #[test]
    fn a_complete_request_plans_an_invocation() {
        let p = plan(
            RoutingOutcome::Action {
                spec: &OPEN_APP,
                raw_args: json!({"app": "Mail"}),
            },
            &reg(),
        );
        match &p {
            RequestPlan::Invoke { spec, args } => {
                assert_eq!(spec.id, "open_app");
                assert_eq!(args.text("app"), Some("Mail"));
            }
            other => panic!("expected an invocation, got {other:?}"),
        }
    }

    #[test]
    fn only_the_invoke_plan_ever_names_an_action() {
        // The property the session depends on: three of four outcomes are
        // words, and no amount of downstream code can turn them into a run.
        let plans = [
            plan(RoutingOutcome::Answer { text: "x".into() }, &reg()),
            plan(
                RoutingOutcome::Ambiguous {
                    candidates: vec!["open_app", "start_meeting"],
                },
                &reg(),
            ),
            plan(
                RoutingOutcome::Unparseable {
                    reason: "x".into(),
                },
                &reg(),
            ),
        ];
        for p in plans {
            assert!(p.action().is_none(), "{p:?} must not invoke");
        }
    }
}
