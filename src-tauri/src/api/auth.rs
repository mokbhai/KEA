//! Who may call the local API.
//!
//! # Why a local HTTP server is a real attack surface
//!
//! "It is only listening on this machine" is the reasoning behind a long line
//! of desktop-app CVEs, and it is wrong for two reasons. First, *every* process
//! on the machine is a peer: a local server has no per-process identity to
//! check, so any script, any Electron app, any `npm postinstall` running as the
//! user is already on the inside. Second, a browser is a local process too —
//! a web page can issue cross-origin requests at `127.0.0.1`, and with DNS
//! rebinding it can make those requests look same-origin.
//!
//! What makes the default transport acceptable is its shape, not the checks in
//! this file. KEA listens on a **unix domain socket** at mode `0600` under the
//! app data directory (see [`super::token::socket_path`]). That gets filesystem
//! access control for free, and it is unreachable from a browser at all: there
//! is no URL for a unix socket, so the entire DNS-rebinding class is removed
//! rather than defended against. `curl --unix-socket` covers the Raycast, Alfred
//! and shell callers.
//!
//! A **loopback TCP port** is an opt-in setting, off by default, bound to
//! `127.0.0.1` only. It exists because an SDK such as the OpenAI client can only
//! be pointed at a URL, and a unix socket is not one. Turning it on deliberately
//! gives back the one property the socket did not have: a web page can send
//! requests to it. The checks below stand in for that property, and they apply
//! on both transports:
//!
//! - a bearer token, as `Authorization: Bearer` or as the `x-kea-token` header.
//!   Neither is a CORS-safelisted header, so a cross-origin page must preflight,
//!   and the preflight fails because no `Access-Control-Allow-*` header is ever
//!   sent. A page can still send a *simple* request without a token; it is
//!   refused with a 401 before any action runs, and the page cannot read the
//!   answer;
//! - any request carrying an `Origin` header is refused outright — nothing
//!   that legitimately calls this API has an origin;
//! - the `Host` header must name loopback, which is the only thing that
//!   distinguishes a rebound `evil.com` request from a real one;
//! - the token comparison is constant-time.
//!
//! The port is off by default so that a user who never reads this file never
//! has a browser-reachable API. The settings-page warning says what the token
//! allows, because switching the port on is the user's call to make with that
//! in view.
//!
//! # What an attacker holding the token can do
//!
//! Stated plainly, because it belongs in the code as much as in the UI: the
//! token is equivalent to **typing arbitrary text into whatever app the user
//! currently has focused**, **reading whatever text they have selected**,
//! **opening the microphone**, and **spending their LLM API credits**.
//! `execute_rewrite` captures the selection and writes the result back over
//! it; `start_dictation_inner` opens the mic.
//!
//! It grants no new *credential* exposure — a local process that can read this
//! token out of the keychain can read the provider API keys from the same
//! keychain — so what the API genuinely adds is text injection into the
//! focused app. The boundary this buys is "another user on this machine, or a
//! web page", not "malware already running as you". Against that attacker the
//! keychain is no better than a file, and we do not pretend otherwise.

use http::header::{HeaderMap, AUTHORIZATION, HOST, ORIGIN};

/// The token header. Custom on purpose: a custom header is not a CORS
/// simple-request header, so a browser must preflight, and the preflight has
/// no `Access-Control-Allow-*` to succeed against.
pub const TOKEN_HEADER: &str = "x-kea-token";

/// The hosts a loopback request may claim. A unix socket has no authority of
/// its own, so clients send whatever was in the URL they were given —
/// `curl --unix-socket … http://localhost/v1/status` sends `localhost`.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// An `Origin` header at all: a browser is calling, and no browser should.
    OriginPresent,
    /// A `Host` that is not loopback — the DNS-rebinding tell.
    BadHost,
    MissingToken,
    BadToken,
}

impl Denied {
    /// All four are 401.
    ///
    /// Not 403 and not a distinct code per reason: telling a caller *which*
    /// check it failed is free reconnaissance, and there is no case where a
    /// legitimate client recovers differently.
    pub fn status(self) -> u16 {
        401
    }

    /// For the log line only. The response body says nothing but
    /// "unauthorized".
    pub fn reason(self) -> &'static str {
        match self {
            Denied::OriginPresent => "request carried an Origin header",
            Denied::BadHost => "Host header is not loopback",
            Denied::MissingToken => "no token",
            Denied::BadToken => "wrong token",
        }
    }
}

/// Compare two secrets without leaking where they diverge.
///
/// The timing oracle over a local socket is largely theoretical; the fix is
/// thirty lines and no dependency, so there is nothing to argue about. The
/// length is not hidden — it is fixed and public (a 32-byte token as hex).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Strip the optional `:port` from a `Host` value.
fn host_without_port(host: &str) -> &str {
    // An IPv6 literal keeps its brackets, so only split after the closing one.
    match host.rfind(']') {
        Some(end) => &host[..=end],
        None => host.split(':').next().unwrap_or(host),
    }
}

/// Decide whether a request head may act.
///
/// A pure function over the headers and the expected token: every case below
/// is a unit test, and none of them needs a live socket.
pub fn authorize(headers: &HeaderMap, expected: &[u8]) -> Result<(), Denied> {
    if headers.contains_key(ORIGIN) {
        return Err(Denied::OriginPresent);
    }

    // Absent is fine: HTTP/1.0 clients over a unix socket have no authority to
    // name, and there is no rebinding attack without a browser to rebind.
    if let Some(host) = headers.get(HOST) {
        let host = host.to_str().map_err(|_| Denied::BadHost)?;
        let bare = host_without_port(host).to_ascii_lowercase();
        if !LOOPBACK_HOSTS.contains(&bare.as_str()) {
            return Err(Denied::BadHost);
        }
    }

    let Some(offered) = offered_token(headers) else {
        return Err(Denied::MissingToken);
    };
    if constant_time_eq(offered, expected) {
        Ok(())
    } else {
        Err(Denied::BadToken)
    }
}

/// The token a request offers: the `x-kea-token` value if there is one, else
/// the credential of an `Authorization: Bearer` header.
///
/// The bearer form is what OpenAI SDKs send, so they work unchanged. Any other
/// scheme is no token at all rather than a wrong one, so it reads as missing.
fn offered_token(headers: &HeaderMap) -> Option<&[u8]> {
    if let Some(custom) = headers.get(TOKEN_HEADER) {
        return Some(custom.as_bytes());
    }
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(credential.trim().as_bytes())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::HeaderValue;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn the_right_token_from_loopback_is_allowed() {
        let h = headers(&[("host", "localhost"), (TOKEN_HEADER, TOKEN)]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Ok(()));
    }

    #[test]
    fn a_port_on_the_host_is_fine() {
        for host in ["localhost:8765", "127.0.0.1:8765", "[::1]:8765"] {
            let h = headers(&[("host", host), (TOKEN_HEADER, TOKEN)]);
            assert_eq!(authorize(&h, TOKEN.as_bytes()), Ok(()), "{host}");
        }
    }

    #[test]
    fn a_bearer_token_is_accepted_as_well_as_the_custom_header() {
        let h = headers(&[
            ("host", "127.0.0.1:8765"),
            ("authorization", &format!("Bearer {TOKEN}")),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Ok(()));
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive() {
        let h = headers(&[
            ("host", "localhost"),
            ("authorization", &format!("bearer {TOKEN}")),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Ok(()));
    }

    #[test]
    fn a_wrong_bearer_token_is_denied() {
        let mut wrong = TOKEN.to_string();
        wrong.pop();
        wrong.push('0');
        let h = headers(&[
            ("host", "localhost"),
            ("authorization", &format!("Bearer {wrong}")),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::BadToken));
    }

    #[test]
    fn another_auth_scheme_carries_no_token() {
        let h = headers(&[
            ("host", "localhost"),
            ("authorization", &format!("Basic {TOKEN}")),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::MissingToken));
    }

    #[test]
    fn an_origin_is_refused_even_with_a_valid_bearer_token() {
        let h = headers(&[
            ("host", "localhost"),
            ("origin", "http://localhost:3000"),
            ("authorization", &format!("Bearer {TOKEN}")),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::OriginPresent));
    }

    #[test]
    fn no_token_is_denied() {
        let h = headers(&[("host", "localhost")]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::MissingToken));
    }

    #[test]
    fn a_wrong_token_of_identical_length_is_denied() {
        let mut wrong = TOKEN.to_string();
        wrong.pop();
        wrong.push('0');
        assert_eq!(wrong.len(), TOKEN.len());
        let h = headers(&[("host", "localhost"), (TOKEN_HEADER, &wrong)]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::BadToken));
    }

    #[test]
    fn a_prefix_of_the_token_is_denied() {
        let h = headers(&[("host", "localhost"), (TOKEN_HEADER, &TOKEN[..8])]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::BadToken));
    }

    #[test]
    fn an_origin_header_is_denied_before_the_token_is_even_read() {
        // Denied even *with* the right token: a browser has no business here,
        // and a page that somehow obtained the token is the case this catches.
        let h = headers(&[
            ("host", "localhost"),
            ("origin", "https://evil.example"),
            (TOKEN_HEADER, TOKEN),
        ]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::OriginPresent));
    }

    #[test]
    fn a_rebound_host_is_denied() {
        let h = headers(&[("host", "evil.com"), (TOKEN_HEADER, TOKEN)]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Err(Denied::BadHost));
    }

    #[test]
    fn an_absent_host_is_allowed() {
        let h = headers(&[(TOKEN_HEADER, TOKEN)]);
        assert_eq!(authorize(&h, TOKEN.as_bytes()), Ok(()));
    }

    #[test]
    fn every_denial_is_the_same_status_and_says_nothing_extra() {
        for d in [
            Denied::OriginPresent,
            Denied::BadHost,
            Denied::MissingToken,
            Denied::BadToken,
        ] {
            assert_eq!(d.status(), 401);
        }
    }

    #[test]
    fn constant_time_eq_still_answers_correctly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
