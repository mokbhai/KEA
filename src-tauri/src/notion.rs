//! Sending a meeting to Notion: the one request shape, and the walk that turns
//! a Markdown document into a page.
//!
//! The *contents* — blocks, bodies, page links, what a refusal means — are
//! pure and live in `kea_core::meetings::notion`. What is here is the part
//! that touches the network.
//!
//! ## Why not the `HttpClient` port
//!
//! `kea_engines::http::HttpClient` exists and was the first thing tried. It
//! does not fit, for two reasons that are not stylistic: it only speaks POST,
//! and Notion needs PATCH to append the rest of a document longer than 100
//! blocks; and it attaches exactly one header pattern (`Authorization`), while
//! every Notion request must also carry `Notion-Version` or the API refuses
//! it. Widening that port for one consumer would push a Notion detail into the
//! shared inference path, so this is a small client scoped to this one API —
//! ~40 lines, behind a trait so the walk below is testable without a network.

use async_trait::async_trait;
use kea_core::meetings::notion::{
    append_children_payload, block_children_url, chunk_children, classify, create_page_payload,
    markdown_to_blocks, page_id, page_url, parse_page_id, NotionError, NOTION_API_VERSION,
    NOTION_PAGES_URL,
};
use serde_json::Value;

/// The two verbs the Notion export uses: create the page, then append what did
/// not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Post,
    Patch,
}

/// The Notion REST calls this export makes.
///
/// A trait so the multi-request walk in [`export_markdown`] — which is where
/// the batching and the "append to the page we just made" ordering live — can
/// be tested without a server.
#[async_trait]
pub trait NotionApi: Send + Sync {
    async fn send(
        &self,
        verb: Verb,
        url: &str,
        token: &str,
        body: Value,
    ) -> Result<String, NotionError>;
}

pub struct ReqwestNotionApi {
    client: reqwest::Client,
}

impl ReqwestNotionApi {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestNotionApi {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NotionApi for ReqwestNotionApi {
    async fn send(
        &self,
        verb: Verb,
        url: &str,
        token: &str,
        body: Value,
    ) -> Result<String, NotionError> {
        let request = match verb {
            Verb::Post => self.client.post(url),
            Verb::Patch => self.client.patch(url),
        };
        let response = request
            .bearer_auth(token)
            .header("Notion-Version", NOTION_API_VERSION)
            .json(&body)
            .send()
            .await
            // No status yet: DNS, TLS and "the wifi is off" all land here, and
            // none of them is something to fix on the token or the page.
            .map_err(|e| NotionError::Unreachable(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| NotionError::Unreachable(e.to_string()))?;
        if (200..300).contains(&status) {
            return Ok(text);
        }
        Err(classify(status, &text))
    }
}

/// What an export produced: where it landed, so the app can offer to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedPage {
    pub id: String,
    pub url: Option<String>,
}

/// Create a Notion page under `parent_link` holding `markdown`.
///
/// Notion caps a request at 100 child blocks, so a long meeting is one create
/// plus however many appends it takes. The appends are sequential and stop at
/// the first failure: a half-written page the user can see beats a silent
/// retry loop, and the error names which half it got to.
pub async fn export_markdown(
    api: &dyn NotionApi,
    token: &str,
    parent_link: &str,
    title: &str,
    markdown: &str,
) -> Result<ExportedPage, NotionError> {
    if token.trim().is_empty() {
        return Err(NotionError::NoToken);
    }
    let parent_id = parse_page_id(parent_link)?;
    let mut batches = chunk_children(markdown_to_blocks(markdown)).into_iter();
    // An empty document is still a page: the meeting's title and the fact it
    // was exported are worth keeping, and an error here would be a lie.
    let first = batches.next().unwrap_or_default();

    let created = api
        .send(
            Verb::Post,
            NOTION_PAGES_URL,
            token,
            create_page_payload(&parent_id, title, &first),
        )
        .await?;
    let id = page_id(&created).ok_or_else(|| {
        // A 2xx with no id is not something the user can fix, but it must not
        // read as success either.
        NotionError::Rejected("Notion accepted the page but returned no id".into())
    })?;

    let children_url = block_children_url(&id);
    for batch in batches {
        api.send(
            Verb::Patch,
            &children_url,
            token,
            append_children_payload(&batch),
        )
        .await?;
    }

    Ok(ExportedPage {
        url: page_url(&created),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_core::meetings::notion::MAX_CHILDREN_PER_REQUEST;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeNotion {
        calls: Mutex<Vec<(Verb, String, Value)>>,
        fail_on_call: Option<usize>,
    }

    #[async_trait]
    impl NotionApi for FakeNotion {
        async fn send(
            &self,
            verb: Verb,
            url: &str,
            _token: &str,
            body: Value,
        ) -> Result<String, NotionError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push((verb, url.to_string(), body));
            if self.fail_on_call == Some(calls.len()) {
                return Err(NotionError::Unavailable("HTTP 503".into()));
            }
            Ok(r#"{"id":"page-1","url":"https://www.notion.so/page-1"}"#.into())
        }
    }

    const PARENT: &str = "https://www.notion.so/Notes-0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn a_short_meeting_is_one_create() {
        let api = FakeNotion::default();
        let page = export_markdown(
            &api,
            "secret_x",
            PARENT,
            "Standup",
            "# Standup\n\nWe met.\n",
        )
        .await
        .unwrap();
        assert_eq!(page.id, "page-1");
        assert_eq!(page.url.as_deref(), Some("https://www.notion.so/page-1"));

        let calls = api.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, Verb::Post);
        assert_eq!(calls[0].1, NOTION_PAGES_URL);
        // The link the user pasted became the dashed id Notion wants.
        assert_eq!(
            calls[0].2["parent"]["page_id"],
            serde_json::json!("01234567-89ab-cdef-0123-456789abcdef")
        );
    }

    /// The case the 100-block cap creates: everything past the first batch has
    /// to be appended to the page that was just made, not to the parent.
    #[tokio::test]
    async fn a_long_meeting_appends_the_rest_to_the_new_page() {
        let api = FakeNotion::default();
        let markdown = "a line\n".repeat(MAX_CHILDREN_PER_REQUEST * 2 + 1);
        export_markdown(&api, "secret_x", PARENT, "Long", &markdown)
            .await
            .unwrap();

        let calls = api.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0].2["children"].as_array().unwrap().len(),
            MAX_CHILDREN_PER_REQUEST
        );
        for call in &calls[1..] {
            assert_eq!(call.0, Verb::Patch);
            assert_eq!(call.1, "https://api.notion.com/v1/blocks/page-1/children");
        }
        assert_eq!(calls[2].2["children"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_append_surfaces_rather_than_reporting_success() {
        let api = FakeNotion {
            fail_on_call: Some(2),
            ..FakeNotion::default()
        };
        let markdown = "a line\n".repeat(MAX_CHILDREN_PER_REQUEST + 1);
        let error = export_markdown(&api, "secret_x", PARENT, "Long", &markdown)
            .await
            .unwrap_err();
        assert!(matches!(error, NotionError::Unavailable(_)));
    }

    /// Both are refused before a request is made: neither is worth a round
    /// trip, and the 404 a bad link earns would be blamed on sharing.
    #[tokio::test]
    async fn a_missing_token_or_a_bad_link_never_reaches_the_network() {
        let api = FakeNotion::default();
        assert_eq!(
            export_markdown(&api, "  ", PARENT, "t", "x")
                .await
                .unwrap_err(),
            NotionError::NoToken
        );
        assert!(matches!(
            export_markdown(&api, "secret_x", "my page", "t", "x")
                .await
                .unwrap_err(),
            NotionError::BadPageLink(_)
        ));
        assert!(api.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_empty_meeting_still_produces_a_page() {
        let api = FakeNotion::default();
        export_markdown(&api, "secret_x", PARENT, "Empty", "")
            .await
            .unwrap();
        let calls = api.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].2["children"].as_array().unwrap().is_empty());
    }
}

/// The real client against a real HTTP server — headers included, because the
/// missing `Notion-Version` is a failure the fake above can never reproduce.
#[cfg(test)]
mod wire_tests {
    use super::*;
    use wiremock::matchers::{body_json_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn every_request_carries_the_bearer_and_the_api_version() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/pages"))
            .and(header("authorization", "Bearer secret_x"))
            .and(header("notion-version", NOTION_API_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"id":"p1"}"#))
            .mount(&server)
            .await;

        let api = ReqwestNotionApi::new();
        let body = api
            .send(
                Verb::Post,
                &format!("{}/v1/pages", server.uri()),
                "secret_x",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert!(body.contains("p1"));
    }

    /// The failure this whole feature is most likely to produce in the wild:
    /// the integration exists, the token is right, and nobody shared the page.
    #[tokio::test]
    async fn an_unshared_page_says_to_share_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_string(
                r#"{"object":"error","status":404,"code":"object_not_found","message":"Could not find page with ID 0123. Make sure the relevant pages and databases are shared with your integration."}"#,
            ))
            .mount(&server)
            .await;

        let api = ReqwestNotionApi::new();
        let error = api
            .send(
                Verb::Post,
                &format!("{}/v1/pages", server.uri()),
                "secret_x",
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, NotionError::PageNotShared(_)));
        assert!(error.to_string().contains("Connections"));
    }

    #[tokio::test]
    async fn a_patch_sends_the_children_body_it_was_given() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/v1/blocks/p1/children"))
            .and(body_json_string(r#"{"children":[]}"#))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"object":"list"}"#))
            .mount(&server)
            .await;

        let api = ReqwestNotionApi::new();
        api.send(
            Verb::Patch,
            &format!("{}/v1/blocks/p1/children", server.uri()),
            "secret_x",
            serde_json::json!({ "children": [] }),
        )
        .await
        .unwrap();
    }

    /// A server that is not there at all is neither a bad token nor an
    /// unshared page, and must not be reported as either.
    #[tokio::test]
    async fn an_unreachable_server_is_its_own_failure() {
        let api = ReqwestNotionApi::new();
        let error = api
            .send(
                Verb::Post,
                // Reserved by RFC 2606 and guaranteed not to resolve.
                "https://notion.invalid/v1/pages",
                "secret_x",
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, NotionError::Unreachable(_)));
    }
}
