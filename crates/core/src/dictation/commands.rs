//! The deterministic voice-command pass: spoken editing instructions executed
//! during dictation instead of typed.
//!
//! **The feature is the false positives, not the commands.** Someone dictating
//! documentation says "press return for a new paragraph" and must get those
//! seven words; someone dictating prose says "the period between the wars".
//! A pass that silently eats either is worse than no pass at all, because the
//! user cannot tell it apart from a transcription error. So the matcher is an
//! exact-phrase table — never fuzzy — behind four rules that each have to pass
//! before a phrase is treated as a command:
//!
//! 1. **Position.** A `Trailing` mark has to have something to attach to, so
//!    it needs preceding content. This is what keeps "period costume drama"
//!    literal while "hello period world" becomes "hello. world".
//! 2. **Leading-word denylist.** A determiner or a quoting verb immediately
//!    before the phrase, in the same clause, forces it literal: "add **a**
//!    comma", "press return for **a** new paragraph", "**the** period".
//! 3. **Escape phrase.** `literally <command>` and `the words <command>` always
//!    emit the phrase verbatim and consume only the escape token. This is the
//!    user's sole recourse when the rules get it wrong, so nothing may
//!    pre-empt it — it is checked before everything else.
//! 4. **Per-command enable.** [`CommandFamily::default_enabled`] ships
//!    punctuation and retraction on and the structure commands off, because
//!    "new line" and "new paragraph" are the phrases people most often say
//!    literally. That default is not a nicety: the people who dictate because
//!    typing is hard are the ones a false positive hurts most, and they have
//!    the least recourse.
//!
//! The fifth mitigation, undo, is [`apply_voice_commands_except`]: the pass is
//! a pure function of the raw transcript, so "undo that command" is re-running
//! it with one span suppressed rather than mutating the result. That also
//! makes it trivially idempotent over a growing streaming prefix — re-run it
//! on the whole raw text every time and diff the output.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::settings::VoiceCommandSettings;
use super::tokens::{match_at, tokenize, word_tokens, Token};

/// The `id` reported for a consumed escape phrase.
///
/// It is in `applied` rather than silent because an escape *changes the text*
/// — it drops the marker — and the HUD's rule is that anything which changed
/// the transcript says so.
pub const ESCAPE_ID: &str = "literal";

/// Which of the three command families a phrase belongs to.
///
/// The family, not the individual command, owns the default: the argument for
/// shipping structure commands off is an argument about that whole family, and
/// writing it once is what stops a later addition quietly defaulting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandFamily {
    Punctuation,
    Structure,
    Retraction,
}

impl CommandFamily {
    /// Whether a command of this family is on for a user who has never opened
    /// the settings table.
    pub fn default_enabled(self) -> bool {
        match self {
            // Saying "period" and meaning the mark is the overwhelmingly
            // common case, and the denylist covers the noun reading.
            CommandFamily::Punctuation | CommandFamily::Retraction => true,
            // "new paragraph" and "new line" are ordinary English about text.
            // Let them be turned on once the table has been seen.
            CommandFamily::Structure => false,
        }
    }
}

/// How an inserted glyph sits against the words around it.
///
/// Spelled out per command rather than inferred, because it is also the
/// position rule: only a mark that hugs the word *before* it needs one to
/// exist, and that single fact is what separates "hello period world" from
/// "period costume drama".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// Hugs the preceding word, one space before the next: `.` `,` `?`
    Trailing,
    /// Hugs the following word: an opening quote.
    Leading,
    /// A space on both sides: an em dash.
    Surrounded,
    /// No space on either side: a hyphen, and the structure breaks.
    Tight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    Insert {
        glyph: &'static str,
        attach: Attach,
    },
    /// Delete back to the start of the preceding sentence.
    Retract,
}

/// One command: its stable id, the phrases that invoke it, and what it does.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Stable across releases: it is the value persisted in the enabled set
    /// and the key the settings table renders rows by.
    pub id: &'static str,
    /// Every spelling that invokes it, each matched as a whole word sequence.
    pub phrases: &'static [&'static str],
    pub family: CommandFamily,
    pub action: CommandAction,
    /// What the command puts in the transcript, for the settings table. A
    /// glyph for the punctuation marks, prose for the ones a glyph cannot
    /// show.
    pub produces: &'static str,
}

impl CommandSpec {
    /// The phrase to show in the settings table: the first spelling, which is
    /// the one the table is written to lead with.
    pub fn primary_phrase(&self) -> &'static str {
        self.phrases.first().copied().unwrap_or(self.id)
    }
}

/// The English command table.
///
/// Kept short on purpose. Every phrase added here is a new class of false
/// positive, and the whole value of an exact table over a fuzzy matcher is
/// that a user can predict its failures.
const EN_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "period",
        phrases: &["period", "full stop"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: ".",
            attach: Attach::Trailing,
        },
        produces: ".",
    },
    CommandSpec {
        id: "comma",
        phrases: &["comma"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: ",",
            attach: Attach::Trailing,
        },
        produces: ",",
    },
    CommandSpec {
        id: "question_mark",
        phrases: &["question mark"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "?",
            attach: Attach::Trailing,
        },
        produces: "?",
    },
    CommandSpec {
        id: "exclamation",
        phrases: &["exclamation point", "exclamation mark"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "!",
            attach: Attach::Trailing,
        },
        produces: "!",
    },
    CommandSpec {
        id: "colon",
        phrases: &["colon"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: ":",
            attach: Attach::Trailing,
        },
        produces: ":",
    },
    CommandSpec {
        id: "semicolon",
        phrases: &["semicolon", "semi colon"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: ";",
            attach: Attach::Trailing,
        },
        produces: ";",
    },
    CommandSpec {
        id: "ellipsis",
        phrases: &["ellipsis"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "…",
            attach: Attach::Trailing,
        },
        produces: "…",
    },
    CommandSpec {
        id: "open_quote",
        phrases: &["open quote"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "\u{201c}",
            attach: Attach::Leading,
        },
        produces: "\u{201c}",
    },
    CommandSpec {
        id: "close_quote",
        phrases: &["close quote"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "\u{201d}",
            attach: Attach::Trailing,
        },
        produces: "\u{201d}",
    },
    CommandSpec {
        id: "hyphen",
        phrases: &["hyphen"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "-",
            attach: Attach::Tight,
        },
        produces: "-",
    },
    CommandSpec {
        id: "dash",
        phrases: &["dash", "em dash"],
        family: CommandFamily::Punctuation,
        action: CommandAction::Insert {
            glyph: "\u{2014}",
            attach: Attach::Surrounded,
        },
        produces: "\u{2014}",
    },
    CommandSpec {
        id: "new_line",
        phrases: &["new line"],
        family: CommandFamily::Structure,
        action: CommandAction::Insert {
            glyph: "\n",
            attach: Attach::Tight,
        },
        produces: "a line break",
    },
    CommandSpec {
        id: "new_paragraph",
        phrases: &["new paragraph"],
        family: CommandFamily::Structure,
        action: CommandAction::Insert {
            glyph: "\n\n",
            attach: Attach::Tight,
        },
        produces: "a blank line",
    },
    CommandSpec {
        id: "scratch_that",
        phrases: &["scratch that", "delete that"],
        family: CommandFamily::Retraction,
        action: CommandAction::Retract,
        produces: "deletes the sentence you just said",
    },
];

/// Phrases that force the words after them to be read literally.
///
/// The plan's nine, plus the determiners and possessives that give a
/// punctuation word its noun reading ("**this** period was long"). It is a
/// table and not a heuristic on purpose: a user can learn a table.
///
/// `that` is deliberately **absent**, even though it is a determiner. It is
/// also the last word of both retraction phrases, so "I don't like that,
/// scratch that" would block the one command whose failure costs the most —
/// and the clause rule below already stops it binding across the comma anyway.
const EN_DENY: &[&str] = &[
    // The plan's list: quoting verbs and the two commonest determiners.
    "a", "the", "for", "say", "type", "write", "insert", "word", "literal",
    // Determiners and possessives, which only ever precede a noun reading.
    "an", "this", "these", "those", "each", "every", "another", "my", "your", "our", "their", "his",
    "her", "its",
];

/// The escape markers, longest first so "the words" wins over nothing.
const EN_ESCAPES: &[&str] = &["the words", "literally"];

/// The primary language subtag a command table is keyed by: "en" from "en-US".
///
/// A newtype rather than a bare `String` so the table lookup cannot be handed
/// a full locale by accident, which would miss every time and disable the pass
/// silently.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LangTag(String);

impl LangTag {
    pub fn parse(tag: &str) -> Self {
        Self(
            tag.split(['-', '_'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        )
    }

    pub fn english() -> Self {
        Self("en".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One language's matcher: the specs, their phrases pre-tokenized and sorted
/// longest-first, the escape markers and the denylist.
pub struct CommandTable {
    specs: &'static [CommandSpec],
    phrases: Vec<Phrase>,
    escapes: Vec<Vec<String>>,
    deny: BTreeSet<&'static str>,
}

struct Phrase {
    words: Vec<String>,
    spec: &'static CommandSpec,
}

impl CommandTable {
    fn build(specs: &'static [CommandSpec], escapes: &[&str], deny: &[&'static str]) -> Self {
        let mut phrases: Vec<Phrase> = specs
            .iter()
            .flat_map(|spec| {
                spec.phrases.iter().filter_map(move |p| {
                    let words = word_tokens(p);
                    (!words.is_empty()).then_some(Phrase { words, spec })
                })
            })
            .collect();
        // Longest first, so "full stop" is never eaten by a hypothetical
        // one-word prefix and "exclamation point" beats "exclamation".
        phrases.sort_by(|a, b| b.words.len().cmp(&a.words.len()));
        Self {
            specs,
            phrases,
            escapes: escapes.iter().map(|e| word_tokens(e)).collect(),
            deny: deny.iter().copied().collect(),
        }
    }

    /// Every command in this language, in table order.
    pub fn specs(&self) -> &'static [CommandSpec] {
        self.specs
    }

    pub fn spec(&self, id: &str) -> Option<&'static CommandSpec> {
        self.specs.iter().find(|s| s.id == id)
    }
}

fn tables() -> &'static HashMap<LangTag, CommandTable> {
    static TABLES: OnceLock<HashMap<LangTag, CommandTable>> = OnceLock::new();
    TABLES.get_or_init(|| {
        // A map from the first commit even though only English is populated:
        // adding German is then a table, not a refactor.
        HashMap::from([(
            LangTag::english(),
            CommandTable::build(EN_COMMANDS, EN_ESCAPES, EN_DENY),
        )])
    })
}

/// The table for a language, or `None` when there is none — which is the
/// honest answer for every language but English today.
pub fn table_for(lang: &LangTag) -> Option<&'static CommandTable> {
    tables().get(lang)
}

/// Whether a model id names a build that can only produce English.
///
/// Whisper's catalog spells those with a `.en` suffix ("ggml-base.en") and
/// everything else in it is multilingual. Anything this cannot recognise is
/// reported as *not* English-only, because the cost of being wrong here is
/// firing English phrase matching at German — and a German transcript that
/// happens to contain "komma" is not the user's problem to debug.
pub fn model_is_english_only(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.ends_with(".en") || model.ends_with("-en")
}

/// Which command table, if any, may run over this transcript.
///
/// `language` is the `dictation.language` setting — `None` means auto-detect.
/// `model` is the speech model the run actually bound.
///
/// Auto-detect plus a multilingual model resolves to *off*. The command list
/// is English-only, and running it against a transcript the user asked to be
/// decoded as German would be the same class of defect as a control that
/// writes a setting nothing reads: it looks like a feature and behaves like
/// corruption.
pub fn resolve_language(language: Option<&str>, model: Option<&str>) -> Option<LangTag> {
    match language {
        Some(tag) => {
            let tag = LangTag::parse(tag);
            table_for(&tag).is_some().then_some(tag)
        }
        None => model
            .is_some_and(model_is_english_only)
            .then(LangTag::english),
    }
}

/// Which commands run, for one dictation run.
///
/// Carries the resolved language rather than the raw setting so the gate is
/// decided once, where the bound model is known, instead of at each call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceCommandConfig {
    lang: Option<LangTag>,
    enabled_ids: BTreeSet<String>,
}

impl Default for VoiceCommandConfig {
    fn default() -> Self {
        Self::off()
    }
}

impl VoiceCommandConfig {
    /// The pass does nothing at all. Every caller that has not resolved a
    /// language yet uses this, so "not wired up" and "gated off" are the same
    /// code path.
    pub fn off() -> Self {
        Self {
            lang: None,
            enabled_ids: BTreeSet::new(),
        }
    }

    pub fn new(lang: LangTag, enabled_ids: BTreeSet<String>) -> Self {
        Self {
            lang: Some(lang),
            enabled_ids,
        }
    }

    /// The per-family defaults, for a user who has never opened the table.
    pub fn defaults_for(lang: LangTag) -> Self {
        let enabled_ids = table_for(&lang)
            .map(|t| {
                t.specs()
                    .iter()
                    .filter(|s| s.family.default_enabled())
                    .map(|s| s.id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        Self::new(lang, enabled_ids)
    }

    /// Folds the two stored settings and the run's language gate into one
    /// config.
    ///
    /// The master switch collapses into "no language": there is one way for
    /// the pass to be off, so no caller can have it half on.
    pub fn resolve(
        settings: &VoiceCommandSettings,
        language: Option<&str>,
        model: Option<&str>,
    ) -> Self {
        if !settings.enabled {
            return Self::off();
        }
        let Some(lang) = resolve_language(language, model) else {
            return Self::off();
        };
        match &settings.enabled_ids {
            // An empty array means the user turned everything off. Only a
            // missing key means "never configured", and only that takes the
            // defaults.
            Some(ids) => Self::new(lang, ids.iter().cloned().collect()),
            None => Self::defaults_for(lang),
        }
    }

    pub fn table(&self) -> Option<&'static CommandTable> {
        self.lang.as_ref().and_then(table_for)
    }

    pub fn language(&self) -> Option<&LangTag> {
        self.lang.as_ref()
    }

    pub fn is_on(&self, id: &str) -> bool {
        self.enabled_ids.contains(id)
    }

    /// Whether any command could fire. `false` lets a caller skip the pass
    /// entirely, and is also what the log line reports.
    pub fn is_active(&self) -> bool {
        self.table().is_some() && !self.enabled_ids.is_empty()
    }
}

/// One command that fired, in transcript order.
///
/// `start`/`end` are byte offsets into the **raw** transcript, which is what
/// makes the pass reversible: feed the span back through
/// [`apply_voice_commands_except`] and this occurrence is left as spoken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedCommand {
    /// A [`CommandSpec::id`], or [`ESCAPE_ID`].
    pub id: String,
    /// The phrase as it was spoken, for the HUD's transient line.
    pub phrase: String,
    pub start: usize,
    pub end: usize,
}

impl AppliedCommand {
    pub fn source(&self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VoiceCommandResult {
    pub text: String,
    pub applied: Vec<AppliedCommand>,
}

/// Executes the enabled voice commands in `text`.
///
/// Pure, infallible and total: an un-gated config, an empty transcript and a
/// transcript with no commands in it all return the input unchanged. That is
/// what lets the settings page run it on typed text and the streaming path
/// re-run it on every partial.
///
/// **"The preceding sentence"**, which retraction deletes, is defined as
/// everything back to the nearest of: a sentence-ending mark (`.`, `!`, `?`),
/// a line break, or the start of the transcript — whether that mark was
/// spoken as a command or already in the transcript. When the text already
/// ends on such a boundary, the sentence removed is the completed one,
/// boundary included; that is what makes two consecutive "scratch that"s
/// remove two sentences.
pub fn apply_voice_commands(text: &str, cfg: &VoiceCommandConfig) -> VoiceCommandResult {
    apply_voice_commands_except(text, cfg, &[])
}

/// [`apply_voice_commands`], with the occurrences at `suppressed` left exactly
/// as spoken.
///
/// This is undo. The pass is a pure function of the raw transcript, so undoing
/// a command is re-running the whole thing with that one span held back rather
/// than trying to invert an edit — which means undo cannot drift from apply,
/// and suppressing every span in `applied` returns the raw transcript
/// verbatim.
pub fn apply_voice_commands_except(
    text: &str,
    cfg: &VoiceCommandConfig,
    suppressed: &[Range<usize>],
) -> VoiceCommandResult {
    let Some(table) = cfg.table() else {
        return VoiceCommandResult {
            text: text.to_string(),
            applied: Vec::new(),
        };
    };

    let tokens = tokenize(text);
    let mut out = String::with_capacity(text.len() + 8);
    let mut applied: Vec<AppliedCommand> = Vec::new();
    // Byte offset of the input already copied into `out`. Kept separate from
    // the token cursor so untouched runs are copied verbatim rather than
    // reassembled token by token — that verbatim copy is what makes the
    // suppress-everything round trip exact.
    let mut copied = 0usize;
    let mut i = 0usize;

    while i < tokens.len() {
        if !tokens[i].is_word {
            i += 1;
            continue;
        }

        // Mitigation 3, first: the escape phrase is the user's only recourse,
        // so no other rule may pre-empt it. It matches against the whole
        // table, not just the enabled commands — "literally new paragraph"
        // must drop the marker whether or not that command is switched on.
        if let Some(escape) = match_escape(table, &tokens, i, text) {
            let source = tokens[i].start..tokens[escape.phrase_end].end;
            if !is_suppressed(suppressed, &source) {
                let phrase =
                    &text[tokens[escape.phrase_start].start..tokens[escape.phrase_end].end];
                out.push_str(&text[copied..tokens[i].start]);
                out.push_str(phrase);
                copied = tokens[escape.phrase_end].end;
                applied.push(AppliedCommand {
                    id: ESCAPE_ID.to_string(),
                    phrase: phrase.to_string(),
                    start: source.start,
                    end: source.end,
                });
            }
            // Either way the escaped phrase is literal, so step past it rather
            // than letting the command matcher have another go at it.
            i = escape.phrase_end + 1;
            continue;
        }

        let Some((end, spec)) = match_command(table, &tokens, i, cfg, text) else {
            i += 1;
            continue;
        };

        let source = tokens[i].start..tokens[end].end;
        if is_suppressed(suppressed, &source) {
            i = end + 1;
            continue;
        }

        // Mitigation 2. Only a leading word in the same clause binds: a
        // determiner cannot reach across a full stop or a comma to make the
        // next phrase a noun.
        if leading_word(&tokens, i, text).is_some_and(|w| table.deny.contains(w)) {
            i += 1;
            continue;
        }

        // Mitigation 1.
        let pending = &text[copied.min(tokens[i].start)..tokens[i].start];
        if !position_allows(spec, &out, pending) {
            i += 1;
            continue;
        }

        // Everything before the phrase. A command absorbs the separator run in
        // front of it — that is how "hello period" becomes "hello." and not
        // "hello ." — except a leading mark, which hugs the word after it and
        // wants that separator kept.
        let keeps_leading_space = i == 0
            || matches!(
                spec.action,
                CommandAction::Insert {
                    attach: Attach::Leading,
                    ..
                }
            )
            // A line break in front of the phrase is structure the user
            // dictated, not spacing noise. Absorbing it would silently merge
            // two paragraphs, which is a worse edit than a mark that does not
            // quite hug its word.
            || text[tokens[i - 1].start..tokens[i - 1].end].contains(['\n', '\r']);
        let flush_to = if keeps_leading_space {
            tokens[i].start
        } else {
            tokens[i - 1].start
        };
        out.push_str(&text[copied..flush_to.max(copied)]);

        match spec.action {
            CommandAction::Insert { glyph, attach } => {
                if attach == Attach::Surrounded && !out.is_empty() && !ends_with_space(&out) {
                    out.push(' ');
                }
                out.push_str(glyph);
            }
            CommandAction::Retract => retract_one(&mut out),
        }

        applied.push(AppliedCommand {
            id: spec.id.to_string(),
            phrase: text[source.clone()].to_string(),
            start: source.start,
            end: source.end,
        });

        // The separator after the phrase is replaced by whatever the mark
        // wants, so "one comma  two" cannot keep the doubled space the
        // decoder happened to emit.
        let trailing = tokens.get(end + 1).filter(|t| !t.is_word);
        copied = trailing.map_or(tokens[end].end, |t| t.end);
        let next_word = tokens.len() > end + 2;
        if next_word {
            let raw = trailing.map_or("", |t| &text[t.start..t.end]);
            join_after(&mut out, spec.action, raw);
        }

        i = end + 1;
    }

    out.push_str(&text[copied.min(text.len())..]);
    VoiceCommandResult { text: out, applied }
}

/// Re-runs the pass with the most recent command left as spoken.
///
/// The HUD's "undo last command": one call, the whole result rebuilt, no
/// incremental state to get out of step.
pub fn undo_last(
    text: &str,
    cfg: &VoiceCommandConfig,
    applied: &[AppliedCommand],
) -> VoiceCommandResult {
    match applied.last() {
        Some(last) => apply_voice_commands_except(text, cfg, &[last.source()]),
        None => apply_voice_commands(text, cfg),
    }
}

/// Where an escape phrase's payload sits in the token stream.
struct EscapeHit {
    phrase_start: usize,
    phrase_end: usize,
}

fn match_escape(table: &CommandTable, tokens: &[Token], i: usize, text: &str) -> Option<EscapeHit> {
    for marker in &table.escapes {
        let Some(marker_end) = match_at(tokens, i, marker, text) else {
            continue;
        };
        // The payload is the next word token; the separator between is
        // whatever the decoder emitted.
        let phrase_start = marker_end + 2;
        let hit = table
            .phrases
            .iter()
            .find_map(|p| match_at(tokens, phrase_start, &p.words, text));
        if let Some(phrase_end) = hit {
            return Some(EscapeHit {
                phrase_start,
                phrase_end,
            });
        }
    }
    None
}

fn match_command(
    table: &CommandTable,
    tokens: &[Token],
    i: usize,
    cfg: &VoiceCommandConfig,
    text: &str,
) -> Option<(usize, &'static CommandSpec)> {
    table
        .phrases
        .iter()
        .filter(|p| cfg.is_on(p.spec.id))
        .find_map(|p| match_at(tokens, i, &p.words, text).map(|end| (end, p.spec)))
}

/// The word immediately before `i`, or `None` when a clause boundary or the
/// start of the transcript sits between them.
fn leading_word<'a>(tokens: &'a [Token], i: usize, text: &str) -> Option<&'a str> {
    if i < 2 {
        return None;
    }
    let gap = &text[tokens[i - 1].start..tokens[i - 1].end];
    // A determiner binds to the next noun only inside its own clause. Without
    // this, "I don't like that, scratch that" would read "that" as a leading
    // word and refuse the retraction.
    if gap.contains(['.', ',', ';', ':', '!', '?', '\n', '\r']) {
        return None;
    }
    Some(tokens[i - 2].lower.as_str())
}

/// Mitigation 1: a mark that hugs the word before it needs one to exist.
fn position_allows(spec: &CommandSpec, out: &str, pending: &str) -> bool {
    match spec.action {
        CommandAction::Insert {
            attach: Attach::Trailing | Attach::Surrounded,
            ..
        } => !out.trim_end().is_empty() || !pending.trim().is_empty(),
        // A retraction with nothing behind it removes nothing, which is the
        // right answer rather than a reason to type the words; a leading mark
        // and a structure break are both legitimate at the very start.
        _ => true,
    }
}

/// What goes between a command's output and the next word.
fn join_after(out: &mut String, action: CommandAction, raw_separator: &str) {
    match action {
        CommandAction::Insert {
            attach: Attach::Trailing | Attach::Surrounded,
            ..
        } => {
            // A paragraph break the transcript already had outranks the single
            // space: it was structure the user dictated, not spacing noise.
            if raw_separator.contains(['\n', '\r']) {
                out.push_str(raw_separator);
            } else {
                out.push(' ');
            }
        }
        // A leading quote, a hyphen and the structure breaks all hug what
        // follows.
        CommandAction::Insert { .. } => {}
        CommandAction::Retract => {
            if !out.is_empty() && !ends_with_space(out) {
                out.push(' ');
            }
        }
    }
}

fn ends_with_space(out: &str) -> bool {
    out.ends_with(char::is_whitespace)
}

fn is_sentence_end(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\n' | '\r')
}

/// Deletes the sentence at the end of `out`, as documented on
/// [`apply_voice_commands`].
fn retract_one(out: &mut String) {
    // Drop the trailing whitespace and the mark that closes the sentence being
    // removed. Without this step "one. two." would find its own full stop and
    // retract nothing at all.
    let body_end = out
        .trim_end_matches(|c: char| c.is_whitespace() || is_sentence_end(c))
        .len();
    let head = &out[..body_end];
    let start = match head.rfind(is_sentence_end) {
        Some(idx) => idx + head[idx..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    out.truncate(start);
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
}

fn is_suppressed(suppressed: &[Range<usize>], span: &Range<usize>) -> bool {
    suppressed
        .iter()
        .any(|s| s.start < span.end && span.start < s.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipping defaults: punctuation and retraction on, structure off.
    fn default_cfg() -> VoiceCommandConfig {
        VoiceCommandConfig::defaults_for(LangTag::english())
    }

    /// Everything on, for the rows that are about the structure family.
    fn all_on() -> VoiceCommandConfig {
        let ids = EN_COMMANDS.iter().map(|s| s.id.to_string()).collect();
        VoiceCommandConfig::new(LangTag::english(), ids)
    }

    fn run(text: &str, cfg: &VoiceCommandConfig) -> String {
        apply_voice_commands(text, cfg).text
    }

    /// The table the whole feature is judged by. Every row that says
    /// "unchanged" is a literal-usage trap — a sentence a real person dictates
    /// — and those are the rows that matter.
    #[test]
    fn the_command_table() {
        let d = default_cfg();
        let cases: &[(&str, &str, &str)] = &[
            ("hello world period", "hello world.", "a mark at the end"),
            ("hello period world", "hello. world", "a mark mid-sentence"),
            ("one comma two comma three", "one, two, three", "repeated"),
            ("scratch that", "", "a retraction with nothing behind it"),
            (
                "the meeting is monday scratch that tuesday",
                "tuesday",
                "back to the start of the transcript",
            ),
            (
                "press return for a new paragraph",
                "press return for a new paragraph",
                "'a' precedes, and the structure family is off anyway",
            ),
            (
                "type the words scratch that",
                "type scratch that",
                "the escape consumes only its own marker",
            ),
            ("literally period", "period", "the escape phrase"),
            ("add a comma here", "add a comma here", "'a' precedes"),
            (
                "this period was long",
                "this period was long",
                "a determiner gives it the noun reading",
            ),
            (
                "period costume drama",
                "period costume drama",
                "nothing to attach to",
            ),
            ("new paragraph", "new paragraph", "the command is off"),
            ("", "", "empty in, empty out"),
            (
                "say period out loud",
                "say period out loud",
                "a quoting verb precedes",
            ),
            (
                "he wrote the word comma",
                "he wrote the word comma",
                "'word' precedes",
            ),
            (
                "we met on tuesday full stop she was late",
                "we met on tuesday. she was late",
                "the second spelling of the same command",
            ),
            (
                "is it ready question mark",
                "is it ready?",
                "a two-word punctuation phrase",
            ),
            (
                "he said open quote hello close quote",
                "he said \u{201c}hello\u{201d}",
                "quotes hug the words they wrap",
            ),
            (
                "one dash two",
                "one \u{2014} two",
                "an em dash takes a space on each side",
            ),
            ("nine hyphen ten", "nine-ten", "a hyphen takes neither"),
        ];

        for (input, expected, why) in cases {
            assert_eq!(run(input, &d), *expected, "{why}: {input:?}");
        }
    }

    /// The structure family, which only these rows switch on.
    #[test]
    fn the_structure_commands_when_they_are_enabled() {
        let cfg = all_on();
        assert_eq!(run("new paragraph", &cfg), "\n\n");
        assert_eq!(run("one new paragraph two", &cfg), "one\n\ntwo");
        assert_eq!(run("one new line two", &cfg), "one\ntwo");
        // Still literal: the denylist does not care which family it guards.
        assert_eq!(
            run("press return for a new paragraph", &cfg),
            "press return for a new paragraph"
        );
        assert_eq!(
            run("start a new paragraph here", &cfg),
            "start a new paragraph here"
        );
    }

    /// Two retractions in a row take two sentences, which is only true because
    /// retraction resolves over text the punctuation commands have already
    /// closed.
    #[test]
    fn consecutive_retractions_remove_consecutive_sentences() {
        let d = default_cfg();
        assert_eq!(run("one period two period scratch that", &d), "one.");
        assert_eq!(
            run("one period two period scratch that scratch that", &d),
            ""
        );
        // The same over punctuation the decoder produced by itself, which is
        // what the definition in the doc comment means by "already in the
        // transcript".
        assert_eq!(run("One. Two. scratch that", &d), "One.");
        assert_eq!(run("One. Two. delete that", &d), "One.");
    }

    /// A retraction mid-transcript leaves one space, not the decoder's.
    #[test]
    fn text_after_a_retraction_is_spaced_once() {
        let d = default_cfg();
        assert_eq!(run("one period two scratch that three", &d), "one. three");
    }

    /// The commands a HUD would name, in the order they fired.
    #[test]
    fn applied_lists_every_command_in_order() {
        let got = apply_voice_commands(
            "one comma two period literally period scratch that",
            &default_cfg(),
        );
        let ids: Vec<&str> = got.applied.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["comma", "period", ESCAPE_ID, "scratch_that"]);
        let phrases: Vec<&str> = got.applied.iter().map(|a| a.phrase.as_str()).collect();
        assert_eq!(phrases, vec!["comma", "period", "period", "scratch that"]);
        // Spans are disjoint and in transcript order, which is what lets a
        // caller suppress any subset of them.
        for pair in got.applied.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{:?}", got.applied);
        }
    }

    /// Undo. Suppressing every span the pass reported has to give back the raw
    /// transcript character for character — that is the property that makes
    /// "insert what I actually said" trustworthy.
    #[test]
    fn suppressing_every_applied_command_is_the_identity() {
        let cfg = all_on();
        let inputs = [
            "hello world period",
            "one comma two comma three",
            "the meeting is monday scratch that tuesday",
            "literally period",
            "type the words scratch that",
            "one new paragraph two period three",
            "he said open quote hello close quote period",
            "one period two period scratch that scratch that",
            "nothing to do here",
            "",
        ];
        for input in inputs {
            let first = apply_voice_commands(input, &cfg);
            let spans: Vec<Range<usize>> = first.applied.iter().map(|a| a.source()).collect();
            let undone = apply_voice_commands_except(input, &cfg, &spans);
            assert_eq!(undone.text, input, "round trip for {input:?}");
            assert!(undone.applied.is_empty(), "round trip for {input:?}");
        }
    }

    /// Undoing only the last one leaves the earlier commands applied.
    #[test]
    fn undo_last_keeps_the_earlier_commands() {
        let cfg = default_cfg();
        let input = "hello period world period";
        let first = apply_voice_commands(input, &cfg);
        assert_eq!(first.text, "hello. world.");
        let undone = undo_last(input, &cfg, &first.applied);
        assert_eq!(undone.text, "hello. world period");
    }

    /// Streaming: the pass is re-run over the whole raw prefix every time, so
    /// a phrase that is only half-spoken cannot half-fire and cannot be
    /// double-applied when the rest arrives.
    #[test]
    fn a_growing_prefix_never_double_applies() {
        let cfg = default_cfg();
        let steps = [
            ("hello", "hello"),
            ("hello period", "hello."),
            ("hello period world", "hello. world"),
            ("hello period world scratch", "hello. world scratch"),
            ("hello period world scratch that", "hello."),
        ];
        for (raw, expected) in steps {
            assert_eq!(run(raw, &cfg), expected, "prefix {raw:?}");
        }
        // And running it twice over the same raw text is the same answer.
        let once = run("hello period world scratch that", &cfg);
        assert_eq!(run("hello period world scratch that", &cfg), once);
    }

    /// The language gate, which is the honest half of "English-only".
    #[test]
    fn the_language_gate() {
        let on = VoiceCommandSettings {
            enabled: true,
            enabled_ids: None,
        };
        // An explicit English tag, in either spelling.
        assert!(VoiceCommandConfig::resolve(&on, Some("en"), None).is_active());
        assert!(VoiceCommandConfig::resolve(&on, Some("en-US"), None).is_active());
        // An explicit other language: off, and not "off but pretending".
        assert!(!VoiceCommandConfig::resolve(&on, Some("de"), None).is_active());
        assert!(!VoiceCommandConfig::resolve(&on, Some("de"), Some("ggml-base.en")).is_active());
        // Auto-detect: only an English-only build qualifies.
        assert!(VoiceCommandConfig::resolve(&on, None, Some("ggml-base.en")).is_active());
        assert!(!VoiceCommandConfig::resolve(&on, None, Some("ggml-large-v3")).is_active());
        assert!(!VoiceCommandConfig::resolve(&on, None, None).is_active());
        // The master switch beats everything.
        let off = VoiceCommandSettings {
            enabled: false,
            enabled_ids: None,
        };
        assert!(!VoiceCommandConfig::resolve(&off, Some("en"), None).is_active());
    }

    /// A gated-off config is a no-op, not a partially applied pass.
    #[test]
    fn a_gated_off_config_returns_the_transcript_verbatim() {
        let got = apply_voice_commands("hello world period", &VoiceCommandConfig::off());
        assert_eq!(got.text, "hello world period");
        assert!(got.applied.is_empty());
    }

    /// An explicitly empty set is "everything off", which is not the same as
    /// the key never having been written.
    #[test]
    fn an_empty_enabled_set_is_not_the_defaults() {
        let cleared = VoiceCommandSettings {
            enabled: true,
            enabled_ids: Some(Vec::new()),
        };
        let cfg = VoiceCommandConfig::resolve(&cleared, Some("en"), None);
        assert!(!cfg.is_active());
        assert_eq!(run("hello world period", &cfg), "hello world period");

        let unset = VoiceCommandSettings {
            enabled: true,
            enabled_ids: None,
        };
        let cfg = VoiceCommandConfig::resolve(&unset, Some("en"), None);
        assert_eq!(run("hello world period", &cfg), "hello world.");
    }

    /// The defaults, spelled out. This is the row that fails if a new command
    /// is added to a family without thinking about whether it ships on.
    #[test]
    fn the_shipping_defaults() {
        let cfg = default_cfg();
        for spec in EN_COMMANDS {
            assert_eq!(
                cfg.is_on(spec.id),
                spec.family.default_enabled(),
                "{} defaults wrong",
                spec.id
            );
        }
        assert!(cfg.is_on("period"));
        assert!(cfg.is_on("scratch_that"));
        assert!(!cfg.is_on("new_paragraph"));
        assert!(!cfg.is_on("new_line"));
    }

    /// The ids are a persisted vocabulary and a UI contract: the settings page
    /// renders one row per id and stores the enabled ones. Renaming one
    /// silently turns that command off for everyone who had it on.
    #[test]
    fn the_command_ids_are_stable() {
        let ids: Vec<&str> = EN_COMMANDS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "period",
                "comma",
                "question_mark",
                "exclamation",
                "colon",
                "semicolon",
                "ellipsis",
                "open_quote",
                "close_quote",
                "hyphen",
                "dash",
                "new_line",
                "new_paragraph",
                "scratch_that",
            ]
        );
        let table = table_for(&LangTag::english()).expect("english is populated");
        assert!(table.spec("period").is_some());
        assert!(table.spec("nope").is_none());
    }

    /// A full locale must find the table. Keying on the whole tag would
    /// disable the pass for every "en-US" user without saying anything.
    #[test]
    fn a_locale_resolves_to_its_primary_subtag() {
        assert_eq!(LangTag::parse("en-US").as_str(), "en");
        assert_eq!(LangTag::parse("EN_GB").as_str(), "en");
        assert_eq!(LangTag::parse("de").as_str(), "de");
        assert!(table_for(&LangTag::parse("de")).is_none());
    }

    /// A phrase must not match across a line break: two words either side of a
    /// paragraph gap are not a phrase.
    #[test]
    fn a_phrase_does_not_match_across_a_line_break() {
        let cfg = all_on();
        assert_eq!(run("scratch\nthat", &cfg), "scratch\nthat");
        assert_eq!(run("hello\nperiod", &cfg), "hello\n.");
    }

    /// Longest-first matching, so a two-word spelling is never split.
    #[test]
    fn the_longest_phrase_wins() {
        let cfg = default_cfg();
        assert_eq!(
            run("that is wrong exclamation point", &cfg),
            "that is wrong!"
        );
        assert_eq!(run("wait exclamation mark", &cfg), "wait!");
    }

    /// The clause rule. A determiner cannot reach across punctuation to make
    /// the next phrase a noun, which is what keeps the most important command
    /// working in ordinary prose.
    #[test]
    fn a_leading_word_binds_only_inside_its_clause() {
        let cfg = default_cfg();
        // Adjacent: "a" makes it a noun.
        assert_eq!(run("a scratch that", &cfg), "a scratch that");
        // Across a comma it does not bind, so the retraction fires and takes
        // the sentence with it.
        assert_eq!(run("a, scratch that", &cfg), "");
        assert_eq!(run("a comma", &cfg), "a comma");
    }
}
