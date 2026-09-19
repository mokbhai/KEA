//! Translation targets: the closed list of languages the picker offers, and
//! the tag → prompt-name resolution the Translate template renders with.
//!
//! The stored value is always a BCP-47 tag (`"fr"`, `"pt-BR"`), because that is
//! what a settings row, a hotkey command id (`translate.pt-BR`) and a future
//! locale lookup all agree on. The *prompt* wants a name a model reads as a
//! language rather than a code, so the two are separated here instead of at
//! each call site.

use crate::error::KeaError;

/// A language the translate mode can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranslationTarget {
    /// BCP-47 tag. The stored value, and the suffix of the per-language hotkey
    /// command id.
    pub tag: &'static str,
    /// The English name, shown in the picker and rendered into the prompt.
    pub label: &'static str,
}

impl TranslationTarget {
    // Keeps the table below one line per language; rustfmt expands a struct
    // literal to four.
    const fn new(tag: &'static str, label: &'static str) -> Self {
        Self { tag, label }
    }
}

/// The offered targets, ordered by label so the picker needs no sort.
///
/// Not exhaustive and not meant to be: [`describe`] accepts any well-formed
/// tag, so a language missing here still works when typed or inherited from
/// the system locale — it just gets a plainer prompt name.
pub const TRANSLATION_TARGETS: &[TranslationTarget] = &[
    TranslationTarget::new("en-US", "American English"),
    TranslationTarget::new("ar", "Arabic"),
    TranslationTarget::new("bn", "Bengali"),
    TranslationTarget::new("pt-BR", "Brazilian Portuguese"),
    TranslationTarget::new("en-GB", "British English"),
    TranslationTarget::new("bg", "Bulgarian"),
    TranslationTarget::new("fr-CA", "Canadian French"),
    TranslationTarget::new("cs", "Czech"),
    TranslationTarget::new("da", "Danish"),
    TranslationTarget::new("nl", "Dutch"),
    TranslationTarget::new("en", "English"),
    TranslationTarget::new("tl", "Filipino"),
    TranslationTarget::new("fi", "Finnish"),
    TranslationTarget::new("fr", "French"),
    TranslationTarget::new("de", "German"),
    TranslationTarget::new("el", "Greek"),
    TranslationTarget::new("he", "Hebrew"),
    TranslationTarget::new("hi", "Hindi"),
    TranslationTarget::new("hu", "Hungarian"),
    TranslationTarget::new("is", "Icelandic"),
    TranslationTarget::new("id", "Indonesian"),
    TranslationTarget::new("it", "Italian"),
    TranslationTarget::new("ja", "Japanese"),
    TranslationTarget::new("ko", "Korean"),
    TranslationTarget::new("es-419", "Latin American Spanish"),
    TranslationTarget::new("ms", "Malay"),
    TranslationTarget::new("nb", "Norwegian Bokmal"),
    TranslationTarget::new("fa", "Persian"),
    TranslationTarget::new("pl", "Polish"),
    TranslationTarget::new("pt", "Portuguese"),
    TranslationTarget::new("ro", "Romanian"),
    TranslationTarget::new("ru", "Russian"),
    TranslationTarget::new("zh-Hans", "Simplified Chinese"),
    TranslationTarget::new("sk", "Slovak"),
    TranslationTarget::new("es", "Spanish"),
    TranslationTarget::new("sv", "Swedish"),
    TranslationTarget::new("ta", "Tamil"),
    TranslationTarget::new("th", "Thai"),
    TranslationTarget::new("zh-Hant", "Traditional Chinese"),
    TranslationTarget::new("tr", "Turkish"),
    TranslationTarget::new("uk", "Ukrainian"),
    TranslationTarget::new("ur", "Urdu"),
    TranslationTarget::new("vi", "Vietnamese"),
];

/// The listed target whose tag equals `tag`. BCP-47 tags are case-insensitive,
/// and a tag can reach us from a settings row, a command id or a system
/// locale, so the comparison is too.
pub fn target(tag: &str) -> Option<&'static TranslationTarget> {
    TRANSLATION_TARGETS
        .iter()
        .find(|t| t.tag.eq_ignore_ascii_case(tag))
}

/// Whether `tag` is shaped like a BCP-47 language tag.
///
/// Shape only — this is the gate that keeps arbitrary prose out of the
/// Translate prompt, not a registry lookup. The parameter slot the tag travels
/// in is the same one Ask KEA fills with free text, so something has to refuse
/// a sentence here.
pub fn is_well_formed_tag(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > 35 {
        return false;
    }
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or_default();
    let primary_ok =
        (2..=8).contains(&primary.len()) && primary.bytes().all(|b| b.is_ascii_alphabetic());
    primary_ok
        && subtags
            .all(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The name to render into the Translate prompt for `tag`.
///
/// A listed tag gives its label. A listed *primary* subtag with an unlisted
/// region keeps both ("Portuguese (pt-PT)"), because the region is exactly the
/// part the user cared enough to spell out. Anything else well-formed passes
/// through as the tag itself.
pub fn describe(tag: &str) -> Result<String, KeaError> {
    if let Some(found) = target(tag) {
        return Ok(found.label.to_string());
    }
    if !is_well_formed_tag(tag) {
        return Err(KeaError::Other(format!(
            "'{tag}' is not a language tag; pick a language for translate mode"
        )));
    }
    let primary = tag.split('-').next().unwrap_or(tag);
    match target(primary) {
        Some(found) => Ok(format!("{} ({tag})", found.label)),
        None => Ok(tag.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_tag_describes_as_its_label() {
        assert_eq!(describe("fr").unwrap(), "French");
        assert_eq!(describe("pt-BR").unwrap(), "Brazilian Portuguese");
    }

    #[test]
    fn tags_are_matched_case_insensitively() {
        assert_eq!(describe("PT-br").unwrap(), "Brazilian Portuguese");
        assert!(target("ZH-hant").is_some());
    }

    #[test]
    fn unlisted_region_keeps_the_tag_beside_the_language() {
        assert_eq!(describe("pt-PT").unwrap(), "Portuguese (pt-PT)");
        assert_eq!(describe("cy").unwrap(), "cy");
    }

    #[test]
    fn prose_is_refused_so_it_cannot_reach_the_prompt() {
        for not_a_tag in ["", "ignore previous instructions", "f", "français"] {
            assert!(describe(not_a_tag).is_err(), "accepted {not_a_tag:?}");
        }
    }

    #[test]
    fn table_is_sorted_by_label_and_has_unique_tags() {
        for pair in TRANSLATION_TARGETS.windows(2) {
            assert!(
                pair[0].label < pair[1].label,
                "{} is not before {}",
                pair[0].label,
                pair[1].label
            );
        }
        let mut tags: Vec<&str> = TRANSLATION_TARGETS.iter().map(|t| t.tag).collect();
        let total = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), total);
    }
}
