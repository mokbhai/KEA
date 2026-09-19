//! The stored per-app profile row and the query it is matched against.

use serde::{Deserialize, Serialize};

use crate::rewrite::RewriteMode;
use crate::store::bindings::Binding;

/// Where a profile says the rewritten/dictated text should be put back.
///
/// Mirrors `kea_platform::ReplaceMode` without depending on it — core cannot
/// see the platform crate. A descriptor rather than a bare string so that
/// every consumer branches on the enum and the column's two legal values are
/// written down exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionMode {
    /// macOS Accessibility insertion (`ReplaceMode::Accessibility`).
    Accessibility,
    /// Clipboard save → set → synthetic ⌘V → restore (`ReplaceMode::ClipboardPaste`).
    ClipboardPaste,
}

impl InsertionMode {
    pub const ALL: [InsertionMode; 2] =
        [InsertionMode::Accessibility, InsertionMode::ClipboardPaste];

    /// The value stored in `app_profiles.insertion_mode`.
    pub fn as_str(self) -> &'static str {
        match self {
            InsertionMode::Accessibility => "ax",
            InsertionMode::ClipboardPaste => "paste",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`. Same shape
    // as `RewriteMode::from_str` next door.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ax" => Some(InsertionMode::Accessibility),
            "paste" => Some(InsertionMode::ClipboardPaste),
            _ => None,
        }
    }
}

/// One row of `app_profiles`.
///
/// Every override column is `Option`, and `None` means **inherit the global
/// setting** — not "off". `post_process` in particular is a tri-state:
/// `Some(true)` forces the dictation cleanup pass on, `Some(false)` forces it
/// off (the point of the whole feature: no LLM pass into a shell prompt), and
/// `None` leaves the global flag alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AppProfile {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i64,

    /// Exact bundle id, compared case-insensitively. `None` = matches any app.
    pub match_bundle_id: Option<String>,
    /// `*`-only glob over the normalized `host[/path]`. `None` = matches any URL.
    pub match_url_glob: Option<String>,

    /// [`RewriteMode::as_str`]; kept as the raw column so a mode this build
    /// does not know about round-trips instead of being silently dropped on
    /// the next save. Read it through [`AppProfile::mode`].
    pub rewrite_mode: Option<String>,
    pub preset_id: Option<String>,
    pub llm_engine_id: Option<String>,
    pub llm_model: Option<String>,
    pub llm_provider_ref: Option<String>,
    pub post_process: Option<bool>,
    /// [`InsertionMode::as_str`]; read it through [`AppProfile::insertion`].
    pub insertion_mode: Option<String>,

    pub created_at: String,
}

impl AppProfile {
    /// A blank profile with the given id/name: every override inherits and
    /// every match key is "any". Callers set only the fields they mean.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            enabled: true,
            priority: 0,
            match_bundle_id: None,
            match_url_glob: None,
            rewrite_mode: None,
            preset_id: None,
            llm_engine_id: None,
            llm_model: None,
            llm_provider_ref: None,
            post_process: None,
            insertion_mode: None,
            created_at: String::new(),
        }
    }

    /// The rewrite mode this profile forces, or `None` to inherit.
    ///
    /// An unparseable stored value reads as `None` (inherit) rather than as an
    /// error: a profile written by a newer build naming a mode this one has
    /// never heard of should fall back to the global setting, not break every
    /// rewrite into that app.
    pub fn mode(&self) -> Option<RewriteMode> {
        RewriteMode::from_str(self.rewrite_mode.as_deref()?)
    }

    /// The insertion mode this profile forces, or `None` to inherit.
    pub fn insertion(&self) -> Option<InsertionMode> {
        InsertionMode::from_str(self.insertion_mode.as_deref()?)
    }

    /// The LLM this profile forces, in the exact shape `SlotResolver::require_llm`
    /// returns — so a caller substitutes it for that call rather than
    /// re-deriving a second engine-selection path.
    ///
    /// `None` unless an engine id is set: a model or a provider ref with no
    /// engine names nothing resolvable, so it inherits rather than half-applying.
    pub fn llm_binding(&self) -> Option<Binding> {
        Some(Binding {
            engine_id: self.llm_engine_id.clone()?,
            model: self.llm_model.clone(),
            provider_ref: self.llm_provider_ref.clone(),
        })
    }

    /// How specific this profile's match keys are; see [`Specificity`].
    pub fn specificity(&self) -> Specificity {
        match (
            self.match_bundle_id.is_some(),
            self.match_url_glob.is_some(),
        ) {
            (true, true) => Specificity::BundleAndUrl,
            (true, false) => Specificity::BundleOnly,
            (false, true) => Specificity::UrlOnly,
            (false, false) => Specificity::CatchAll,
        }
    }
}

/// How narrow a profile's match keys are. Ordered: a later variant wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Specificity {
    /// Both match keys NULL — applies to everything.
    CatchAll,
    /// URL only: "any app, when the page is *.slack.com".
    UrlOnly,
    /// Bundle id only: "Slack, wherever in it".
    BundleOnly,
    /// Bundle id and URL together — the narrowest rule wins.
    BundleAndUrl,
}

/// The matchable subset of a captured app context.
///
/// Deliberately narrower than the platform's `AppContext`: `app_name` and
/// `window_title` are display strings — localized, volatile, different on
/// every machine — so they are not match keys and cannot be typed into a rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProfileQuery<'a> {
    pub bundle_id: Option<&'a str>,
    /// Absent whenever URL capture is off, which is the default. A rule with a
    /// URL pattern is then simply inert — see [`super::resolve_profile`].
    pub url: Option<&'a str>,
}

impl<'a> ProfileQuery<'a> {
    pub fn bundle(bundle_id: &'a str) -> Self {
        Self {
            bundle_id: Some(bundle_id),
            url: None,
        }
    }

    pub fn with_url(mut self, url: &'a str) -> Self {
        self.url = Some(url);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_mode_round_trips_through_its_column_value() {
        // Same guard as RewriteMode's: a half-added variant would make the
        // stored column stop parsing and silently read as "inherit".
        for mode in InsertionMode::ALL {
            assert_eq!(InsertionMode::from_str(mode.as_str()), Some(mode));
        }
        assert_eq!(InsertionMode::from_str("accessibility"), None);
    }

    #[test]
    fn unknown_stored_mode_inherits_rather_than_erroring() {
        let mut p = AppProfile::new("p", "P");
        p.rewrite_mode = Some("mode_from_the_future".into());
        p.insertion_mode = Some("carrier_pigeon".into());
        assert_eq!(p.mode(), None);
        assert_eq!(p.insertion(), None);
    }

    #[test]
    fn typed_accessors_read_the_stored_columns() {
        let mut p = AppProfile::new("p", "P");
        p.rewrite_mode = Some(RewriteMode::Friendly.as_str().into());
        p.insertion_mode = Some(InsertionMode::ClipboardPaste.as_str().into());
        assert_eq!(p.mode(), Some(RewriteMode::Friendly));
        assert_eq!(p.insertion(), Some(InsertionMode::ClipboardPaste));
    }

    #[test]
    fn llm_binding_needs_an_engine_id() {
        let mut p = AppProfile::new("p", "P");
        p.llm_model = Some("gpt-4o".into());
        // A model with no engine names nothing resolvable: inherit instead of
        // half-applying an override.
        assert_eq!(p.llm_binding(), None);

        p.llm_engine_id = Some("openai".into());
        p.llm_provider_ref = Some("work".into());
        let b = p.llm_binding().unwrap();
        assert_eq!(b.engine_id, "openai");
        assert_eq!(b.model.as_deref(), Some("gpt-4o"));
        assert_eq!(b.provider_ref.as_deref(), Some("work"));
    }

    #[test]
    fn specificity_orders_narrowest_last() {
        assert!(Specificity::BundleAndUrl > Specificity::BundleOnly);
        assert!(Specificity::BundleOnly > Specificity::UrlOnly);
        assert!(Specificity::UrlOnly > Specificity::CatchAll);
    }

    #[test]
    fn specificity_reads_the_match_keys() {
        let mut p = AppProfile::new("p", "P");
        assert_eq!(p.specificity(), Specificity::CatchAll);
        p.match_url_glob = Some("*.slack.com/*".into());
        assert_eq!(p.specificity(), Specificity::UrlOnly);
        p.match_bundle_id = Some("com.google.Chrome".into());
        assert_eq!(p.specificity(), Specificity::BundleAndUrl);
        p.match_url_glob = None;
        assert_eq!(p.specificity(), Specificity::BundleOnly);
    }
}
