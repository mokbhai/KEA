use std::fmt::Display;

use kea_core::store::actions::{ActionRepo, ActionStatus};

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
