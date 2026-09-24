//! Turning what the user said into an answer, or into one action with arguments.
//!
//! **Answering is the default, not the fallback.** The assistant is an answer
//! agent that happens to have tools; a request matching no action is a success,
//! and the router is told so explicitly. The opposite framing — route first,
//! answer when routing fails — produces a model that reaches for the nearest
//! action rather than saying something useful, which is the failure mode that
//! makes assistants annoying.
//!
//! **The contract is JSON in the completion body, not a provider tool API.**
//! `LlmEngine::complete` is `String -> String` and the engine registry spans
//! hosted providers, local servers and arbitrary OpenAI-compatible endpoints.
//! Native tool-calling would make routing quality depend on which engine the
//! user happened to bind, and would leave local models — the ones someone
//! running an assistant that reads their screen is most likely to choose — on a
//! degraded path. A JSON contract routes identically everywhere, and its
//! failure mode is safe: anything unparseable invokes nothing.
//!
//! Every outcome below that is not [`RoutingOutcome::Action`] reaches the user
//! as words and touches nothing.

use kea_engines::traits::{EngineError, LlmEngine, LlmRequest};

use super::action::ActionSpec;
use super::registry::ActionRegistry;
use crate::store::bindings::Binding;

/// How many words an answer may run to.
///
/// This is a correctness constraint, not a style preference. The answer is
/// spoken, speech is linear, and a listener cannot skim — so a long answer is
/// not merely verbose, it is unusable, and the user's only recourse is to
/// interrupt. Enforced by asking for it up front rather than truncating after,
/// because a truncated answer is a wrong answer read aloud confidently.
pub const ANSWER_WORD_LIMIT: usize = 60;

/// What the router decided.
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingOutcome {
    /// Answer the user directly. The default and most common outcome.
    Answer { text: String },
    /// Invoke exactly one action. `args` is still raw — it has not been checked
    /// against the action's schema yet, which is [`super::args::validate_args`].
    Action {
        spec: &'static ActionSpec,
        raw_args: serde_json::Value,
    },
    /// More than one action plausibly fits. The session asks rather than picks.
    Ambiguous { candidates: Vec<&'static str> },
    /// The completion was not a routing decision at all.
    ///
    /// Also what a well-formed object naming an action outside the catalog
    /// becomes: an invented action name is not a lesser fault than malformed
    /// JSON, and both must invoke nothing.
    Unparseable { reason: String },
}

impl RoutingOutcome {
    /// Whether this outcome is the router doing its job.
    ///
    /// Answering is a success. The session reports it as one, the ledger does
    /// not record it as a fault, and nothing in the UI calls it a miss — which
    /// is the whole difference between an answer agent with tools and a
    /// command router that sometimes gives up.
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            RoutingOutcome::Answer { .. } | RoutingOutcome::Action { .. }
        )
    }

    /// Whether anything at all will be dispatched. Only one variant may.
    pub fn invokes_action(&self) -> bool {
        matches!(self, RoutingOutcome::Action { .. })
    }
}

/// Build the prompt that asks for a routing decision.
pub fn build_routing_prompt(
    registry: &ActionRegistry,
    utterance: &str,
    history: &[(String, String)],
) -> String {
    let mut p = String::new();

    p.push_str(
        "You are a voice assistant. The user spoke a request. Your reply is read \
         aloud, so it must be brief.\n\n",
    );

    p.push_str("Reply with a single JSON object and nothing else. One of:\n\n");
    p.push_str(
        "  {\"answer\": \"<your answer>\"}\n    \
         Answer the user directly. THIS IS THE DEFAULT. Use it for questions, \
         for anything conversational, and for anything no action below covers. \
         Answering is a correct outcome, not a failure.\n\n",
    );
    p.push_str(
        "  {\"action\": \"<id>\", \"args\": {...}}\n    \
         Only when the user is asking for one of the actions below to be \
         performed. A question *about* what an action does is an answer, not \
         an action.\n\n",
    );
    p.push_str(
        "  {\"ambiguous\": [\"<id>\", \"<id>\"]}\n    \
         When more than one action genuinely fits and you cannot tell which.\n\n",
    );

    p.push_str(&format!(
        "Answers must be at most {ANSWER_WORD_LIMIT} words. Be direct: no preamble, \
         no restating the question, no offers of further help. If a full answer \
         cannot fit, give the most useful part — the user can ask a follow-up.\n\n"
    ));

    p.push_str("Actions:\n");
    if registry.list().is_empty() {
        p.push_str("  (none — always answer)\n");
    }
    for spec in registry.list() {
        p.push_str(&format!("- id: {}\n  when: {}\n", spec.id, spec.description));
        if spec.args.is_empty() {
            p.push_str("  args: none\n");
        } else {
            p.push_str("  args:\n");
            for a in spec.args {
                p.push_str(&format!(
                    "    - {} ({}{})\n",
                    a.name,
                    a.ty.as_str(),
                    if a.required { ", required" } else { ", optional" }
                ));
            }
        }
    }

    if !history.is_empty() {
        p.push_str("\nEarlier in this conversation:\n");
        for (user, assistant) in history {
            p.push_str(&format!("  User: {user}\n  You: {assistant}\n"));
        }
    }

    p.push_str(&format!("\nUser said: {utterance}\n"));
    p
}

/// Strip a fenced code block, if the model wrapped its JSON in one.
///
/// Models do this constantly regardless of instruction, and refusing a
/// correct decision over markdown punctuation would be a self-inflicted
/// failure rate.
fn unfence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop an optional language tag on the opening fence.
    let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
    rest.trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}

/// Find the outermost JSON object in `s`.
///
/// A model that emits a sentence before its JSON has still made a decision, and
/// discarding it would trade a recoverable formatting slip for a failed request.
/// Anything with no object at all is genuinely unparseable.
fn extract_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s[start..].char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + c.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Interpret a completion as a routing decision.
pub fn parse_routing_response(body: &str, registry: &ActionRegistry) -> RoutingOutcome {
    let cleaned = unfence(body);

    let Some(object) = extract_object(cleaned) else {
        return RoutingOutcome::Unparseable {
            reason: "the reply contained no JSON object".into(),
        };
    };

    let value: serde_json::Value = match serde_json::from_str(object) {
        Ok(v) => v,
        Err(e) => {
            return RoutingOutcome::Unparseable {
                reason: format!("the reply was not valid JSON: {e}"),
            }
        }
    };

    // Checked before `answer`, so a reply carrying both is treated as the
    // action it names rather than silently answering. Carrying both is itself
    // a confused decision, but acting on the more specific key is the reading
    // that surprises the user least.
    if let Some(id) = value.get("action").and_then(|v| v.as_str()) {
        let Some(spec) = registry.get(id) else {
            return RoutingOutcome::Unparseable {
                reason: format!("the reply named an unknown action {id:?}"),
            };
        };
        let raw_args = value
            .get("args")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return RoutingOutcome::Action { spec, raw_args };
    }

    if let Some(list) = value.get("ambiguous").and_then(|v| v.as_array()) {
        let candidates: Vec<&'static str> = list
            .iter()
            .filter_map(|v| v.as_str())
            .filter_map(|id| registry.get(id).map(|s| s.id))
            .collect();
        // One surviving candidate is not ambiguous, it is a decision; zero means
        // every name was invented.
        return match candidates.len() {
            0 => RoutingOutcome::Unparseable {
                reason: "the reply named no known actions".into(),
            },
            1 => RoutingOutcome::Action {
                spec: registry.get(candidates[0]).expect("just resolved"),
                raw_args: serde_json::Value::Null,
            },
            _ => RoutingOutcome::Ambiguous { candidates },
        };
    }

    if let Some(text) = value.get("answer").and_then(|v| v.as_str()) {
        let text = text.trim();
        if text.is_empty() {
            return RoutingOutcome::Unparseable {
                reason: "the reply contained an empty answer".into(),
            };
        }
        return RoutingOutcome::Answer {
            text: text.to_string(),
        };
    }

    RoutingOutcome::Unparseable {
        reason: "the reply was JSON but named no answer, action or ambiguity".into(),
    }
}

/// Ask `engine` to route `utterance`, and interpret what comes back.
///
/// The engine error is propagated rather than folded into
/// [`RoutingOutcome::Unparseable`]: a provider that could not be reached is a
/// different thing from a provider that answered badly, and the session says
/// different things about them. Both invoke nothing, which is the property that
/// matters.
pub async fn route(
    engine: &dyn LlmEngine,
    binding: &Binding,
    registry: &ActionRegistry,
    utterance: &str,
    history: &[(String, String)],
) -> Result<RoutingOutcome, EngineError> {
    let prompt = build_routing_prompt(registry, utterance, history);
    let response = engine
        .complete(LlmRequest {
            prompt,
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await?;
    Ok(parse_routing_response(&response.text, registry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::registry::CATALOG;
    use async_trait::async_trait;
    use kea_engines::traits::{EngineCaps, LlmResponse};

    /// An engine that returns whatever it was built with.
    ///
    /// The failure case holds a message rather than an `EngineError`, which is
    /// not `Clone` — the error is rebuilt per call instead.
    enum Canned {
        Body(&'static str),
        Fails(&'static str),
    }

    #[async_trait]
    impl LlmEngine for Canned {
        fn id(&self) -> &str {
            "canned"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, EngineError> {
            match self {
                Canned::Body(text) => Ok(LlmResponse::untracked(*text)),
                Canned::Fails(msg) => Err(EngineError::Other(msg.to_string())),
            }
        }
    }

    fn binding() -> Binding {
        Binding {
            engine_id: "canned".into(),
            model: None,
            provider_ref: None,
        }
    }

    #[tokio::test]
    async fn a_malformed_completion_invokes_nothing() {
        // The safety property of the JSON contract: degradation is a refusal,
        // never a wrong action.
        let engine = Canned::Body("I'd suggest opening Mail!");
        let out = route(&engine, &binding(), &reg(), "open mail", &[])
            .await
            .unwrap();
        assert!(
            matches!(out, RoutingOutcome::Unparseable { .. }),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_engine_is_an_error_not_an_action() {
        let engine = Canned::Fails("offline");
        let err = route(&engine, &binding(), &reg(), "open mail", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("offline"));
    }

    #[tokio::test]
    async fn a_question_about_an_action_is_answered_not_invoked() {
        // "What would starting a meeting do?" is a question. Routing it to
        // start_meeting would record a meeting the user did not ask for.
        let engine = Canned::Body(r#"{"answer":"It records and transcribes your meeting."}"#);
        let out = route(
            &engine,
            &binding(),
            &reg(),
            "what does starting a meeting do",
            &[],
        )
        .await
        .unwrap();
        assert!(
            matches!(out, RoutingOutcome::Answer { .. }),
            "got {out:?} — a question about an action must not invoke it"
        );
    }

    #[tokio::test]
    async fn answering_is_a_success_not_a_routing_failure() {
        let engine = Canned::Body(r#"{"answer":"Paris."}"#);
        let out = route(&engine, &binding(), &reg(), "capital of france", &[])
            .await
            .unwrap();
        assert!(out.is_success(), "answering must not be reported as failure");
        assert!(!out.invokes_action());
    }

    fn reg() -> ActionRegistry {
        ActionRegistry::default()
    }

    #[test]
    fn the_prompt_lists_every_action_id_and_its_arguments() {
        let p = build_routing_prompt(&reg(), "open mail", &[]);
        for spec in CATALOG {
            assert!(p.contains(spec.id), "{} missing from prompt", spec.id);
            assert!(p.contains(spec.description), "{} description missing", spec.id);
        }
        assert!(p.contains("app (text, required)"));
        assert!(p.contains("style (text, optional)"));
    }

    #[test]
    fn the_prompt_states_the_answer_default_and_the_length_limit() {
        let p = build_routing_prompt(&reg(), "what is rust", &[]);
        assert!(p.contains("THIS IS THE DEFAULT"));
        assert!(p.contains(&ANSWER_WORD_LIMIT.to_string()));
        assert!(p.contains("at most"));
    }

    #[test]
    fn the_prompt_carries_earlier_turns() {
        let history = vec![("what is it".to_string(), "a bird".to_string())];
        let p = build_routing_prompt(&reg(), "how big", &history);
        assert!(p.contains("what is it") && p.contains("a bird"));
    }

    #[test]
    fn a_well_formed_action_resolves() {
        let out = parse_routing_response(r#"{"action":"open_app","args":{"app":"Mail"}}"#, &reg());
        match out {
            RoutingOutcome::Action { spec, raw_args } => {
                assert_eq!(spec.id, "open_app");
                assert_eq!(raw_args["app"], "Mail");
            }
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn an_answer_resolves() {
        let out = parse_routing_response(r#"{"answer":"Rust is a language."}"#, &reg());
        assert_eq!(
            out,
            RoutingOutcome::Answer {
                text: "Rust is a language.".into()
            }
        );
    }

    #[test]
    fn json_wrapped_in_a_markdown_fence_still_parses() {
        let out = parse_routing_response("```json\n{\"answer\":\"hi\"}\n```", &reg());
        assert_eq!(out, RoutingOutcome::Answer { text: "hi".into() });
    }

    #[test]
    fn json_preceded_by_prose_still_parses() {
        let out = parse_routing_response("Sure! {\"answer\":\"hi\"}", &reg());
        assert_eq!(out, RoutingOutcome::Answer { text: "hi".into() });
    }

    #[test]
    fn an_action_outside_the_registry_invokes_nothing() {
        // The critical refusal: a hallucinated name must never reach a
        // dispatcher, and must not degrade into an answer either.
        let out = parse_routing_response(r#"{"action":"delete_all","args":{}}"#, &reg());
        match out {
            RoutingOutcome::Unparseable { reason } => assert!(reason.contains("delete_all")),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn prose_with_no_json_is_unparseable() {
        let out = parse_routing_response("I think you want to open Mail.", &reg());
        assert!(matches!(out, RoutingOutcome::Unparseable { .. }));
    }

    #[test]
    fn malformed_json_is_unparseable() {
        let out = parse_routing_response(r#"{"answer": "unterminated}"#, &reg());
        assert!(matches!(out, RoutingOutcome::Unparseable { .. }));
    }

    #[test]
    fn json_naming_nothing_recognised_is_unparseable() {
        let out = parse_routing_response(r#"{"thoughts":"hmm"}"#, &reg());
        assert!(matches!(out, RoutingOutcome::Unparseable { .. }));
    }

    #[test]
    fn an_empty_answer_is_unparseable_rather_than_spoken() {
        // Speaking nothing looks identical to a crash from the user's side.
        let out = parse_routing_response(r#"{"answer":"   "}"#, &reg());
        assert!(matches!(out, RoutingOutcome::Unparseable { .. }));
    }

    #[test]
    fn genuine_ambiguity_is_reported() {
        let out = parse_routing_response(r#"{"ambiguous":["open_app","start_meeting"]}"#, &reg());
        assert_eq!(
            out,
            RoutingOutcome::Ambiguous {
                candidates: vec!["open_app", "start_meeting"]
            }
        );
    }

    #[test]
    fn ambiguity_naming_only_invented_actions_is_unparseable() {
        let out = parse_routing_response(r#"{"ambiguous":["a","b"]}"#, &reg());
        assert!(matches!(out, RoutingOutcome::Unparseable { .. }));
    }

    #[test]
    fn ambiguity_with_one_real_candidate_is_a_decision() {
        let out = parse_routing_response(r#"{"ambiguous":["open_app","invented"]}"#, &reg());
        match out {
            RoutingOutcome::Action { spec, .. } => assert_eq!(spec.id, "open_app"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn an_action_with_no_args_key_gets_a_null_payload() {
        // `validate_args` accepts null for an argument-free action, so the
        // common "omitted the key" shape must survive to it.
        let out = parse_routing_response(r#"{"action":"read_focused"}"#, &reg());
        match out {
            RoutingOutcome::Action { spec, raw_args } => {
                assert_eq!(spec.id, "read_focused");
                assert!(raw_args.is_null());
            }
            other => panic!("expected an action, got {other:?}"),
        }
    }
}
