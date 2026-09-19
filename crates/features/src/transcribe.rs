//! File transcription: the feature descriptor for the drop-a-file flow.
//!
//! The work itself lives in `kea_core::transcript::job`, which is pure over an
//! injected engine, sink and cancel token so it can be tested without an STT
//! backend. What belongs here is only what the rest of the app dispatches on:
//! the id the STT slot resolves under, and the capability that slot needs.
//!
//! Registered even though it owns no hotkey. Without a registered feature the
//! resolver still answers — `require_stt("transcribe")` falls through to the
//! capability default and then to auto-selection — but the user has no way to
//! bind an engine to file transcription *deliberately*, because the slot
//! pickers are built from the registry. A feature that silently borrows
//! another's engine choice is the "descriptor exists but nothing dispatches on
//! it" shape the design review set out to remove.

use crate::feature::{CapKind, CapSlot, Command, Feature};

/// Feature id, shared with the `actions` ledger rows this flow writes.
pub const TRANSCRIBE_FEATURE_ID: &str = "transcribe";

pub struct TranscribeFeature;

impl Feature for TranscribeFeature {
    fn id(&self) -> &str {
        TRANSCRIBE_FEATURE_ID
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot {
            name: "stt",
            kind: CapKind::Stt,
        }]
    }

    /// No commands: transcription starts from a dropped file, not a hotkey.
    ///
    /// Deliberately empty rather than absent — `HOTKEY_ACTIONS` is built from
    /// the features that declare commands, so an empty list is what keeps this
    /// feature out of the hotkey table without a special case there.
    fn commands(&self) -> Vec<Command> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_declares_one_stt_slot_and_no_hotkey() {
        let f = TranscribeFeature;
        assert_eq!(f.id(), TRANSCRIBE_FEATURE_ID);

        let caps = f.required_caps();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].name, "stt");
        assert_eq!(caps[0].kind, CapKind::Stt);

        assert!(
            f.commands().is_empty(),
            "a file drop is not a hotkey; a command here would put a row in \
             HOTKEY_ACTIONS that nothing can trigger"
        );
    }
}
