//! The local model API: the feature descriptor behind the OpenAI-compatible
//! routes on the loopback API.
//!
//! The model routes are not a feature with a hotkey or a window; they are
//! requests that borrow the engines the app already has. What they need from
//! the registry is the same thing the transcribe descriptor provides: a
//! feature id the bindings can hold an engine choice under, so the user can
//! pin the API to a different model than the one dictation uses, and so the
//! Models page can show an "API" row.
//!
//! It declares all three slots because one API surface serves all three
//! kinds of model. A request uses one of them per call, and the resolver
//! falls back to the capability default for any slot that is not bound.

use crate::feature::{CapKind, CapSlot, Command, Feature};

/// Feature id the model routes resolve their slots under. It is also the
/// `feature` column on the usage rows those requests write, so the usage view
/// attributes API calls separately from the app's own features.
pub const API_FEATURE_ID: &str = "api";

pub struct ApiFeature;

impl Feature for ApiFeature {
    fn id(&self) -> &str {
        API_FEATURE_ID
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![
            CapSlot {
                name: "stt",
                kind: CapKind::Stt,
            },
            CapSlot {
                name: "tts",
                kind: CapKind::Tts,
            },
            CapSlot {
                name: "llm",
                kind: CapKind::Llm,
            },
        ]
    }

    /// No commands: an API call is a request from another process, not a
    /// hotkey. An empty list keeps it out of `HOTKEY_ACTIONS` the same way the
    /// transcribe descriptor is kept out.
    fn commands(&self) -> Vec<Command> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_declares_stt_tts_and_llm_slots_and_no_hotkey() {
        let f = ApiFeature;
        assert_eq!(f.id(), API_FEATURE_ID);

        let kinds: Vec<CapKind> = f.required_caps().iter().map(|c| c.kind).collect();
        assert_eq!(kinds, vec![CapKind::Stt, CapKind::Tts, CapKind::Llm]);
        assert!(f.commands().is_empty());
    }
}
