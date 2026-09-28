//! The catalog of actions the assistant may invoke, and the rule bounding it.
//!
//! **Nothing here has side effects on the user's data.** That is the whole
//! safety argument for the feature: two uncertain layers stand in front of
//! every invocation — the transcript may be wrong, and the router may be wrong —
//! and no amount of care at the call site fixes a compound failure of both.
//! What fixes it is that the worst outcome of a wrong route is an irrelevant
//! answer or a window the user closes.
//!
//! An earlier design expressed this as three risk tiers and then populated only
//! the safest, which is a mechanism that never fires: two tiers with no members,
//! a confirmation path with nothing to confirm, and a concept every reader had
//! to carry that never distinguished any two entries. One predicate that fails
//! closed is both simpler and stronger — see [`ActionSpec::has_side_effects`]
//! and the catalog test below, which is what actually enforces it.
//!
//! Note what is *not* here: answering. Answering the user is the default
//! outcome, not a catalog entry, so it needs no id, no argument schema and no
//! ledger row distinct from the session itself.

use super::action::{ActionSpec, ArgSpec};

/// Read whatever the user is currently looking at.
pub const READ_FOCUSED: ActionSpec = ActionSpec {
    id: "read_focused",
    title: "Read what's on screen",
    description: "Read the user's current selection, or the contents of the window they \
                  are looking at, so a question about it can be answered. Use when the \
                  request refers to something on screen — \"this\", \"that error\", \
                  \"what I'm looking at\".",
    args: &[],
    has_side_effects: false,
};

/// Bring an application to the front, launching it if needed.
pub const OPEN_APP: ActionSpec = ActionSpec {
    id: "open_app",
    title: "Open an application",
    description: "Open or switch to an application by name. Use only when the user asks \
                  for an application to be opened, not when they ask a question about one.",
    args: &[ArgSpec::text("app", true, true)],
    has_side_effects: false,
};

/// Begin a meeting recording.
pub const START_MEETING: ActionSpec = ActionSpec {
    id: "start_meeting",
    title: "Start a meeting recording",
    description: "Begin capturing and transcribing a meeting. Use when the user asks to \
                  start recording or to take notes on a meeting.",
    args: &[],
    has_side_effects: false,
};

/// Rewrite the user's current selection.
pub const REWRITE_FOCUSED: ActionSpec = ActionSpec {
    id: "rewrite_focused",
    title: "Rewrite the selection",
    description: "Rewrite the text the user has selected, optionally in a named style. \
                  Use when the user asks for their selected text to be reworded, not when \
                  they ask what it means.",
    args: &[ArgSpec::text("style", false, true)],
    has_side_effects: false,
};

/// Every action the assistant may invoke.
///
/// Ordered as the router sees them, which is also the order the settings UI
/// lists them in. Adding a row here is the whole of adding an action.
pub const CATALOG: &[ActionSpec] = &[READ_FOCUSED, OPEN_APP, START_MEETING, REWRITE_FOCUSED];

/// Lookup over a catalog.
///
/// Borrows rather than owns, so tests can build a registry over a fixture
/// catalog without the real one being a special case. [`Default`] is the real
/// catalog, which is what production wants.
#[derive(Debug, Clone, Copy)]
pub struct ActionRegistry {
    specs: &'static [ActionSpec],
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self { specs: CATALOG }
    }
}

impl ActionRegistry {
    pub const fn new(specs: &'static [ActionSpec]) -> Self {
        Self { specs }
    }

    /// The action with this id, or `None`.
    ///
    /// `None` is not an error condition to be smoothed over: it is how a router
    /// naming something outside the catalog is refused, which is the check that
    /// stops a hallucinated action name from reaching a dispatcher.
    pub fn get(&self, id: &str) -> Option<&'static ActionSpec> {
        self.specs.iter().find(|s| s.id == id)
    }

    pub fn list(&self) -> &'static [ActionSpec] {
        self.specs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::action::duplicate_id;

    #[test]
    fn the_catalog_has_no_duplicate_ids() {
        // A duplicate makes the second entry unreachable and the ledger
        // ambiguous about which one ran.
        assert_eq!(duplicate_id(CATALOG), None);
    }

    #[test]
    fn an_id_outside_the_catalog_resolves_to_nothing() {
        // This is the refusal that stops an invented action name from being
        // dispatched.
        let reg = ActionRegistry::default();
        assert!(reg.get("delete_everything").is_none());
        assert!(reg.get("").is_none());
        assert!(reg.get("READ_FOCUSED").is_none(), "ids are case-sensitive");
    }

    #[test]
    fn every_catalog_entry_is_retrievable_by_its_own_id() {
        let reg = ActionRegistry::default();
        for spec in CATALOG {
            assert_eq!(reg.get(spec.id).map(|s| s.id), Some(spec.id));
        }
    }

    #[test]
    fn list_returns_the_whole_catalog() {
        assert_eq!(ActionRegistry::default().list().len(), CATALOG.len());
    }

    #[test]
    fn every_entry_describes_itself_for_the_router() {
        // The description is the only thing telling the model when to pick this
        // action over answering, so an empty one silently degrades routing.
        for spec in CATALOG {
            assert!(
                !spec.description.is_empty(),
                "{} has no description",
                spec.id
            );
            assert!(!spec.title.is_empty(), "{} has no title", spec.id);
        }
    }

    #[test]
    fn nothing_the_assistant_can_reach_has_side_effects() {
        // This is the feature's entire safety argument, and it is why there is
        // no confirmation UI: a wrong route costs an irrelevant answer, never
        // the user's work. Adding an action with side effects must fail here
        // rather than be caught in review.
        for spec in CATALOG {
            assert!(
                !spec.has_side_effects,
                "{} has side effects — the assistant may not reach it. If this \
                 action is genuinely wanted, a confirmation model has to be \
                 designed first; see openspec/changes/add-voice-assistant/design.md.",
                spec.id
            );
        }
    }

    #[test]
    fn a_registry_can_be_built_over_a_fixture_catalog() {
        const FIXTURE: &[ActionSpec] = &[READ_FOCUSED];
        let reg = ActionRegistry::new(FIXTURE);
        assert_eq!(reg.list().len(), 1);
        assert!(reg.get("open_app").is_none());
    }
}
