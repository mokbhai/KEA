//! Checking the router's argument object against an action's declared schema.
//!
//! **This is the boundary between a language model's output and something that
//! runs.** Everything above it is a hypothesis; everything below it assumes the
//! values are the shape the action asked for. So the failures are enumerated
//! rather than collapsed into one "bad arguments" case: the session says
//! different things to the user for each, and the difference matters.
//!
//! * A missing required value is a question to ask — the user did not say which
//!   application, so the assistant asks.
//! * A type mismatch is a routing fault — the model was told the type and
//!   produced something else, and asking the user to rephrase will not help.
//! * An unknown argument means the model invented a parameter, which is the
//!   signal that it has drifted from the schema entirely.
//!
//! Collapsing these would leave the session with one unhelpful sentence for
//! three unrelated situations.

use std::collections::BTreeMap;

use super::action::{ActionSpec, ArgType};

/// One validated argument value, in the action's declared type.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgValue {
    Text(String),
    Integer(i64),
    Bool(bool),
}

impl ArgValue {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ArgValue::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// The arguments of one invocation, already checked against the schema.
///
/// A distinct type rather than a bare map so that holding one is evidence the
/// check happened — an action body cannot be handed unvalidated input without
/// someone constructing this deliberately.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedArgs(BTreeMap<&'static str, ArgValue>);

impl ResolvedArgs {
    pub fn get(&self, name: &str) -> Option<&ArgValue> {
        self.0.get(name)
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(ArgValue::as_text)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Replace one value with the same name's value from the offline
    /// transcript, after the second pass.
    ///
    /// Only the spoken arguments are re-derived, so this is a targeted
    /// overwrite rather than a rebuild: the router's non-spoken choices — a
    /// style it inferred, a flag it set — are not re-litigated by a better
    /// transcript, because they never came from the transcript.
    pub fn replace(&mut self, name: &'static str, value: ArgValue) {
        self.0.insert(name, value);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    #[error("the arguments were not a JSON object")]
    NotAnObject,
    #[error("missing required argument {name:?}")]
    MissingRequired { name: &'static str },
    #[error("argument {name:?} should be {expected} but was {got}")]
    TypeMismatch {
        name: &'static str,
        expected: &'static str,
        got: &'static str,
    },
    #[error("unknown argument {name:?}")]
    UnknownArgument { name: String },
}

/// What a JSON value is, for the mismatch message. `serde_json` has no stable
/// public name for this, and the message is user-facing.
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        serde_json::Value::String(_) => "text",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "object",
    }
}

/// Check `raw` against `spec`, returning the values the action will run with.
///
/// Fails on the first problem rather than collecting every one: the session
/// shows a single sentence, and the first fault is invariably the one that
/// explains the rest.
pub fn validate_args(spec: &ActionSpec, raw: &serde_json::Value) -> Result<ResolvedArgs, ArgError> {
    // An action taking nothing accepts an absent object as readily as an empty
    // one. Models omit the key rather than sending `{}` about half the time,
    // and rejecting that would fail a correct route on a formatting habit.
    let obj = match raw {
        serde_json::Value::Null => return finish(spec, &serde_json::Map::new()),
        serde_json::Value::Object(map) => map,
        _ => return Err(ArgError::NotAnObject),
    };

    for key in obj.keys() {
        if !spec.args.iter().any(|a| a.name == key) {
            return Err(ArgError::UnknownArgument { name: key.clone() });
        }
    }

    finish(spec, obj)
}

fn finish(
    spec: &ActionSpec,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<ResolvedArgs, ArgError> {
    let mut out = BTreeMap::new();

    for arg in spec.args {
        // A null is the same as absent. A model that has nothing to put in an
        // optional field writes `null` as often as it omits the key, and
        // treating those differently would make an optional argument fail.
        let supplied = obj.get(arg.name).filter(|v| !v.is_null());

        let Some(value) = supplied else {
            if arg.required {
                return Err(ArgError::MissingRequired { name: arg.name });
            }
            continue;
        };

        let resolved = match arg.ty {
            ArgType::Text => value.as_str().map(|s| ArgValue::Text(s.to_string())),
            ArgType::Integer => value.as_i64().map(ArgValue::Integer),
            ArgType::Bool => value.as_bool().map(ArgValue::Bool),
        };

        let Some(resolved) = resolved else {
            return Err(ArgError::TypeMismatch {
                name: arg.name,
                expected: arg.ty.as_str(),
                got: json_kind(value),
            });
        };

        out.insert(arg.name, resolved);
    }

    Ok(ResolvedArgs(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::action::ArgSpec;
    use serde_json::json;

    const OPEN_APP: ActionSpec = ActionSpec {
        id: "open_app",
        title: "Open an application",
        description: "Launch an application by name.",
        args: &[ArgSpec::text("app", true, true)],
        has_side_effects: false,
    };

    const NO_ARGS: ActionSpec = ActionSpec {
        id: "read_focused",
        title: "Read",
        description: "Read the focused window.",
        args: &[],
        has_side_effects: false,
    };

    const OPTIONAL: ActionSpec = ActionSpec {
        id: "rewrite_focused",
        title: "Rewrite",
        description: "Rewrite the selection.",
        args: &[ArgSpec::text("style", false, false)],
        has_side_effects: false,
    };

    const COUNTED: ActionSpec = ActionSpec {
        id: "counted",
        title: "Counted",
        description: "Takes a number.",
        args: &[ArgSpec {
            name: "count",
            ty: ArgType::Integer,
            required: true,
            from_speech: false,
        }],
        has_side_effects: false,
    };

    #[test]
    fn a_valid_payload_resolves() {
        let got = validate_args(&OPEN_APP, &json!({"app": "Mail"})).unwrap();
        assert_eq!(got.text("app"), Some("Mail"));
    }

    #[test]
    fn a_missing_required_argument_is_its_own_error() {
        // The session asks the user for this one, so it must not look like a
        // type fault.
        let err = validate_args(&OPEN_APP, &json!({})).unwrap_err();
        assert_eq!(err, ArgError::MissingRequired { name: "app" });
    }

    #[test]
    fn a_type_mismatch_is_its_own_error() {
        let err = validate_args(&COUNTED, &json!({"count": "three"})).unwrap_err();
        assert_eq!(
            err,
            ArgError::TypeMismatch {
                name: "count",
                expected: "integer",
                got: "text",
            }
        );
    }

    #[test]
    fn an_unknown_argument_is_rejected() {
        let err = validate_args(&OPEN_APP, &json!({"app": "Mail", "volume": 3})).unwrap_err();
        assert_eq!(
            err,
            ArgError::UnknownArgument {
                name: "volume".to_string()
            }
        );
    }

    #[test]
    fn an_action_with_no_arguments_accepts_an_absent_object() {
        // Models omit the key about as often as they send `{}`; both are a
        // correct route and neither may fail.
        assert!(validate_args(&NO_ARGS, &json!(null)).unwrap().is_empty());
        assert!(validate_args(&NO_ARGS, &json!({})).unwrap().is_empty());
    }

    #[test]
    fn an_optional_argument_may_be_omitted_or_null() {
        assert!(validate_args(&OPTIONAL, &json!({})).unwrap().is_empty());
        assert!(validate_args(&OPTIONAL, &json!({"style": null}))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_required_argument_explicitly_null_is_missing_not_mismatched() {
        let err = validate_args(&OPEN_APP, &json!({"app": null})).unwrap_err();
        assert_eq!(err, ArgError::MissingRequired { name: "app" });
    }

    #[test]
    fn a_non_object_payload_is_rejected() {
        let err = validate_args(&OPEN_APP, &json!("Mail")).unwrap_err();
        assert_eq!(err, ArgError::NotAnObject);
    }

    #[test]
    fn a_spoken_argument_can_be_replaced_after_the_second_pass() {
        let mut got = validate_args(&OPEN_APP, &json!({"app": "male"})).unwrap();
        got.replace("app", ArgValue::Text("Mail".into()));
        assert_eq!(got.text("app"), Some("Mail"));
    }
}
