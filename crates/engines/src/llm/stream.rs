//! Token-at-a-time completions.
//!
//! ## Why this is a separate trait
//!
//! Same reason [`crate::traits::StreamingSttEngine`] is separate from
//! `SttEngine`: not every backend can stream, and an engine that answers
//! "unsupported" from a method on the main trait is how a feature ends up
//! silently inert. A caller that wants live text asks the registry for a
//! streaming engine and falls back to `complete` when there is none — which
//! is a visible, correct downgrade rather than a broken one.
//!
//! ## Why the port carries bytes, not events
//!
//! Both wire formats here are SSE, and both would fit a shared parser — but
//! they frame differently enough that a shared *event* type would be a lie.
//! OpenAI sends `data:` lines carrying `choices[0].delta.content` and a
//! literal `[DONE]` sentinel; Anthropic sends named events whose payload is
//! `delta.text`. So [`crate::http::HttpClient::post_json_stream`] hands back
//! bytes and each engine supplies the one function that turns one `data:`
//! payload into text.
//!
//! ## What a chunk is, and what it is not
//!
//! A chunk is *display* text: the caller concatenates it. Nothing here
//! reports usage — a streamed response's token counts arrive in a final
//! event that half the providers omit, and [`crate::traits::LlmResponse`]
//! would rather say `None` than guess. Code that needs accounting uses
//! `complete`.

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};

use crate::http::ByteStream;
use crate::traits::{EngineError, LlmRequest};

/// A completion arriving in pieces. Each item is text to append.
pub type LlmStream = Pin<Box<dyn Stream<Item = Result<String, EngineError>> + Send + 'static>>;

/// An engine that can deliver a completion incrementally.
#[async_trait]
pub trait StreamingLlmEngine: Send + Sync {
    fn id(&self) -> &str;
    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, EngineError>;
}

/// What one `data:` payload meant.
///
/// `Done` is not merely "no text": OpenAI's `[DONE]` sentinel and Anthropic's
/// `message_stop` both arrive as payloads that must end the stream rather
/// than be parsed as content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Text(String),
    /// Parsed fine, carried no text — a role-only first delta, a ping.
    Empty,
    Done,
}

/// Decodes one OpenAI `data:` payload.
pub fn openai_event(payload: &str) -> SseEvent {
    if payload.trim() == "[DONE]" {
        return SseEvent::Done;
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload) else {
        // A malformed frame is skipped rather than failing the stream: the
        // text already delivered is still correct, and the alternative is
        // losing a whole answer to one bad line.
        return SseEvent::Empty;
    };
    match parsed["choices"][0]["delta"]["content"].as_str() {
        Some(text) if !text.is_empty() => SseEvent::Text(text.to_string()),
        _ => SseEvent::Empty,
    }
}

/// Decodes one Anthropic `data:` payload.
///
/// Keyed on the payload's own `type` rather than on the `event:` line: the
/// two always agree, and reading only `data:` means the decoder does not have
/// to carry state between the two lines of a frame.
pub fn anthropic_event(payload: &str) -> SseEvent {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload) else {
        return SseEvent::Empty;
    };
    if parsed["type"] == "message_stop" {
        return SseEvent::Done;
    }
    match parsed["delta"]["text"].as_str() {
        Some(text) if !text.is_empty() => SseEvent::Text(text.to_string()),
        _ => SseEvent::Empty,
    }
}

/// Splits a raw byte stream into SSE frames and runs `decode` over each one.
///
/// The buffering is the point: a chunk boundary falls wherever the network
/// put it, routinely mid-frame and sometimes mid-UTF-8-character, so frames
/// are only cut at a blank line and bytes are only decoded once a frame is
/// whole.
pub fn decode_sse(bytes: ByteStream, decode: fn(&str) -> SseEvent) -> LlmStream {
    struct State {
        buffer: Vec<u8>,
        finished: bool,
    }

    let state = State {
        buffer: Vec::new(),
        finished: false,
    };

    Box::pin(
        bytes
            .scan(state, move |state, chunk| {
                let out = match chunk {
                    Err(e) => Some(vec![Err(e)]),
                    Ok(_) if state.finished => Some(Vec::new()),
                    Ok(bytes) => {
                        state.buffer.extend_from_slice(&bytes);
                        let mut texts = Vec::new();
                        while let Some(frame) = take_frame(&mut state.buffer) {
                            for payload in data_payloads(&frame) {
                                match decode(&payload) {
                                    SseEvent::Text(text) => texts.push(Ok(text)),
                                    SseEvent::Empty => {}
                                    SseEvent::Done => {
                                        state.finished = true;
                                        state.buffer.clear();
                                    }
                                }
                            }
                            if state.finished {
                                break;
                            }
                        }
                        Some(texts)
                    }
                };
                std::future::ready(out)
            })
            .flat_map(futures_util::stream::iter),
    )
}

/// Removes and returns the next complete frame (everything up to and
/// including the blank line that ends it). `None` while the buffer holds only
/// a partial one.
///
/// Both terminators are recognised. `\r\n\r\n` is legal SSE and some
/// gateways emit it; matching only `\n\n` would find the second half of it
/// and cut the frame one byte late, leaving a stray `\r` at the head of the
/// next one.
fn take_frame(buffer: &mut Vec<u8>) -> Option<String> {
    let crlf = find(buffer, b"\r\n\r\n").map(|at| (at, 4));
    let lf = find(buffer, b"\n\n").map(|at| (at, 2));
    let (at, len) = match (crlf, lf) {
        (Some(crlf), Some(lf)) => {
            // The earlier terminator wins; a `\r\n\r\n` at position n also
            // matches `\n\n` at n + 1, so a tie on start position cannot
            // happen and the CRLF is always the earlier of the two.
            if crlf.0 <= lf.0 {
                crlf
            } else {
                lf
            }
        }
        (found, None) | (None, found) => found?,
    };
    let frame: Vec<u8> = buffer.drain(..at + len).collect();
    Some(String::from_utf8_lossy(&frame).into_owned())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The `data:` field(s) of one frame. SSE allows several, which are joined
/// with newlines; every provider here sends exactly one, and handling both
/// costs a `Vec`.
fn data_payloads(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|payload| payload.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_of(chunks: Vec<&'static str>) -> ByteStream {
        Box::pin(futures_util::stream::iter(
            chunks.into_iter().map(|c| Ok(c.as_bytes().to_vec())),
        ))
    }

    async fn collect(stream: LlmStream) -> Result<String, EngineError> {
        let mut out = String::new();
        let mut stream = stream;
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk?);
        }
        Ok(out)
    }

    #[test]
    fn openai_deltas_and_the_done_sentinel() {
        assert_eq!(
            openai_event(r#"{"choices":[{"delta":{"content":"hi"}}]}"#),
            SseEvent::Text("hi".into())
        );
        // The first delta of every OpenAI stream carries only a role.
        assert_eq!(
            openai_event(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            SseEvent::Empty
        );
        assert_eq!(openai_event("[DONE]"), SseEvent::Done);
        assert_eq!(openai_event("not json"), SseEvent::Empty);
    }

    #[test]
    fn anthropic_deltas_and_message_stop() {
        assert_eq!(
            anthropic_event(r#"{"type":"content_block_delta","delta":{"text":"hi"}}"#),
            SseEvent::Text("hi".into())
        );
        assert_eq!(anthropic_event(r#"{"type":"ping"}"#), SseEvent::Empty);
        assert_eq!(
            anthropic_event(r#"{"type":"message_stop"}"#),
            SseEvent::Done
        );
    }

    /// The failure this buffering exists to prevent: the network splits
    /// wherever it likes, including between the `d` and the `ata:`.
    #[tokio::test]
    async fn a_frame_split_across_chunks_is_reassembled() {
        let stream = decode_sse(
            bytes_of(vec![
                "data: {\"choices\":[{\"delta\":{\"con",
                "tent\":\"hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}",
                "\n\ndata: [DONE]\n\n",
            ]),
            openai_event,
        );
        assert_eq!(collect(stream).await.unwrap(), "hello");
    }

    /// Nothing after the sentinel is content, and a provider that keeps
    /// talking must not extend the answer.
    #[tokio::test]
    async fn everything_after_done_is_dropped() {
        let stream = decode_sse(
            bytes_of(vec![
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                "data: [DONE]\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n",
            ]),
            openai_event,
        );
        assert_eq!(collect(stream).await.unwrap(), "a");
    }

    /// A stream that just stops — no sentinel — still yields what arrived.
    /// Providers do this on a clean finish_reason and a truncated answer is
    /// better than an error that throws the text away.
    #[tokio::test]
    async fn a_stream_that_ends_without_a_sentinel_keeps_its_text() {
        let stream = decode_sse(
            bytes_of(vec![
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            ]),
            openai_event,
        );
        assert_eq!(collect(stream).await.unwrap(), "a");
    }

    /// A partial trailing frame is never emitted: half a JSON object would
    /// decode as `Empty` at best and as garbage text at worst.
    #[tokio::test]
    async fn a_truncated_trailing_frame_is_not_emitted() {
        let stream = decode_sse(
            bytes_of(vec![
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"cont",
            ]),
            openai_event,
        );
        assert_eq!(collect(stream).await.unwrap(), "a");
    }

    #[tokio::test]
    async fn a_transport_error_surfaces_rather_than_truncating_silently() {
        let bytes: ByteStream = Box::pin(futures_util::stream::iter(vec![
            Ok(b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n".to_vec()),
            Err(EngineError::Other("connection reset".into())),
        ]));
        let mut stream = decode_sse(bytes, openai_event);
        assert_eq!(stream.next().await.unwrap().unwrap(), "a");
        assert!(stream.next().await.unwrap().is_err());
    }

    /// `\r\n` line endings are legal SSE and some gateways emit them.
    #[tokio::test]
    async fn crlf_framing_decodes() {
        let stream = decode_sse(
            bytes_of(vec![
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\r\n\r\n",
            ]),
            openai_event,
        );
        assert_eq!(collect(stream).await.unwrap(), "a");
    }
}
