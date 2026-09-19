use std::fmt::Display;

use kea_core::app_context::{AppProfile, InsertionMode};
use kea_core::store::actions::{ActionRepo, ActionStatus};
use kea_core::store::bindings::Binding;
use kea_platform::ReplaceMode;

/// What a matched app profile changes about one run.
///
/// A struct rather than three more parameters, and `Default` means "inherit
/// everything" — which is exactly the no-profile case, so a caller that has no
/// profile passes `&ProfileOverrides::default()` and every branch below reads
/// the same way as before.
///
/// `mode` and `preset_id` are deliberately NOT here: those are applied to the
/// `RewriteInput` at the app layer, before the request is built, because the
/// mode determines which settings key supplies the mode's parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileOverrides {
    /// Substituted for `SlotResolver::require_llm`, not merged with it.
    pub llm_binding: Option<Binding>,
    /// How text is put back into the app.
    pub insertion: Option<ReplaceMode>,
    /// Tri-state. `Some(false)` is the point of the feature — no LLM cleanup
    /// pass into a shell prompt — so it must not collapse into `None`.
    pub post_process: Option<bool>,
}

impl ProfileOverrides {
    /// Reads a resolved profile into the shape the features consume.
    ///
    /// The `InsertionMode` -> `ReplaceMode` mapping lives here because
    /// `kea_core` deliberately does not depend on `kea_platform`: the domain
    /// type says what the user asked for, the platform type says how text gets
    /// inserted, and only this crate sees both.
    pub fn from_profile(profile: Option<&AppProfile>) -> Self {
        let Some(profile) = profile else {
            return Self::default();
        };
        Self {
            llm_binding: profile.llm_binding(),
            insertion: profile.insertion().map(|m| match m {
                InsertionMode::Accessibility => ReplaceMode::Accessibility,
                InsertionMode::ClipboardPaste => ReplaceMode::ClipboardPaste,
            }),
            post_process: profile.post_process,
        }
    }

    /// Which LLM this run uses, given the preset's own override.
    ///
    /// **The app profile wins.** Both are overrides of the `rewrite/llm`
    /// binding and both are things the user wrote down, so the tie has to be
    /// broken by what each one *means*:
    ///
    /// * A preset is a preference about the job — "this rewrite is hard, use
    ///   the big model" — and paying for a bigger model than intended is a
    ///   disappointing outcome.
    /// * A profile is a rule about where the text is going — "in Terminal,
    ///   keep it on the local model" — and that is a constraint, not a
    ///   preference. Losing it sends a shell buffer to a hosted provider,
    ///   which is not a disappointing outcome but a broken promise.
    ///
    /// So the narrower-scoped preset yields to the profile, and the Rewrite
    /// page says as much beside the preset's AI picker rather than leaving it
    /// to be discovered.
    ///
    /// `None` from both means "use the slot binding": neither names an engine,
    /// and neither half-applies (see [`Binding::from_parts`]).
    pub fn llm_binding_over(&self, preset: Option<Binding>) -> Option<Binding> {
        self.llm_binding.clone().or(preset)
    }

    /// The insertion mode to use, defaulting to today's behaviour.
    pub fn replace_mode(&self) -> ReplaceMode {
        self.insertion.unwrap_or(ReplaceMode::ClipboardPaste)
    }

    /// Whether to run the LLM cleanup pass, given the global setting.
    pub fn post_process_or(&self, global: bool) -> bool {
        self.post_process.unwrap_or(global)
    }
}

pub use kea_core::resolve::CapKind;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CapSlot {
    pub name: &'static str,
    pub kind: CapKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Command {
    pub id: String,
    pub title: String,
    pub default_accelerator: Option<String>,
}

pub trait Feature: Send + Sync {
    fn id(&self) -> &str;
    fn required_caps(&self) -> Vec<CapSlot>;
    fn commands(&self) -> Vec<Command> {
        vec![]
    }
}

/// The platform spelling of a feature's default `Shift`+`key` accelerator.
///
/// Every global-hotkey command wants the same shape and the same two
/// spellings: macOS shows `Cmd`, everywhere else Tauri's portable
/// `CommandOrControl`. Written once here so a feature declares only its key.
pub fn platform_accelerator(key: char) -> String {
    #[cfg(target_os = "macos")]
    {
        format!("Cmd+Shift+{key}")
    }
    #[cfg(not(target_os = "macos"))]
    {
        format!("CommandOrControl+Shift+{key}")
    }
}

/// Owns the `actions` ledger row for the length of one feature run.
///
/// Every run has to close its row on both edges — `ok` at the end, `error`
/// carrying the message on any early return — and a DB failure while closing it
/// is logged, never propagated: the run's own error is the one the caller asked
/// about. Keeping that epilogue here is what lets a run body stay a flat
/// `?`-chain, with the outer function doing:
///
/// ```ignore
/// let guard = ActionGuard::new(actions, action_id, "dictation");
/// let result = run_inner(..).await;
/// match result {
///     Ok(v) => { guard.succeed().await; Ok(v) }
///     Err(e) => Err(guard.fail(e).await),
/// }
/// ```
pub struct ActionGuard<'a> {
    actions: &'a ActionRepo,
    id: i64,
    feature: &'static str,
    outcome: Option<ActionStatus>,
    released: bool,
}

impl<'a> ActionGuard<'a> {
    pub fn new(actions: &'a ActionRepo, id: i64, feature: &'static str) -> Self {
        Self {
            actions,
            id,
            feature,
            outcome: None,
            released: false,
        }
    }

    /// Closes the row as `error` and hands back the message to return, so the
    /// call site reads `Err(guard.fail(e).await)`.
    pub async fn fail(mut self, e: impl Display) -> String {
        let msg = e.to_string();
        self.finish(ActionStatus::Error, Some(&msg)).await;
        msg
    }

    pub async fn succeed(mut self) {
        self.finish(ActionStatus::Ok, None).await;
    }

    /// Closes the row as something the user called off rather than a failure.
    ///
    /// Takes a reason because "cancelled" alone does not say what was dropped;
    /// it lands in the same column as an error message, just under a status
    /// that is not red.
    pub async fn cancel(mut self, reason: impl Display) -> String {
        let msg = reason.to_string();
        self.finish(ActionStatus::Cancelled, Some(&msg)).await;
        msg
    }

    /// Hands the still-open row to the caller, which becomes responsible for
    /// closing it (see `run_tts_synthesize`, whose caller plays the audio).
    pub fn release(mut self) -> i64 {
        self.released = true;
        self.id
    }

    async fn finish(&mut self, outcome: ActionStatus, error: Option<&str>) {
        self.outcome = Some(outcome);
        if let Err(e) = self.actions.finish(self.id, outcome, error).await {
            tracing::warn!(
                error = %e,
                action_id = %self.id,
                feature = %self.feature,
                outcome = %outcome.as_str(),
                "failed to finish action in DB"
            );
        }
    }
}

impl Drop for ActionGuard<'_> {
    fn drop(&mut self) {
        if self.outcome.is_none() && !self.released {
            // A run that returns without closing its row leaves it pending
            // forever in History; there is no async drop to do it for us.
            tracing::warn!(
                action_id = %self.id,
                feature = %self.feature,
                "action row dropped without an outcome"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(engine: &str) -> Binding {
        Binding {
            engine_id: engine.into(),
            model: None,
            provider_ref: None,
        }
    }

    #[test]
    fn an_app_rule_outranks_a_preset_that_names_its_own_ai() {
        // The rule this encodes: a profile is a constraint about where the
        // text goes, a preset a preference about the job. See the doc comment
        // — getting this backwards sends a Terminal buffer to a hosted
        // provider the user told KEA not to use there.
        let profile = ProfileOverrides {
            llm_binding: Some(binding("local-llm")),
            ..ProfileOverrides::default()
        };
        assert_eq!(
            profile.llm_binding_over(Some(binding("openai"))),
            Some(binding("local-llm"))
        );
    }

    #[test]
    fn a_preset_applies_when_no_rule_matched() {
        let none = ProfileOverrides::default();
        assert_eq!(
            none.llm_binding_over(Some(binding("openai"))),
            Some(binding("openai"))
        );
    }

    #[test]
    fn neither_means_the_slot_binding() {
        // `None` is what sends the caller to `SlotResolver::require_llm`, so
        // it must survive both overrides being absent.
        assert_eq!(ProfileOverrides::default().llm_binding_over(None), None);
    }
}
