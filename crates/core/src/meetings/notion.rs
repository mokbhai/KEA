//! Notion export: what to send, and what a refusal means.
//!
//! Pure, like [`super::export`]: block conversion, request bodies, page-link
//! parsing and error classification. The HTTP call and the credential read
//! belong to the app layer, which is also where the decision about *when* to
//! export lives.
//!
//! ## Why an internal integration token, and not OAuth
//!
//! A public OAuth integration needs three things KEA does not have: a client
//! secret that cannot ship inside a desktop binary, a hosted redirect URL, and
//! a callback listener to catch the code. An **internal integration token** is
//! a single secret the user pastes once, needs no callback, and is Notion's
//! own answer for a tool talking to one person's workspace.
//!
//! It is not free, and the UI has to say so: the user must create an
//! integration at notion.so/my-integrations *and* share the destination page
//! with it. Miss the second step and Notion answers `404 object_not_found` —
//! indistinguishable, from the status line alone, from a page that does not
//! exist. That is precisely why [`classify`] exists rather than the caller
//! reading a status code.

use serde_json::{json, Value};

/// The API version header every request must carry. Notion pins behaviour to
/// this date; omitting it is an error, and bumping it is a deliberate act.
pub const NOTION_API_VERSION: &str = "2022-06-28";

pub const NOTION_PAGES_URL: &str = "https://api.notion.com/v1/pages";

/// Where the rest of a long document goes. Notion caps a request at 100 child
/// blocks, so an export longer than that is a create followed by appends.
pub fn block_children_url(page_id: &str) -> String {
    format!("https://api.notion.com/v1/blocks/{page_id}/children")
}

/// The most children Notion accepts in one request.
pub const MAX_CHILDREN_PER_REQUEST: usize = 100;

/// The most characters one rich-text run may carry.
pub const MAX_RICH_TEXT_CHARS: usize = 2000;

/// Why an export did not happen.
///
/// Four failures with four different fixes, kept apart on purpose: a wrong
/// token is retyped, an unshared page is shared from Notion's own ⋯ menu, a
/// rejected block is a bug here, and a 503 is waited out. Collapsing them into
/// one "Notion error" string is what leaves a user re-pasting a token that was
/// never the problem.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotionError {
    #[error("no Notion token saved — create an internal integration at notion.so/my-integrations and paste its secret")]
    NoToken,
    #[error("no Notion page chosen — paste the link of the page to export into")]
    NoParentPage,
    #[error(
        "'{0}' does not look like a Notion page link — copy the page's link from Notion itself"
    )]
    BadPageLink(String),
    #[error("Notion rejected the token ({0}) — check the integration secret, and that the integration has not been revoked")]
    BadToken(String),
    #[error("Notion cannot see that page ({0}) — open the page in Notion, press ⋯ → Connections, and add your integration")]
    PageNotShared(String),
    #[error("Notion is not answering right now ({0}) — try the export again in a moment")]
    Unavailable(String),
    #[error("Notion rejected the export: {0}")]
    Rejected(String),
    #[error("could not reach Notion: {0}")]
    Unreachable(String),
}

/// Turn a failed Notion response into the one of [`NotionError`] that names
/// its fix.
///
/// The `code` field is preferred over the status because the two disagree in
/// the case that matters: a page the integration has not been shared with is a
/// plain `404 object_not_found`, the same status a typo in the link produces,
/// and both have the same fix — go into Notion and connect the page.
pub fn classify(status: u16, body: &str) -> NotionError {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let field = |name: &str| -> Option<String> {
        parsed
            .as_ref()
            .and_then(|v| v.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let code = field("code");
    // The message, when Notion sent one, is worth more than the raw body: it
    // names the offending property or id.
    let detail = field("message").unwrap_or_else(|| preview(body));
    let detail = format!("HTTP {status}: {detail}");

    match code.as_deref() {
        Some("unauthorized" | "invalid_request_url" | "missing_version") => {
            NotionError::BadToken(detail)
        }
        Some("object_not_found") => NotionError::PageNotShared(detail),
        // The integration exists and the page is connected, but the workspace
        // or the integration's capabilities forbid the write. That is still
        // something to fix on the integration, not on the page.
        Some("restricted_resource" | "insufficient_permissions") => NotionError::BadToken(detail),
        Some("rate_limited" | "service_unavailable" | "internal_server_error") => {
            NotionError::Unavailable(detail)
        }
        Some("validation_error") => NotionError::Rejected(detail),
        // No recognisable code: fall back to the status, which is still enough
        // to separate "your credential" from "their server".
        _ => match status {
            401 | 403 => NotionError::BadToken(detail),
            404 => NotionError::PageNotShared(detail),
            429 | 500..=599 => NotionError::Unavailable(detail),
            _ => NotionError::Rejected(detail),
        },
    }
}

/// Trims a response body down to something that fits in an error message.
fn preview(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 200 {
        let short: String = trimmed.chars().take(200).collect();
        format!("{short}…")
    } else if trimmed.is_empty() {
        "no response body".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The 32-hex page id inside whatever the user pasted.
///
/// Notion offers "Copy link", so a link is what arrives — `https://www.notion.so/
/// My-Page-<32 hex>?pvs=4`. A bare id and a dashed UUID are accepted too,
/// because both are what someone who knows the API will paste.
///
/// The id is found by *segment*, not by filtering hex characters out of the
/// whole slug: a page called "Beta Feedback" contributes `bea` and `eedbac` to
/// such a filter, and the resulting id would be silently wrong.
pub fn parse_page_id(input: &str) -> Result<String, NotionError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(NotionError::NoParentPage);
    }
    let head = trimmed.split(['?', '#']).next().unwrap_or(trimmed);
    let tail = head
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(head);

    let undashed: String = tail.chars().filter(|c| *c != '-').collect();
    let candidate = if is_page_id(&undashed) {
        // A bare id or a dashed UUID.
        undashed
    } else {
        // A slug: the id is the last dash-separated piece.
        tail.rsplit('-').next().unwrap_or_default().to_string()
    };

    if !is_page_id(&candidate) {
        return Err(NotionError::BadPageLink(preview(trimmed)));
    }
    Ok(dashed(&candidate))
}

fn is_page_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 32 hex characters as the 8-4-4-4-12 form Notion echoes back.
fn dashed(id: &str) -> String {
    let mut out = String::with_capacity(36);
    for (index, ch) in id.chars().enumerate() {
        if matches!(index, 8 | 12 | 16 | 20) {
            out.push('-');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// The body of `POST /v1/pages`: a child page of `parent_page_id`, titled, with
/// its first blocks.
pub fn create_page_payload(parent_page_id: &str, title: &str, children: &[Value]) -> Value {
    json!({
        "parent": { "type": "page_id", "page_id": parent_page_id },
        "properties": {
            "title": { "title": rich_text(&truncate(title, MAX_RICH_TEXT_CHARS)) }
        },
        "children": children,
    })
}

/// The body of `PATCH /v1/blocks/{id}/children`, for everything past the first
/// [`MAX_CHILDREN_PER_REQUEST`] blocks.
pub fn append_children_payload(children: &[Value]) -> Value {
    json!({ "children": children })
}

/// The created page's own URL, so the app can offer to open what it wrote.
pub fn page_url(response_body: &str) -> Option<String> {
    serde_json::from_str::<Value>(response_body)
        .ok()?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

/// The created page's id, needed to append the rest of a long document.
pub fn page_id(response_body: &str) -> Option<String> {
    serde_json::from_str::<Value>(response_body)
        .ok()?
        .get("id")?
        .as_str()
        .map(str::to_string)
}

/// Split blocks into request-sized batches.
pub fn chunk_children(blocks: Vec<Value>) -> Vec<Vec<Value>> {
    if blocks.is_empty() {
        return Vec::new();
    }
    blocks
        .chunks(MAX_CHILDREN_PER_REQUEST)
        .map(<[Value]>::to_vec)
        .collect()
}

/// Markdown → Notion blocks.
///
/// Deliberately a small, total converter over the Markdown
/// [`super::export::meeting_to_markdown`] actually emits — headings, bullets,
/// checkboxes and paragraphs — rather than a general Markdown parser. Anything
/// it does not recognise becomes a paragraph, which is lossy in styling and
/// never lossy in content.
pub fn markdown_to_blocks(markdown: &str) -> Vec<Value> {
    markdown
        .lines()
        .filter_map(|line| block_for_line(line.trim_end()))
        .collect()
}

fn block_for_line(line: &str) -> Option<Value> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        // Blocks are already separate; a blank line would become an empty
        // paragraph and double every gap.
        return None;
    }
    // Notion has three heading levels; deeper Markdown headings flatten onto
    // the last one rather than disappearing.
    for (marker, kind) in [
        ("#### ", "heading_3"),
        ("### ", "heading_3"),
        ("## ", "heading_2"),
        ("# ", "heading_1"),
    ] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(text_block(kind, rest));
        }
    }
    for marker in ["- [ ] ", "* [ ] "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(todo_block(rest, false));
        }
    }
    for marker in ["- [x] ", "- [X] ", "* [x] ", "* [X] "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(todo_block(rest, true));
        }
    }
    for marker in ["- ", "* "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(text_block("bulleted_list_item", rest));
        }
    }
    Some(text_block("paragraph", trimmed))
}

fn text_block(kind: &str, text: &str) -> Value {
    json!({
        "object": "block",
        "type": kind,
        kind: { "rich_text": rich_text(text) },
    })
}

fn todo_block(text: &str, checked: bool) -> Value {
    json!({
        "object": "block",
        "type": "to_do",
        "to_do": { "rich_text": rich_text(text), "checked": checked },
    })
}

/// One line of Markdown as Notion rich text, with `**bold**` honoured.
///
/// Bold is the only inline style worth carrying: the exported document uses it
/// for every field label ("**Started:**"), and leaving the asterisks in is the
/// difference between a document that reads as written and one that reads as
/// raw Markdown someone forgot to render.
///
/// Runs are also split at [`MAX_RICH_TEXT_CHARS`], which Notion rejects the
/// whole request over — a single long transcript line is enough to hit it.
pub fn rich_text(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (chunk, bold) in bold_spans(text) {
        if chunk.is_empty() {
            continue;
        }
        for piece in split_chars(&chunk, MAX_RICH_TEXT_CHARS) {
            out.push(json!({
                "type": "text",
                "text": { "content": piece },
                "annotations": { "bold": bold },
            }));
        }
    }
    if out.is_empty() {
        // Notion accepts an empty array, but an empty *block* is easier to
        // reason about than a block with no rich text at all.
        out.push(json!({ "type": "text", "text": { "content": "" } }));
    }
    out
}

/// `text` split into (span, is_bold) at `**` pairs. An unmatched `**` is left
/// as literal text rather than bolding the rest of the line.
fn bold_spans(text: &str) -> Vec<(String, bool)> {
    let mut spans = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    while let Some(open) = rest.find("**") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("**") else {
            break;
        };
        plain.push_str(&rest[..open]);
        if !plain.is_empty() {
            spans.push((std::mem::take(&mut plain), false));
        }
        spans.push((after_open[..close].to_string(), true));
        rest = &after_open[close + 2..];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        spans.push((plain, false));
    }
    spans
}

fn split_chars(text: &str, limit: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return vec![text.to_string()];
    }
    chars
        .chunks(limit)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_page_link_yields_its_id() {
        assert_eq!(
            parse_page_id("https://www.notion.so/My-Notes-0123456789abcdef0123456789abcdef?pvs=4")
                .unwrap(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
    }

    /// The bug a "filter the hex characters out of the slug" parser has: every
    /// letter of "Beta" but `t` is a hex digit, so the id would gain a prefix.
    #[test]
    fn a_title_full_of_hex_letters_does_not_corrupt_the_id() {
        let id = parse_page_id(
            "https://www.notion.so/team/Beta-Feedback-decade-0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert_eq!(id, "01234567-89ab-cdef-0123-456789abcdef");
    }

    #[test]
    fn a_bare_id_or_a_dashed_uuid_is_accepted() {
        let expected = "01234567-89ab-cdef-0123-456789abcdef";
        assert_eq!(
            parse_page_id("0123456789ABCDEF0123456789abcdef").unwrap(),
            expected
        );
        assert_eq!(parse_page_id(expected).unwrap(), expected);
    }

    #[test]
    fn nonsense_is_refused_rather_than_sent() {
        assert_eq!(parse_page_id("   "), Err(NotionError::NoParentPage));
        assert!(matches!(
            parse_page_id("https://www.notion.so/My-Notes"),
            Err(NotionError::BadPageLink(_))
        ));
        assert!(matches!(
            parse_page_id("my notion page"),
            Err(NotionError::BadPageLink(_))
        ));
    }

    /// The whole point of the enum: these three have three different fixes.
    #[test]
    fn each_refusal_names_its_own_fix() {
        let unshared = classify(
            404,
            r#"{"object":"error","status":404,"code":"object_not_found","message":"Could not find page with ID 0123."}"#,
        );
        assert!(matches!(unshared, NotionError::PageNotShared(_)));
        assert!(unshared.to_string().contains("Connections"));

        let bad_token = classify(
            401,
            r#"{"object":"error","status":401,"code":"unauthorized","message":"API token is invalid."}"#,
        );
        assert!(matches!(bad_token, NotionError::BadToken(_)));
        assert!(bad_token.to_string().contains("integration secret"));

        let down = classify(
            503,
            r#"{"object":"error","status":503,"code":"service_unavailable"}"#,
        );
        assert!(matches!(down, NotionError::Unavailable(_)));
        assert!(down.to_string().contains("try the export again"));
    }

    /// A body that is not Notion's JSON at all — a proxy's HTML error page,
    /// say — must still land on the right side of "your fault / their fault".
    #[test]
    fn a_body_with_no_code_falls_back_to_the_status() {
        assert!(matches!(
            classify(401, "<html>Unauthorized</html>"),
            NotionError::BadToken(_)
        ));
        assert!(matches!(classify(404, ""), NotionError::PageNotShared(_)));
        assert!(matches!(
            classify(502, "bad gateway"),
            NotionError::Unavailable(_)
        ));
        assert!(matches!(classify(418, "teapot"), NotionError::Rejected(_)));
    }

    #[test]
    fn headings_bullets_and_checkboxes_become_their_own_block_types() {
        let blocks = markdown_to_blocks(
            "# Title\n\n## Summary\n\nWe shipped it.\n\n- a bullet\n- [ ] open item\n- [x] done item\n",
        );
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec![
                "heading_1",
                "heading_2",
                "paragraph",
                "bulleted_list_item",
                "to_do",
                "to_do",
            ]
        );
        assert_eq!(blocks[4]["to_do"]["checked"], json!(false));
        assert_eq!(blocks[5]["to_do"]["checked"], json!(true));
        assert_eq!(
            blocks[1]["heading_2"]["rich_text"][0]["text"]["content"],
            json!("Summary")
        );
    }

    #[test]
    fn field_labels_arrive_bold_rather_than_asterisked() {
        let runs = rich_text("**Started:** 2026-09-19");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["text"]["content"], json!("Started:"));
        assert_eq!(runs[0]["annotations"]["bold"], json!(true));
        assert_eq!(runs[1]["text"]["content"], json!(" 2026-09-19"));
        assert_eq!(runs[1]["annotations"]["bold"], json!(false));
    }

    /// An unmatched marker is content, not the start of a style that eats the
    /// rest of the line.
    #[test]
    fn a_lone_asterisk_pair_stays_literal() {
        let runs = rich_text("a ** b");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["text"]["content"], json!("a ** b"));
    }

    /// Notion rejects the whole request over one oversized run, and a
    /// transcript line is easily long enough.
    #[test]
    fn a_very_long_line_is_split_into_accepted_runs() {
        let long = "x".repeat(MAX_RICH_TEXT_CHARS * 2 + 5);
        let runs = rich_text(&long);
        assert_eq!(runs.len(), 3);
        for run in &runs {
            assert!(
                run["text"]["content"].as_str().unwrap().chars().count() <= MAX_RICH_TEXT_CHARS
            );
        }
    }

    #[test]
    fn children_are_batched_at_the_api_limit() {
        let blocks = markdown_to_blocks(&"a line\n".repeat(250));
        let chunks = chunk_children(blocks);
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![100, 100, 50]
        );
        assert!(chunk_children(Vec::new()).is_empty());
    }

    #[test]
    fn the_create_body_names_the_parent_and_the_title() {
        let blocks = markdown_to_blocks("# Hi\n");
        let payload =
            create_page_payload("01234567-89ab-cdef-0123-456789abcdef", "Standup", &blocks);
        assert_eq!(payload["parent"]["type"], json!("page_id"));
        assert_eq!(
            payload["parent"]["page_id"],
            json!("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert_eq!(
            payload["properties"]["title"]["title"][0]["text"]["content"],
            json!("Standup")
        );
        assert_eq!(payload["children"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_created_page_reports_where_it_landed() {
        let body = r#"{"object":"page","id":"abc","url":"https://www.notion.so/Standup-abc"}"#;
        assert_eq!(page_id(body).as_deref(), Some("abc"));
        assert_eq!(
            page_url(body).as_deref(),
            Some("https://www.notion.so/Standup-abc")
        );
        assert_eq!(page_url("not json"), None);
    }
}
