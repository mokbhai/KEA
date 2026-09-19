//! Picking the profile that applies to one captured app context.
//!
//! Pure, synchronous and total: no I/O, no clock, no SQL. The caller loads the
//! rows once (`AppProfileRepo::list_enabled`) and asks this. Every tie-break
//! below is decided here rather than in an `ORDER BY`, because a result that
//! depends on the order SQLite happened to return rows in is a result nobody
//! can write a test for.

use super::profile::{AppProfile, ProfileQuery};

/// The profile that applies to `q`, or `None` when no rule matches.
///
/// # Precedence
///
/// 1. `enabled = 0` is never a candidate.
/// 2. A profile is a candidate when **both** hold: `match_bundle_id` is `None`
///    or equals `q.bundle_id` case-insensitively; and `match_url_glob` is
///    `None` or matches `q.url`.
/// 3. **A profile with a `match_url_glob` never matches when `q.url` is
///    `None`.** It does not degrade to a bundle-only match. With URL capture
///    off (the default), a "Slack in the browser" rule is simply inert —
///    silently widening it to all of Chrome is the failure mode where a narrow
///    rule starts applying to the user's bank.
/// 4. Specificity, highest first: `bundle + url` → `bundle` → `url` → catch-all.
/// 5. Within equal specificity: higher `priority` wins.
/// 6. Within equal priority: lowest `id` lexicographically.
pub fn resolve_profile<'a>(
    q: ProfileQuery<'_>,
    profiles: &'a [AppProfile],
) -> Option<&'a AppProfile> {
    profiles
        .iter()
        .filter(|p| p.enabled && matches(p, q))
        // Steps 4–6 as one total ordering. `id` is reversed because the rule
        // is *lowest* id wins, and `max_by` keeps the greatest key.
        .max_by(|a, b| {
            a.specificity()
                .cmp(&b.specificity())
                .then(a.priority.cmp(&b.priority))
                .then(b.id.cmp(&a.id))
        })
}

fn matches(profile: &AppProfile, q: ProfileQuery<'_>) -> bool {
    if let Some(want) = profile.match_bundle_id.as_deref() {
        // Bundle ids are reverse-DNS ASCII in practice, so the ASCII-only
        // fold is both correct and allocation-free on the hotkey path.
        match q.bundle_id {
            Some(got) if got.eq_ignore_ascii_case(want) => {}
            _ => return false,
        }
    }
    if let Some(glob) = profile.match_url_glob.as_deref() {
        // Rule 3: no captured URL means a URL rule does not apply, rather than
        // collapsing to "any URL".
        let Some(url) = q.url else { return false };
        if !url_matches(glob, url) {
            return false;
        }
    }
    true
}

/// Whether `glob` matches `url`, both normalized first.
///
/// Host and path are matched **separately**, so a `*` can never swallow the
/// `/` that ends the host. Without that split, `*.slack.com/*` happily matches
/// `https://evil.com/?x=app.slack.com/` — the `*` eats `evil.com/?x=app`, the
/// literal `.slack.com/` lines up, and a rule the user wrote for Slack starts
/// applying to a page anyone can link them to.
///
/// A pattern with no `/` constrains the host only and leaves the path
/// unconstrained: `slack.com` means all of Slack, and `*` means everything.
/// To pin a path, write one — `slack.com/messages*`.
pub fn url_matches(glob: &str, url: &str) -> bool {
    let (pattern, text) = (normalize_url(glob), normalize_url(url));
    let (pat_host, pat_path) = split_host_path(&pattern);
    let (url_host, url_path) = split_host_path(&text);

    glob_match(pat_host, url_host) && (pat_path.is_empty() || glob_match(pat_path, url_path))
}

/// `host/some/path` → `("host", "/some/path")`; no `/` → an empty path.
fn split_host_path(s: &str) -> (&str, &str) {
    match s.find('/') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// `scheme://user@HOST:port/path` → `host/path`.
///
/// The host is lowercased, the scheme, any userinfo, the port and a leading
/// `www.` are dropped; the path is kept exactly as given (paths are
/// case-sensitive on the wire and a rule that says `/Admin` means `/Admin`).
/// Patterns go through the same function, so `www.slack.com/*` and
/// `https://WWW.Slack.com/messages` meet in the middle.
///
/// IPv6 literals (`[::1]:8080`) are not special-cased: the port strip below
/// only fires when everything after the last `:` is digits, which a bare
/// `[::1]` is not, so the literal survives intact.
fn normalize_url(raw: &str) -> String {
    let rest = match raw.trim().split_once("://") {
        Some((_scheme, rest)) => rest,
        None => raw.trim(),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = match authority.rsplit_once('@') {
        Some((_userinfo, host)) => host,
        None => authority,
    };
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    let host = host.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    format!("{host}{path}")
}

/// `*`-only glob match, iteratively.
///
/// `*` is the whole language on purpose: a regex in a settings field is a
/// support burden and a catastrophic-backtracking hazard, and this runs on the
/// hotkey path. The two-pointer form below is linear — it remembers the last
/// `*` and resumes from there instead of recursing, so no pattern can make it
/// blow up.
///
/// Byte-wise, which is safe for equality over UTF-8: multi-byte sequences
/// never contain an ASCII byte, so `*` can only ever land on a char boundary.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (pat, txt) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);

    while t < txt.len() {
        if p < pat.len() && pat[p] == txt[t] {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(s) = star {
            // Backtrack: let the last `*` swallow one more byte.
            p = s + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_context::profile::Specificity;

    fn profile(id: &str, bundle: Option<&str>, url: Option<&str>) -> AppProfile {
        let mut p = AppProfile::new(id, id);
        p.match_bundle_id = bundle.map(str::to_string);
        p.match_url_glob = url.map(str::to_string);
        p
    }

    // ---- glob / normalization -------------------------------------------

    #[test]
    fn url_normalization_strips_scheme_port_userinfo_and_www() {
        assert_eq!(
            normalize_url("https://WWW.Slack.com:443/x/Y"),
            "slack.com/x/Y"
        );
        assert_eq!(normalize_url("http://user@Example.COM"), "example.com");
        assert_eq!(normalize_url("slack.com"), "slack.com");
        // Path case survives; only the host is folded.
        assert_eq!(
            normalize_url("https://Example.com/Admin"),
            "example.com/Admin"
        );
        // A bare IPv6 literal is not mistaken for a port.
        assert_eq!(normalize_url("http://[::1]/x"), "[::1]/x");
    }

    #[test]
    fn glob_is_star_only_and_anchored_at_both_ends() {
        assert!(url_matches(
            "*.slack.com/*",
            "https://app.slack.com/client/T1"
        ));
        assert!(url_matches("slack.com/*", "https://www.slack.com/messages"));
        // A host-only pattern leaves the path unconstrained.
        assert!(url_matches("slack.com", "https://slack.com/messages"));
        assert!(url_matches("slack.com", "https://www.slack.com"));
        assert!(url_matches("*", "https://anything/at/all"));
        // Anchored at both ends within each half: no accidental substrings.
        assert!(!url_matches("slack.com", "https://notslack.com/"));
        assert!(!url_matches("slack.com", "https://slack.com.evil.test/"));
        // "*.slack.com" requires a real subdomain label.
        assert!(!url_matches("*.slack.com", "https://slack.com/"));
        assert!(url_matches(
            "slack.com/messages*",
            "https://slack.com/messages/42"
        ));
        assert!(!url_matches(
            "slack.com/messages*",
            "https://slack.com/admin"
        ));
    }

    /// The bug the host/path split exists to prevent: `*` must not eat the
    /// slash that ends the host, or a rule written for Slack applies to any
    /// page that mentions Slack in its query string.
    #[test]
    fn a_host_glob_cannot_be_smuggled_through_a_path() {
        assert!(!url_matches(
            "*.slack.com/*",
            "https://evil.com/?x=app.slack.com/"
        ));
        assert!(!url_matches(
            "*.slack.com",
            "https://evil.test/app.slack.com"
        ));
    }

    #[test]
    fn pathological_glob_terminates() {
        // The classic backtracking bomb. Linear here; this test is a canary
        // for anyone tempted to reach for a regex.
        let pattern = "a*a*a*a*a*a*a*a*b";
        let text = "a".repeat(64);
        assert!(!url_matches(pattern, &text));
    }

    // ---- precedence table ------------------------------------------------

    /// The whole precedence order, one row per rule, against one fixed rule
    /// set. Table-driven on purpose: the interesting failures are the ones
    /// where adding a rule changes an unrelated answer.
    #[test]
    fn precedence_table() {
        let mut catch_all = profile("z-catch-all", None, None);
        catch_all.priority = 100; // priority must not beat specificity

        let mut disabled = profile("a-disabled", Some("com.tinyspeck.slackmacgap"), None);
        disabled.enabled = false;
        disabled.priority = 100;

        let profiles = vec![
            catch_all,
            disabled,
            profile("b-chrome", Some("com.google.Chrome"), None),
            profile(
                "c-chrome-slack",
                Some("com.google.Chrome"),
                Some("*.slack.com/*"),
            ),
            profile("d-any-slack-url", None, Some("*.slack.com/*")),
            profile("e-terminal", Some("com.apple.Terminal"), None),
        ];

        let cases: [(&str, ProfileQuery, Option<&str>); 8] = [
            (
                "bundle+url beats bundle alone",
                ProfileQuery::bundle("com.google.Chrome").with_url("https://app.slack.com/client"),
                Some("c-chrome-slack"),
            ),
            (
                "bundle alone beats the catch-all",
                ProfileQuery::bundle("com.google.Chrome"),
                Some("b-chrome"),
            ),
            (
                "a url rule with no bundle still beats the catch-all",
                ProfileQuery::bundle("com.apple.Safari").with_url("https://app.slack.com/client"),
                Some("d-any-slack-url"),
            ),
            (
                "an unknown app falls through to the catch-all",
                ProfileQuery::bundle("com.unknown.App"),
                Some("z-catch-all"),
            ),
            (
                "a disabled profile is skipped, even at high priority",
                ProfileQuery::bundle("com.tinyspeck.slackmacgap"),
                Some("z-catch-all"),
            ),
            (
                "bundle match is case-insensitive",
                ProfileQuery::bundle("COM.APPLE.terminal"),
                Some("e-terminal"),
            ),
            (
                "a url rule is inert when no url was captured",
                ProfileQuery::bundle("com.apple.Safari"),
                Some("z-catch-all"),
            ),
            (
                "no context at all still resolves the catch-all",
                ProfileQuery::default(),
                Some("z-catch-all"),
            ),
        ];

        for (name, q, want) in cases {
            let got = resolve_profile(q, &profiles).map(|p| p.id.as_str());
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn empty_rule_set_resolves_to_nothing() {
        assert!(resolve_profile(ProfileQuery::bundle("com.google.Chrome"), &[]).is_none());
    }

    #[test]
    fn higher_priority_wins_within_equal_specificity() {
        let mut low = profile("a-low", Some("com.google.Chrome"), None);
        low.priority = 1;
        let mut high = profile("b-high", Some("com.google.Chrome"), None);
        high.priority = 5;
        let profiles = vec![low, high];

        let got = resolve_profile(ProfileQuery::bundle("com.google.Chrome"), &profiles);
        assert_eq!(got.map(|p| p.id.as_str()), Some("b-high"));
    }

    #[test]
    fn ties_resolve_deterministically_by_lowest_id() {
        // Same specificity, same priority: the answer must not depend on the
        // order the rows arrived in, so both orderings are asserted.
        let forward = vec![
            profile("aaa", Some("com.google.Chrome"), None),
            profile("bbb", Some("com.google.Chrome"), None),
        ];
        let reversed: Vec<_> = forward.iter().rev().cloned().collect();

        for set in [&forward, &reversed] {
            let got = resolve_profile(ProfileQuery::bundle("com.google.Chrome"), set);
            assert_eq!(got.map(|p| p.id.as_str()), Some("aaa"));
        }
    }

    #[test]
    fn a_url_rule_does_not_widen_to_the_whole_browser() {
        // The failure this rule exists to prevent: a "Slack in the browser"
        // profile must not start applying to the user's bank just because URL
        // capture is off or the page is something else.
        let profiles = vec![profile(
            "slack-web",
            Some("com.google.Chrome"),
            Some("*.slack.com/*"),
        )];

        assert!(resolve_profile(ProfileQuery::bundle("com.google.Chrome"), &profiles).is_none());
        assert!(resolve_profile(
            ProfileQuery::bundle("com.google.Chrome").with_url("https://bank.example/transfer"),
            &profiles
        )
        .is_none());
    }

    #[test]
    fn specificity_of_the_winner_matches_its_keys() {
        let profiles = vec![profile("p", Some("com.google.Chrome"), Some("*"))];
        let got = resolve_profile(
            ProfileQuery::bundle("com.google.Chrome").with_url("https://x.test/"),
            &profiles,
        )
        .unwrap();
        assert_eq!(got.specificity(), Specificity::BundleAndUrl);
    }
}
