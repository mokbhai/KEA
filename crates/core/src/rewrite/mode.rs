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
}

impl RewriteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RewriteMode::Improve => "improve",
            RewriteMode::FixGrammar => "fix_grammar",
            RewriteMode::Professional => "professional",
            RewriteMode::Concise => "concise",
            RewriteMode::Friendly => "friendly",
            RewriteMode::AudioRefinement => "audio_refinement",
            RewriteMode::AskKea => "ask_kea",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "improve" => Some(RewriteMode::Improve),
            "fix_grammar" => Some(RewriteMode::FixGrammar),
            "professional" => Some(RewriteMode::Professional),
            "concise" => Some(RewriteMode::Concise),
            "friendly" => Some(RewriteMode::Friendly),
            "audio_refinement" => Some(RewriteMode::AudioRefinement),
            "ask_kea" => Some(RewriteMode::AskKea),
            _ => None,
        }
    }
}
