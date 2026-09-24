//! What the assistant is allowed to do, described as data.
//!
//! **The registry is the safety property.** Two uncertain layers sit in front of
//! every invocation — the transcript may be wrong, and the router may be wrong —
//! so what makes a probabilistic assistant shippable is not care at the call
//! site but the fact that nothing it can reach has side effects. That rule is
//! enforced in [`super::registry`]; this module is the vocabulary it is written
//! in.
//!
//! An [`ActionSpec`] is `&'static` data rather than a trait object because
//! every consumer wants a different half of it: the router serialises `id`,
//! `description` and `args` into a prompt, the validator reads `args`, the
//! dispatcher matches on `id`, and the catalog test reads all of them at once.
//! A trait would make each of those a virtual call for no gain, and would let
//! an action describe itself differently to the prompt than to the validator.

use serde::Serialize;

/// The type of one argument value, as the router must produce it.
///
/// Deliberately small. These exist to make a *mismatch* detectable — a router
/// that answers `{"count": "three"}` must fail validation rather than reach an
/// action expecting a number — not to model a type system. Widen it when an
/// action needs something it cannot express, never in anticipation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgType {
    Text,
    Integer,
    Bool,
}

impl ArgType {
    /// The spelling used in the routing prompt, so the model is told the same
    /// name the validator will check against.
    pub fn as_str(self) -> &'static str {
        match self {
            ArgType::Text => "text",
            ArgType::Integer => "integer",
            ArgType::Bool => "bool",
        }
    }
}

/// One argument an action accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ArgSpec {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub ty: ArgType,
    pub required: bool,
    /// Whether this value is lifted from what the user said, rather than
    /// inferred from context by the router.
    ///
    /// This is the two-pass trigger. The streaming hypothesis is good enough to
    /// decide *which* action was meant — a misheard verb usually still routes —
    /// but not to fill in *which thing*: streaming models drop proper nouns
    /// first, and a proper noun is exactly what an argument cannot survive
    /// losing. An action with any `from_speech` argument waits for the offline
    /// re-decode; one without resolves immediately. See
    /// [`ActionSpec::spoken_arg_names`].
    pub from_speech: bool,
}

impl ArgSpec {
    pub const fn text(name: &'static str, required: bool, from_speech: bool) -> Self {
        Self {
            name,
            ty: ArgType::Text,
            required,
            from_speech,
        }
    }
}

/// One thing the assistant can do.
///
/// Every field is required by construction — there is no `Option` here and no
/// `Default`, so an action cannot exist without an id or without an argument
/// schema. That is the spec's "an entry missing either MUST NOT be invocable"
/// discharged by the type rather than by a runtime check: the unrepresentable
/// state needs no guard.
///
/// Note that *no arguments* and *no schema* are different things. An action
/// taking nothing declares `args: &[]`, which is a complete schema that happens
/// to be empty, and is fully invocable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ActionSpec {
    /// Stable across releases: it is what the router names, what the dispatcher
    /// matches, and what is written to the `actions` ledger, so changing one
    /// orphans history.
    pub id: &'static str,
    /// Shown to the user when reporting what was understood.
    pub title: &'static str,
    /// Written for the router, not for the UI. This is the only thing telling
    /// the model when to pick this action over answering directly, so it
    /// describes the *situation* the action is for.
    pub description: &'static str,
    pub args: &'static [ArgSpec],
    /// Whether running this changes anything belonging to the user.
    ///
    /// **Always false for anything the assistant can reach**, asserted over the
    /// whole catalog by a test in [`super::registry`]. This is one boolean
    /// rather than a tier enum on purpose: an earlier design had three risk
    /// tiers and populated only the safest, which is a mechanism that never
    /// fires — two empty tiers and a confirmation path with nothing to confirm.
    ///
    /// A single predicate that fails closed is stronger. It exists at all so
    /// that adding a side-effecting action is a build failure rather than a
    /// judgement call in review: the field must be set, and setting it true
    /// breaks the catalog test.
    pub has_side_effects: bool,
}

impl ActionSpec {
    /// The arguments whose values come from the user's speech.
    ///
    /// Returns an owned `Vec` rather than a borrowed slice because the answer
    /// is a filter over `args`, and there is no slice to borrow — the
    /// alternative is storing the names a second time on the spec, which lets
    /// the two copies disagree.
    pub fn spoken_arg_names(&self) -> Vec<&'static str> {
        self.args
            .iter()
            .filter(|a| a.from_speech)
            .map(|a| a.name)
            .collect()
    }

    /// Whether this action must wait for the offline transcript before running.
    pub fn needs_accurate_transcript(&self) -> bool {
        self.args.iter().any(|a| a.from_speech)
    }
}

/// The duplicate id in `specs`, if any.
///
/// A free function over a slice rather than a method on the catalog so the
/// catalog's own test and the fixture tests below exercise the same code. Two
/// actions sharing an id is not a cosmetic problem: the dispatcher matches on
/// `id`, so the second one is unreachable and the ledger cannot tell which ran.
pub fn duplicate_id(specs: &[ActionSpec]) -> Option<&'static str> {
    for (i, a) in specs.iter().enumerate() {
        if specs[i + 1..].iter().any(|b| b.id == a.id) {
            return Some(a.id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_ARGS: ActionSpec = ActionSpec {
        id: "read_focused",
        title: "Read what is on screen",
        description: "Read the user's current selection or window.",
        args: &[],
        has_side_effects: false,
    };

    const WITH_ARGS: ActionSpec = ActionSpec {
        id: "open_app",
        title: "Open an application",
        description: "Launch an application by name.",
        args: &[ArgSpec::text("app", true, true)],
        has_side_effects: false,
    };

    #[test]
    fn an_action_with_no_arguments_still_has_a_schema() {
        // Empty is not missing: this action is fully invocable, and the type
        // offers no way to express "schema absent" at all.
        assert!(NO_ARGS.args.is_empty());
        assert!(NO_ARGS.spoken_arg_names().is_empty());
    }

    #[test]
    fn spoken_arguments_are_the_ones_lifted_from_speech() {
        assert_eq!(WITH_ARGS.spoken_arg_names(), vec!["app"]);
        assert!(WITH_ARGS.needs_accurate_transcript());
    }

    #[test]
    fn an_action_without_spoken_arguments_skips_the_second_pass() {
        // The whole point of the flag: this is the case that must not pay for
        // an offline re-decode.
        assert!(!NO_ARGS.needs_accurate_transcript());
    }

    #[test]
    fn an_argument_the_router_infers_is_not_a_spoken_argument() {
        const INFERRED: ActionSpec = ActionSpec {
            id: "x",
            title: "x",
            description: "x",
            args: &[ArgSpec::text("style", false, false)],
            has_side_effects: false,
        };
        assert!(INFERRED.spoken_arg_names().is_empty());
        assert!(!INFERRED.needs_accurate_transcript());
    }

    #[test]
    fn duplicate_ids_are_detected() {
        let dup = [NO_ARGS, WITH_ARGS, NO_ARGS];
        assert_eq!(duplicate_id(&dup), Some("read_focused"));
    }

    #[test]
    fn distinct_ids_report_no_duplicate() {
        assert_eq!(duplicate_id(&[NO_ARGS, WITH_ARGS]), None);
    }

    #[test]
    fn arg_type_spellings_are_stable() {
        // These reach the routing prompt, so a rename here silently changes
        // what the model is asked for.
        assert_eq!(ArgType::Text.as_str(), "text");
        assert_eq!(ArgType::Integer.as_str(), "integer");
        assert_eq!(ArgType::Bool.as_str(), "bool");
    }
}
