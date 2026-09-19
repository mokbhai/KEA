//! The one action vocabulary both scriptable entry points parse into.
//!
//! `kea://` URLs and `POST /v1/...` bodies are two spellings of the same small
//! set of verbs. They parse into [`KeaAction`] here and are carried out in
//! [`super::exec`], so a verb cannot mean one thing over the socket and
//! another through LaunchServices — and so the gate that keeps two recorders
//! out of the audio layer is written once.

use std::path::{Component, Path, PathBuf};

use kea_core::rewrite::RewriteMode;
use url::Url;

/// The URL scheme KEA registers. Also the `CFBundleURLSchemes` entry in
/// `Info.plist`; the two must not drift.
pub const SCHEME: &str = "kea";

/// The `kea://open/<page>` targets, mirroring the `Page` union in
/// `ui/src/lib/nav.ts`.
///
/// A table rather than a match: `open` has no behaviour per page — it shows
/// one window and hands the name to the frontend — so the only thing worth
/// encoding is which names exist.
pub const SETTINGS_PAGES: [&str; 12] = [
    "rewrite",
    "dictation",
    "meetings",
    "transcribe",
    "read-aloud",
    "ai-providers",
    "models",
    "vocabulary",
    "profiles",
    "general",
    "history",
    "logs",
];

/// Which way a dictation verb points.
///
/// `Toggle` is what the hotkey does; `Start`/`Stop` exist because a script
/// knows what it wants and a toggle that guessed wrong is a lost recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationVerb {
    Start,
    Stop,
    Toggle,
}

impl DictationVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            DictationVerb::Start => "start",
            DictationVerb::Stop => "stop",
            DictationVerb::Toggle => "toggle",
        }
    }

    /// `None` for anything else, deliberately — see [`parse_kea_url`] on why
    /// an unknown word must not fall back to a plausible default.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(DictationVerb::Start),
            "stop" => Some(DictationVerb::Stop),
            "toggle" => Some(DictationVerb::Toggle),
            _ => None,
        }
    }
}

/// A rewrite asked for from outside the app.
///
/// `text` absent means "take whatever the user has selected in the frontmost
/// app", which is the hotkey's behaviour and the only one that needs the
/// selection guard. `insert` is what decides whether the answer is written
/// back over that selection or only returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewriteRequest {
    pub text: Option<String>,
    pub mode: Option<RewriteMode>,
    pub preset_id: Option<String>,
    pub instruction: Option<String>,
    pub insert: bool,
}

impl RewriteRequest {
    /// The one combination that cannot mean anything: text supplied by the
    /// caller has no selection to be inserted over, and pasting it into
    /// whatever happens to be focused is not what "insert" asked for.
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.text.is_some() && self.insert {
            return Err(ParseError::Contradiction(
                "`insert` rewrites the user's selection, so it cannot be combined with `text`",
            ));
        }
        Ok(())
    }

    /// Whether carrying this out fires a synthetic ⌘C or ⌘V at the frontmost
    /// app — which is what decides whether it has to take `selection_busy`.
    pub fn touches_the_selection(&self) -> bool {
        self.text.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeaAction {
    Rewrite(RewriteRequest),
    Dictation(DictationVerb),
    ReadAloud,
    Transcribe { path: PathBuf },
    Open { page: String },
    Status,
}

impl KeaAction {
    /// The verb as it appears in a log line. Every API call leaves one; an
    /// API whose calls leave no trace is one nobody can audit afterwards.
    pub fn label(&self) -> &'static str {
        match self {
            KeaAction::Rewrite(_) => "rewrite",
            KeaAction::Dictation(_) => "dictation",
            KeaAction::ReadAloud => "read-aloud",
            KeaAction::Transcribe { .. } => "transcribe",
            KeaAction::Open { .. } => "open",
            KeaAction::Status => "status",
        }
    }

    /// Whether this action needs the user's own app to be frontmost, which is
    /// exactly the set that activating KEA would break.
    pub fn needs_the_users_app(&self) -> bool {
        match self {
            KeaAction::Rewrite(req) => req.touches_the_selection(),
            KeaAction::ReadAloud => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Malformed,
    Scheme(String),
    UnknownVerb(String),
    MissingSubject(&'static str),
    UnexpectedSubject(&'static str),
    UnknownSubject { verb: &'static str, got: String },
    UnknownMode(String),
    MissingParam(&'static str),
    EmptyParam(&'static str),
    Contradiction(&'static str),
    NotOverTheUrlScheme(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Malformed => write!(f, "not a URL"),
            ParseError::Scheme(s) => write!(f, "expected a {SCHEME}:// URL, got '{s}://'"),
            ParseError::UnknownVerb(v) => write!(f, "unknown verb '{v}'"),
            ParseError::MissingSubject(verb) => {
                write!(f, "'{verb}' needs a subject, e.g. {SCHEME}://{verb}/…")
            }
            ParseError::UnexpectedSubject(verb) => {
                write!(f, "'{verb}' takes no path, only query parameters")
            }
            ParseError::UnknownSubject { verb, got } => {
                write!(f, "'{verb}' has no subject '{got}'")
            }
            // Named modes rather than "invalid": a Raycast script that
            // silently rewrote in the wrong voice is worse than one that
            // stopped, and the fix is usually a typo away.
            ParseError::UnknownMode(m) => write!(
                f,
                "unknown rewrite mode '{m}'; expected one of {}",
                RewriteMode::ALL
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ParseError::MissingParam(p) => write!(f, "missing required parameter '{p}'"),
            ParseError::EmptyParam(p) => write!(f, "parameter '{p}' is empty"),
            ParseError::Contradiction(why) => write!(f, "{why}"),
            ParseError::NotOverTheUrlScheme(why) => write!(f, "{why}"),
        }
    }
}

/// One query parameter, percent- and plus-decoded.
fn param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// A required, non-empty query parameter.
fn required(url: &Url, key: &'static str) -> Result<String, ParseError> {
    match param(url, key) {
        None => Err(ParseError::MissingParam(key)),
        Some(v) if v.is_empty() => Err(ParseError::EmptyParam(key)),
        Some(v) => Ok(v),
    }
}

/// `Some` only for a present, non-empty value: `?preset=` is a script that
/// interpolated an unset shell variable, not a request for the empty preset.
fn optional(url: &Url, key: &str) -> Option<String> {
    param(url, key).filter(|v| !v.is_empty())
}

/// The mode named by `?mode=`, if any.
///
/// [`RewriteMode::from_str`] is the parser — it already has the explicit
/// `_ => None` arm that rejects an unknown value — so a `None` from it becomes
/// an error here rather than being swallowed by an `unwrap_or(Improve)`.
fn mode_param(url: &Url) -> Result<Option<RewriteMode>, ParseError> {
    match optional(url, "mode") {
        None => Ok(None),
        Some(raw) => RewriteMode::from_str(&raw)
            .map(Some)
            .ok_or(ParseError::UnknownMode(raw)),
    }
}

/// Parse one `kea://` URL into the action it names.
///
/// Rejecting is the point of most of this function. The caller is a script the
/// user wrote once and will never read the output of again, so a URL that is
/// almost right has to fail loudly at the door instead of doing something
/// almost right to their text.
pub fn parse_kea_url(raw: &str) -> Result<KeaAction, ParseError> {
    let url = Url::parse(raw.trim()).map_err(|_| ParseError::Malformed)?;
    if url.scheme() != SCHEME {
        return Err(ParseError::Scheme(url.scheme().to_string()));
    }

    // The `url` crate does not case-fold an opaque host (a non-special scheme
    // has no registrable domain to normalise), so `kea://Rewrite` would
    // otherwise be an unknown verb.
    let verb = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let subject: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();

    match verb.as_str() {
        "rewrite" => {
            if !subject.is_empty() {
                return Err(ParseError::UnexpectedSubject("rewrite"));
            }
            let text = match param(&url, "text") {
                // Present-but-empty is the shell-variable mistake again, and
                // here it is worth catching: an empty `text` would otherwise
                // silently become "rewrite my selection".
                Some(t) if t.is_empty() => return Err(ParseError::EmptyParam("text")),
                other => other,
            };
            let request = RewriteRequest {
                insert: text.is_none(),
                text,
                mode: mode_param(&url)?,
                preset_id: optional(&url, "preset"),
                instruction: optional(&url, "instruction"),
            };
            request.validate()?;
            Ok(KeaAction::Rewrite(request))
        }
        "dictation" => {
            let Some(first) = subject.first() else {
                return Err(ParseError::MissingSubject("dictation"));
            };
            DictationVerb::parse(&first.to_ascii_lowercase())
                .map(KeaAction::Dictation)
                .ok_or_else(|| ParseError::UnknownSubject {
                    verb: "dictation",
                    got: (*first).to_string(),
                })
        }
        "read-aloud" => {
            if !subject.is_empty() {
                return Err(ParseError::UnexpectedSubject("read-aloud"));
            }
            Ok(KeaAction::ReadAloud)
        }
        "transcribe" => {
            if !subject.is_empty() {
                return Err(ParseError::UnexpectedSubject("transcribe"));
            }
            Ok(KeaAction::Transcribe {
                path: PathBuf::from(required(&url, "path")?),
            })
        }
        "open" => {
            let Some(page) = subject.first() else {
                return Err(ParseError::MissingSubject("open"));
            };
            let page = page.to_ascii_lowercase();
            if !SETTINGS_PAGES.contains(&page.as_str()) {
                return Err(ParseError::UnknownSubject {
                    verb: "open",
                    got: page,
                });
            }
            Ok(KeaAction::Open { page })
        }
        // Reachable only as a mistake: a URL is fire-and-forget, so there is
        // nowhere for a status body to go. Saying where it *does* live is more
        // use than "unknown verb".
        "status" => Err(ParseError::NotOverTheUrlScheme(
            "status has a response body, so it is only on the local API socket (GET /v1/status)",
        )),
        other => Err(ParseError::UnknownVerb(other.to_string())),
    }
}

/// Whether a `transcribe` path is one KEA will open.
///
/// Absolute, no `..`, and under the user's home. Not a security boundary —
/// anything holding the token already runs as the user and can read the same
/// files — but it stops a script from handing KEA a system path by accident,
/// and it keeps the answer to "what did the API touch" inside one directory
/// tree. Pure, so the rule is testable without a real home.
pub fn transcribe_path_allowed(path: &Path, home: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|c| c == Component::ParentDir)
        && path.starts_with(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(raw: &str) -> RewriteRequest {
        match parse_kea_url(raw) {
            Ok(KeaAction::Rewrite(r)) => r,
            other => panic!("expected a rewrite from {raw}, got {other:?}"),
        }
    }

    #[test]
    fn every_mode_string_round_trips_through_the_url() {
        for mode in RewriteMode::ALL {
            let url = format!("kea://rewrite?mode={}", mode.as_str());
            assert_eq!(rewrite(&url).mode, Some(mode), "{url}");
        }
    }

    #[test]
    fn an_unknown_mode_is_an_error_not_a_default() {
        // `Improve` is the plausible-looking wrong answer, so assert it is
        // not what a typo produces.
        let err = parse_kea_url("kea://rewrite?mode=Improve").unwrap_err();
        assert_eq!(err, ParseError::UnknownMode("Improve".into()));
        assert!(err.to_string().contains("improve"), "{err}");
    }

    #[test]
    fn a_selection_rewrite_inserts_and_a_text_rewrite_does_not() {
        let selection = rewrite("kea://rewrite?mode=concise");
        assert!(selection.insert);
        assert!(selection.touches_the_selection());

        let supplied = rewrite("kea://rewrite?text=hello");
        assert!(!supplied.insert);
        assert!(!supplied.touches_the_selection());
        assert_eq!(supplied.text.as_deref(), Some("hello"));
    }

    #[test]
    fn instruction_text_survives_percent_and_plus_encoding() {
        let req = rewrite(
            "kea://rewrite?mode=ask_kea&instruction=R%26D%20plan%2Bnotes%20for%20Espa%C3%B1a",
        );
        assert_eq!(req.mode, Some(RewriteMode::AskKea));
        // `+` inside a query is a space under form decoding; the caller that
        // wanted a literal plus percent-encoded it, and got one.
        assert_eq!(
            req.instruction.as_deref(),
            Some("R&D plan+notes for España")
        );
    }

    #[test]
    fn a_preset_can_be_asked_for_by_id() {
        assert_eq!(
            rewrite("kea://rewrite?preset=standup").preset_id.as_deref(),
            Some("standup")
        );
    }

    #[test]
    fn an_empty_text_parameter_is_refused() {
        assert_eq!(
            parse_kea_url("kea://rewrite?text=").unwrap_err(),
            ParseError::EmptyParam("text")
        );
    }

    #[test]
    fn an_empty_optional_parameter_reads_as_absent() {
        let req = rewrite("kea://rewrite?preset=&instruction=");
        assert_eq!(req.preset_id, None);
        assert_eq!(req.instruction, None);
    }

    #[test]
    fn dictation_takes_its_three_directions_and_nothing_else() {
        for (raw, want) in [
            ("kea://dictation/start", DictationVerb::Start),
            ("kea://dictation/stop", DictationVerb::Stop),
            ("kea://dictation/toggle", DictationVerb::Toggle),
            // A trailing slash is what a shell loop that joined paths
            // produces; it is not a different verb.
            ("kea://dictation/toggle/", DictationVerb::Toggle),
        ] {
            assert_eq!(parse_kea_url(raw), Ok(KeaAction::Dictation(want)), "{raw}");
        }
        assert_eq!(
            parse_kea_url("kea://dictation/pause").unwrap_err(),
            ParseError::UnknownSubject {
                verb: "dictation",
                got: "pause".into()
            }
        );
        assert_eq!(
            parse_kea_url("kea://dictation").unwrap_err(),
            ParseError::MissingSubject("dictation")
        );
    }

    #[test]
    fn the_verb_is_case_insensitive() {
        assert_eq!(
            parse_kea_url("kea://Dictation/Start"),
            Ok(KeaAction::Dictation(DictationVerb::Start))
        );
    }

    #[test]
    fn read_aloud_and_open_and_transcribe_parse() {
        assert_eq!(parse_kea_url("kea://read-aloud"), Ok(KeaAction::ReadAloud));
        assert_eq!(
            parse_kea_url("kea://open/general"),
            Ok(KeaAction::Open {
                page: "general".into()
            })
        );
        assert_eq!(
            parse_kea_url("kea://transcribe?path=/Users/x/a%20b.m4a"),
            Ok(KeaAction::Transcribe {
                path: PathBuf::from("/Users/x/a b.m4a")
            })
        );
    }

    #[test]
    fn every_settings_page_opens() {
        for page in SETTINGS_PAGES {
            assert_eq!(
                parse_kea_url(&format!("kea://open/{page}")),
                Ok(KeaAction::Open { page: page.into() })
            );
        }
        assert!(matches!(
            parse_kea_url("kea://open/nope"),
            Err(ParseError::UnknownSubject { verb: "open", .. })
        ));
    }

    #[test]
    fn transcribe_needs_a_path() {
        assert_eq!(
            parse_kea_url("kea://transcribe").unwrap_err(),
            ParseError::MissingParam("path")
        );
    }

    #[test]
    fn a_missing_or_unknown_verb_is_refused() {
        assert_eq!(
            parse_kea_url("kea://").unwrap_err(),
            ParseError::UnknownVerb(String::new())
        );
        assert_eq!(
            parse_kea_url("kea://frobnicate").unwrap_err(),
            ParseError::UnknownVerb("frobnicate".into())
        );
    }

    #[test]
    fn another_apps_scheme_is_refused() {
        assert_eq!(
            parse_kea_url("https://example.com/rewrite").unwrap_err(),
            ParseError::Scheme("https".into())
        );
        assert_eq!(
            parse_kea_url("not a url").unwrap_err(),
            ParseError::Malformed
        );
    }

    #[test]
    fn status_points_at_the_socket_rather_than_failing_as_unknown() {
        assert!(matches!(
            parse_kea_url("kea://status"),
            Err(ParseError::NotOverTheUrlScheme(_))
        ));
    }

    #[test]
    fn text_and_insert_together_are_a_contradiction() {
        let req = RewriteRequest {
            text: Some("hi".into()),
            insert: true,
            ..RewriteRequest::default()
        };
        assert!(matches!(req.validate(), Err(ParseError::Contradiction(_))));
    }

    #[test]
    fn transcribe_paths_outside_the_home_are_refused() {
        let home = Path::new("/Users/kea");
        assert!(transcribe_path_allowed(
            Path::new("/Users/kea/Recordings/a.m4a"),
            home
        ));
        assert!(!transcribe_path_allowed(Path::new("/etc/passwd"), home));
        assert!(!transcribe_path_allowed(
            Path::new("Recordings/a.m4a"),
            home
        ));
        assert!(!transcribe_path_allowed(
            Path::new("/Users/kea/../root/a.m4a"),
            home
        ));
        // A sibling whose name merely starts with the home directory's is not
        // inside it; `starts_with` compares components, not bytes.
        assert!(!transcribe_path_allowed(
            Path::new("/Users/kea-backup/a.m4a"),
            home
        ));
    }
}
