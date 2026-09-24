//! The voice assistant feature: ask a question, hear a short answer.

pub mod handlers;

use crate::feature::{CapKind, CapSlot, Command, Feature};

pub use handlers::{OpenApp, ReadFocused};

/// Feature id, shared with the app layer's hotkey table and the bindings rows.
pub const ASSISTANT_FEATURE_ID: &str = "assistant";

/// The one command this feature owns.
pub const ASSISTANT_COMMAND_ID: &str = "ask";

pub struct AssistantFeature;

impl Feature for AssistantFeature {
    fn id(&self) -> &str {
        ASSISTANT_FEATURE_ID
    }

    /// Three slots, because a single request uses all three engine kinds:
    /// speech in, a model to answer or route, and speech out.
    ///
    /// `tts` is a required cap rather than an optional extra even though the
    /// assistant works without it. A cap is what gives the user a picker for
    /// the voice the answers are read in, and an assistant whose speech engine
    /// cannot be chosen is one whose main output channel is unconfigurable.
    /// Missing at runtime is handled separately — the answer is still shown,
    /// and the session says it could not be spoken.
    fn required_caps(&self) -> Vec<CapSlot> {
        vec![
            CapSlot {
                name: "llm",
                kind: CapKind::Llm,
            },
            CapSlot {
                name: "stt",
                kind: CapKind::Stt,
            },
            CapSlot {
                name: "tts",
                kind: CapKind::Tts,
            },
        ]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: ASSISTANT_COMMAND_ID.into(),
            title: "Ask KEA".into(),
            default_accelerator: Some(crate::feature::platform_accelerator('A')),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assistant_declares_one_command() {
        let cmds = AssistantFeature.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, ASSISTANT_COMMAND_ID);
        assert!(cmds[0].default_accelerator.is_some());
    }

    #[test]
    fn its_default_accelerator_collides_with_no_other_feature() {
        // Every default in the app, gathered here rather than asserted as a
        // literal: a feature that later moves to `A` fails this test instead of
        // silently stealing the assistant's key at registration time, where the
        // loser is whichever registers second.
        let mine = AssistantFeature.commands()[0]
            .default_accelerator
            .clone()
            .unwrap();

        let others: Vec<String> = crate::rewrite::RewriteFeature
            .commands()
            .into_iter()
            .chain(crate::dictation::DictationFeature.commands())
            .chain(crate::tts::TtsFeature.commands())
            .chain(crate::meeting::MeetingFeature.commands())
            .filter_map(|c| c.default_accelerator)
            .collect();

        assert!(
            !others.contains(&mine),
            "{mine} is already bound by another feature: {others:?}"
        );
    }

    #[test]
    fn it_asks_for_speech_in_a_model_and_speech_out() {
        let caps = AssistantFeature.required_caps();
        let kinds: Vec<CapKind> = caps.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&CapKind::Llm));
        assert!(kinds.contains(&CapKind::Stt));
        assert!(kinds.contains(&CapKind::Tts));
    }

    #[test]
    fn its_slot_names_are_the_ones_the_resolver_looks_up() {
        // `SlotResolver::resolve` keys binding rows on `CapKind::as_str()`, so
        // a slot named anything else resolves to nothing at runtime while
        // looking perfectly reasonable here.
        for cap in AssistantFeature.required_caps() {
            assert_eq!(cap.name, cap.kind.as_str());
        }
    }
}
