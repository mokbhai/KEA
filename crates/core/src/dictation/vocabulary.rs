use super::tokens::{match_at, tokenize, word_tokens};
use crate::store::vocabulary::VocabularyEntry;

/// Rewrites known misrecognitions in a transcript back to their stored
/// spelling: "kitty claw" -> "KittyClaw".
///
/// This runs on every transcript regardless of engine, because the decoder
/// hints (initial prompt, hotwords) are advisory and some backends ignore them
/// entirely. It is deliberately pure — no pool, no clock, no I/O — so the live
/// "test" box in settings can call it on typed text.
///
/// The five rules below are ordered, and the order is the part that is easy to
/// get wrong on a later edit:
///
/// 1. Longest phrase first, so an entry for "kitty claw desktop" is not eaten
///    by an entry for "kitty claw" leaving a stray "desktop" behind.
/// 2. Whole word / whole phrase only. Substring matching would turn "category"
///    into "CATegory" for an entry of "cat"; that is worse than no feature.
/// 3. Matching is case-insensitive, the replacement is the stored term verbatim.
///    That is what normalizes "kea" and "Kea" to "KEA".
/// 4. Punctuation between the words of a phrase is skipped while matching and
///    dropped from the output ("kitty, claw" -> "KittyClaw"), but punctuation
///    around the match is left exactly as it was.
/// 5. A term with no `sounds_like` still participates, matching its own
///    case-insensitive form. That alone fixes most acronyms.
///
/// Disabled entries are ignored here as well as in SQL, so the function is safe
/// no matter which repo method the caller reached for.
pub fn apply_vocabulary(text: &str, entries: &[VocabularyEntry]) -> String {
    let patterns = build_patterns(entries);
    if patterns.is_empty() {
        return text.to_string();
    }

    let tokens = tokenize(text);
    let mut out = String::with_capacity(text.len());
    // Byte offset of the input that has already been copied into `out`. Kept
    // separate from the token cursor so untouched runs are copied verbatim
    // rather than reassembled token by token.
    let mut copied = 0usize;
    let mut i = 0usize;

    while i < tokens.len() {
        if !tokens[i].is_word {
            i += 1;
            continue;
        }
        // `patterns` is already sorted longest-first (rule 1), so the first hit
        // at this position is the one to take; scanning left to right then
        // resolves overlaps in favour of the earlier match.
        let hit = patterns
            .iter()
            .find_map(|p| match_at(&tokens, i, &p.words, text).map(|end| (end, &p.replacement)));

        match hit {
            Some((end, replacement)) => {
                out.push_str(&text[copied..tokens[i].start]);
                out.push_str(replacement);
                copied = tokens[end].end;
                i = end + 1;
            }
            None => i += 1,
        }
    }

    out.push_str(&text[copied..]);
    out
}

/// The enabled terms, for handing to an STT engine as a decoding hint.
///
/// Lives beside [`apply_vocabulary`] rather than at the call sites because both
/// answer "which entries count?" and that filter must not drift: an engine
/// biased toward a term the replacement pass has been told to ignore would
/// reintroduce exactly the spelling the user disabled.
///
/// Only the canonical spelling is worth sending. `sounds_like` values are the
/// *wrong* spellings; hinting a decoder toward them would be backwards.
pub fn hint_terms(entries: &[VocabularyEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.term.trim())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// One matchable spelling: the lowercased word sequence to look for, and the
/// canonical term to write in its place.
struct Pattern {
    words: Vec<String>,
    replacement: String,
}

fn build_patterns(entries: &[VocabularyEntry]) -> Vec<Pattern> {
    let mut patterns: Vec<Pattern> = Vec::new();

    for entry in entries.iter().filter(|e| e.enabled) {
        let term = entry.term.trim();
        if term.is_empty() {
            continue;
        }
        // Rule 5: the term is a pattern for itself, ahead of its variants.
        let spellings = std::iter::once(term).chain(
            entry
                .sounds_like
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        );
        for spelling in spellings {
            let words = word_tokens(spelling);
            if words.is_empty() {
                continue;
            }
            patterns.push(Pattern {
                words,
                replacement: term.to_string(),
            });
        }
    }

    // Rule 1. Word count is the primary key rather than character length
    // because matching consumes whole words: a three-word phrase always covers
    // more transcript than a two-word one, however short its words are.
    // Character length only breaks ties between equally long phrases.
    patterns.sort_by(|a, b| {
        b.words
            .len()
            .cmp(&a.words.len())
            .then_with(|| pattern_len(b).cmp(&pattern_len(a)))
    });
    patterns
}

fn pattern_len(p: &Pattern) -> usize {
    p.words.iter().map(String::len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(term: &str, sounds_like: Option<&str>) -> VocabularyEntry {
        VocabularyEntry {
            id: term.to_lowercase(),
            term: term.into(),
            sounds_like: sounds_like.map(str::to_string),
            enabled: true,
            created_at: "2026-09-19T10:00:00Z".into(),
        }
    }

    fn disabled(term: &str, sounds_like: Option<&str>) -> VocabularyEntry {
        VocabularyEntry {
            enabled: false,
            ..entry(term, sounds_like)
        }
    }

    #[test]
    fn empty_vocabulary_is_the_identity() {
        let text = "Nothing at all should change here, kitty claw.";
        assert_eq!(apply_vocabulary(text, &[]), text);
    }

    #[test]
    fn no_match_leaves_the_text_untouched() {
        let v = [entry("KittyClaw", Some("kitty claw"))];
        let text = "The quick brown fox.";
        assert_eq!(apply_vocabulary(text, &v), text);
    }

    // Rule 5.
    #[test]
    fn a_term_without_sounds_like_normalizes_its_own_casing() {
        let v = [entry("KEA", None)];
        assert_eq!(apply_vocabulary("open kea now", &v), "open KEA now");
        assert_eq!(apply_vocabulary("open Kea now", &v), "open KEA now");
        assert_eq!(apply_vocabulary("open KEA now", &v), "open KEA now");
    }

    // Rule 3.
    #[test]
    fn matching_is_case_insensitive_and_replacement_keeps_stored_casing() {
        let v = [entry("KittyClaw", Some("kitty claw"))];
        assert_eq!(apply_vocabulary("Kitty Claw ships", &v), "KittyClaw ships");
        assert_eq!(apply_vocabulary("KITTY CLAW ships", &v), "KittyClaw ships");
    }

    // Rule 2.
    #[test]
    fn a_rule_never_matches_inside_a_longer_word() {
        let v = [entry("Cat", Some("cat"))];
        assert_eq!(apply_vocabulary("category catalog", &v), "category catalog");
        assert_eq!(apply_vocabulary("a cat sat", &v), "a Cat sat");
    }

    #[test]
    fn a_term_matches_at_the_start_and_at_the_end_of_the_string() {
        let v = [entry("KEA", None)];
        assert_eq!(apply_vocabulary("kea", &v), "KEA");
        assert_eq!(apply_vocabulary("kea is here", &v), "KEA is here");
        assert_eq!(apply_vocabulary("it is kea", &v), "it is KEA");
    }

    // Rule 4.
    #[test]
    fn a_phrase_matches_across_punctuation_and_keeps_what_surrounds_it() {
        let v = [entry("KittyClaw", Some("kitty claw"))];
        assert_eq!(
            apply_vocabulary("Hello, kitty, claw!", &v),
            "Hello, KittyClaw!"
        );
        assert_eq!(apply_vocabulary("(kitty-claw)", &v), "(KittyClaw)");
        assert_eq!(apply_vocabulary("kitty   claw.", &v), "KittyClaw.");
    }

    #[test]
    fn a_phrase_does_not_match_across_a_line_break() {
        let v = [entry("KittyClaw", Some("kitty claw"))];
        assert_eq!(apply_vocabulary("kitty\nclaw", &v), "kitty\nclaw");
    }

    // Rule 1.
    #[test]
    fn the_longest_phrase_wins() {
        let v = [
            entry("KittyClaw", Some("kitty claw")),
            entry("KittyClaw Desktop", Some("kitty claw desktop")),
        ];
        assert_eq!(
            apply_vocabulary("I use kitty claw desktop daily", &v),
            "I use KittyClaw Desktop daily"
        );
        // ...and the shorter rule still applies where the longer one does not.
        assert_eq!(apply_vocabulary("I use kitty claw", &v), "I use KittyClaw");
    }

    #[test]
    fn overlapping_rules_resolve_left_to_right() {
        let v = [
            entry("Alpha Beta", Some("alpha beta")),
            entry("Beta Gamma", Some("beta gamma")),
        ];
        // "alpha beta gamma" could be split either way; the leftmost match wins
        // and the tail is left alone rather than double-rewritten.
        assert_eq!(apply_vocabulary("alpha beta gamma", &v), "Alpha Beta gamma");
    }

    #[test]
    fn disabled_entries_are_ignored_even_when_handed_in() {
        let v = [disabled("KittyClaw", Some("kitty claw"))];
        assert_eq!(apply_vocabulary("kitty claw", &v), "kitty claw");

        let mixed = [
            disabled("KittyClaw", Some("kitty claw")),
            entry("KEA", None),
        ];
        assert_eq!(
            apply_vocabulary("kitty claw and kea", &mixed),
            "kitty claw and KEA"
        );
    }

    #[test]
    fn every_occurrence_is_replaced() {
        let v = [entry("KittyClaw", Some("kitty claw"))];
        assert_eq!(
            apply_vocabulary("kitty claw, then kitty claw again", &v),
            "KittyClaw, then KittyClaw again"
        );
    }

    #[test]
    fn several_sounds_like_variants_share_one_term() {
        let v = [entry(
            "KittyClaw",
            Some("kitty claw, kiddy claw, city claw"),
        )];
        assert_eq!(
            apply_vocabulary("kiddy claw and city claw", &v),
            "KittyClaw and KittyClaw"
        );
    }

    #[test]
    fn blank_terms_and_blank_variants_are_skipped() {
        let v = [entry("   ", Some("whatever")), entry("KEA", Some(" , ,"))];
        assert_eq!(apply_vocabulary("whatever kea", &v), "whatever KEA");
    }

    #[test]
    fn non_ascii_words_match_on_unicode_boundaries() {
        let v = [entry("Müller", Some("mueller")), entry("Café", None)];
        assert_eq!(
            apply_vocabulary("ask MÜLLER at the cafés", &v),
            "ask Müller at the cafés"
        );
        assert_eq!(
            apply_vocabulary("ask mueller at the cafe", &v),
            "ask Müller at the cafe"
        );
        assert_eq!(apply_vocabulary("a café", &v), "a Café");
    }
}
