//! The word/separator token stream the transcript passes match over.
//!
//! Shared by [`super::vocabulary`] and [`super::commands`] rather than copied,
//! because the two passes have to agree on what a word is. They run one after
//! the other over the same transcript, and the ordering between them is only
//! meaningful if they draw word boundaries the same way — a phrase that
//! tokenized differently in each pass would disagree about where it starts.

/// One maximal run of either alphanumeric or non-alphanumeric characters.
pub(crate) struct Token {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) is_word: bool,
    /// Lowercased slice, for word tokens only; empty for separators.
    pub(crate) lower: String,
}

/// Splits into alternating runs of alphanumeric and non-alphanumeric
/// characters. `char::is_alphanumeric` is Unicode-aware, which is what gives
/// whole-word matching its boundaries for free at either end of the string — a
/// run is maximal, so a single-word pattern can only ever match a whole word.
///
/// The alternation is load-bearing for both callers: given a word token at `i`,
/// `i - 1` and `i + 1` are separators and `i - 2` / `i + 2` are the neighbouring
/// words, with no scanning.
pub(crate) fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    for (i, ch) in text.char_indices() {
        let is_word = ch.is_alphanumeric();
        let end = i + ch.len_utf8();
        match tokens.last_mut() {
            Some(last) if last.is_word == is_word => last.end = end,
            _ => tokens.push(Token {
                start: i,
                end,
                is_word,
                lower: String::new(),
            }),
        }
    }
    for token in tokens.iter_mut().filter(|t| t.is_word) {
        token.lower = text[token.start..token.end].to_lowercase();
    }
    tokens
}

/// The lowercased words of a phrase, dropping whatever separates them.
pub(crate) fn word_tokens(spelling: &str) -> Vec<String> {
    tokenize(spelling)
        .into_iter()
        .filter(|t| t.is_word)
        .map(|t| t.lower)
        .collect()
}

/// Tries `words` against the token stream starting at `start`. Returns the
/// index of the last token consumed, which is always a word token — so a
/// matched span never swallows trailing punctuation.
pub(crate) fn match_at(
    tokens: &[Token],
    start: usize,
    words: &[String],
    text: &str,
) -> Option<usize> {
    let mut idx = start;
    for (n, word) in words.iter().enumerate() {
        if n > 0 {
            // Exactly one separator run sits between any two word tokens, and
            // any of it may be skipped — except a line break. Crossing one
            // would let a phrase match across a paragraph gap, where two
            // adjacent words are almost certainly unrelated.
            let gap = tokens.get(idx)?;
            if gap.is_word || text[gap.start..gap.end].contains(['\n', '\r']) {
                return None;
            }
            idx += 1;
        }
        let token = tokens.get(idx)?;
        if !token.is_word || token.lower != *word {
            return None;
        }
        if n + 1 < words.len() {
            idx += 1;
        }
    }
    Some(idx)
}
