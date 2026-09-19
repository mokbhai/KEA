use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewriteMode {
    Improve,
    FixGrammar,
    Professional,
    Concise,
    Friendly,
    AudioRefinement,
    AskKea,
    Translate,
}

/// What a mode needs beyond the source text, for the two modes whose prompt is
/// a template rather than a fixed string.
///
/// Both travel in `RewriteInput::custom_instruction` — one parameter slot, one
/// settings read at the command layer — but they are not interchangeable: the
/// instruction is free-form prose interpolated into the prompt body, while the
/// target is a language tag validated against [`super::language`] before it
/// reaches the template. Keeping the distinction in a descriptor is what lets
/// every consumer branch on `mode.parameter()` instead of re-deciding which
/// mode means which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeParameter {
    /// Ask KEA's user-written instruction.
    Instruction,
    /// Translate's BCP-47 target language tag.
    TargetLanguage,
}

impl ModeParameter {
    /// The settings key holding the saved value, for the callers that build a
    /// rewrite from stored settings rather than from an explicit argument.
    pub fn setting_key(self) -> &'static str {
        match self {
            ModeParameter::Instruction => "rewrite.custom_instruction",
            ModeParameter::TargetLanguage => "rewrite.translate.target",
        }
    }
}

impl RewriteMode {
    /// Every mode, in the order the settings picker shows them.
    pub const ALL: [RewriteMode; 8] = [
        RewriteMode::Improve,
        RewriteMode::FixGrammar,
        RewriteMode::Professional,
        RewriteMode::Concise,
        RewriteMode::Friendly,
        RewriteMode::AudioRefinement,
        RewriteMode::AskKea,
        RewriteMode::Translate,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RewriteMode::Improve => "improve",
            RewriteMode::FixGrammar => "fix_grammar",
            RewriteMode::Professional => "professional",
            RewriteMode::Concise => "concise",
            RewriteMode::Friendly => "friendly",
            RewriteMode::AudioRefinement => "audio_refinement",
            RewriteMode::AskKea => "ask_kea",
            RewriteMode::Translate => "translate",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "improve" => Some(RewriteMode::Improve),
            "fix_grammar" => Some(RewriteMode::FixGrammar),
            "professional" => Some(RewriteMode::Professional),
            "concise" => Some(RewriteMode::Concise),
            "friendly" => Some(RewriteMode::Friendly),
            "audio_refinement" => Some(RewriteMode::AudioRefinement),
            "ask_kea" => Some(RewriteMode::AskKea),
            "translate" => Some(RewriteMode::Translate),
            _ => None,
        }
    }

    /// The extra value this mode's prompt template needs, if any.
    pub fn parameter(self) -> Option<ModeParameter> {
        match self {
            RewriteMode::AskKea => Some(ModeParameter::Instruction),
            RewriteMode::Translate => Some(ModeParameter::TargetLanguage),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_round_trips_through_its_string() {
        // A half-added mode — variant and `as_str` but no `from_str` arm —
        // fails silently: the stored setting stops parsing and the reader
        // falls back to Improve. This is the cheap guard against that.
        for mode in RewriteMode::ALL {
            assert_eq!(RewriteMode::from_str(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn translate_uses_its_own_tag_and_setting() {
        assert_eq!(RewriteMode::Translate.as_str(), "translate");
        assert_eq!(
            RewriteMode::Translate.parameter(),
            Some(ModeParameter::TargetLanguage)
        );
        assert_eq!(
            RewriteMode::Translate
                .parameter()
                .map(ModeParameter::setting_key),
            Some("rewrite.translate.target")
        );
    }

    #[test]
    fn plain_modes_take_no_parameter() {
        assert_eq!(RewriteMode::Improve.parameter(), None);
        assert_eq!(
            RewriteMode::AskKea.parameter(),
            Some(ModeParameter::Instruction)
        );
    }

    #[test]
    fn serde_matches_as_str() {
        for mode in RewriteMode::ALL {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
        }
    }
}
