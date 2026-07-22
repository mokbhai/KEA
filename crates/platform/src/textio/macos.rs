//! macOS text I/O: synthetic Cmd+C / Cmd+V with clipboard save/restore (D4).
//! Optional Accessibility insertion via [`super::macos_ax`] when
//! [`super::ReplaceMode::Accessibility`] is selected (D12).
//!
//! # Manual verification
//! 1. Grant Accessibility permission to the host app.
//! 2. Select text in TextEdit (or any app) and call `capture_selection()` — clipboard is preserved (original contents restored after the synthetic copy).
//! 3. Call `replace("rewritten")` — selection should become `rewritten`; prior clipboard contents restored after ~80ms.
//! 4. Call `replace_with_mode("rewritten", ReplaceMode::Accessibility)` with AX permission — selection updates via AX when supported.
//! 5. Headless CI cannot drive real focus or key injection — unit tests cover [`super::ClipboardPlan`] and AX seams only.

use super::{ClipboardPlan, ReplaceMode, TextIo, TextIoError};
use async_trait::async_trait;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread;
use std::time::Duration;

/// Stateless macOS backend; `Enigo` is created per operation on a blocking thread.
#[derive(Debug, Default)]
pub struct MacTextIo;

impl MacTextIo {
    pub fn new() -> Self {
        Self
    }
}

// Use enigo::Keyboard::raw() with hardcoded CGKeyCode values to bypass enigo's
// layout-dependent keycode resolution (Key::Unicode), which calls
// TISGetInputSourceProperty → dispatch_assert_queue on macOS 26 Tahoe, crashing
// background threads. Key::Meta is static and does not hit the TSM path.
// Keycodes are physical scan codes, stable across all keyboard layouts:
// kVK_ANSI_C = 8, kVK_ANSI_V = 9

fn synthesize_copy(enigo: &mut Enigo) -> Result<(), TextIoError> {
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    enigo
        .raw(8, Direction::Click)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    Ok(())
}

fn synthesize_paste(enigo: &mut Enigo) -> Result<(), TextIoError> {
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    enigo
        .raw(9, Direction::Click)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|e| TextIoError::Other(e.to_string()))?;
    Ok(())
}

fn capture_selection_sync() -> Result<String, TextIoError> {
    // Save the user's original clipboard before Cmd+C overwrites it.
    let original = {
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|e| TextIoError::Other(format!("clipboard unavailable: {e}")))?;
        ClipboardPlan::capture(&mut clipboard)?
    };

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| TextIoError::Other(format!("keyboard input unavailable: {e}")))?;
    synthesize_copy(&mut enigo)?;
    thread::sleep(Duration::from_millis(50));

    let text = {
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|e| TextIoError::Other(format!("clipboard unavailable: {e}")))?;
        let text = clipboard
            .get_text()
            .map_err(|e| TextIoError::Other(e.to_string()))?;
        // Restore user's original clipboard immediately so it is clean during
        // LLM latency (TTS) and so paste_via_clipboard_sync saves the real
        // original rather than the captured selection. Non-text clipboard
        // contents (images, files) cannot be preserved through arboard 3.x
        // default feature set and are cleared.
        original.restore(&mut clipboard)?;
        text
    };

    if text.trim().is_empty() {
        return Err(TextIoError::Other("no selection".into()));
    }
    Ok(text)
}

/// Clipboard save → set text → synthetic Cmd+V → restore (D4 paste path).
fn paste_via_clipboard_sync(text: &str) -> Result<(), TextIoError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| TextIoError::Other(format!("clipboard unavailable: {e}")))?;
    let plan = ClipboardPlan::capture(&mut clipboard)?;
    clipboard
        .set_text(text)
        .map_err(|e| TextIoError::Other(e.to_string()))?;

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| TextIoError::Other(format!("keyboard input unavailable: {e}")))?;
    synthesize_paste(&mut enigo)?;
    thread::sleep(Duration::from_millis(80));
    plan.restore(&mut clipboard)
}

fn replace_with_mode_sync(text: &str, mode: ReplaceMode) -> Result<(), TextIoError> {
    match mode {
        ReplaceMode::ClipboardPaste => paste_via_clipboard_sync(text),
        ReplaceMode::Accessibility => match super::macos_ax::insert_via_accessibility(text) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!("AX insertion failed, falling back to clipboard: {err}");
                paste_via_clipboard_sync(text)
            }
        },
    }
}

#[cfg(test)]
pub(crate) fn resolve_replace_with_fallback(
    mode: ReplaceMode,
    ax_insert: impl FnOnce(&str) -> Result<(), String>,
    clipboard_insert: impl FnOnce(&str) -> Result<(), TextIoError>,
    text: &str,
) -> Result<(), TextIoError> {
    match mode {
        ReplaceMode::ClipboardPaste => clipboard_insert(text),
        ReplaceMode::Accessibility => match ax_insert(text) {
            Ok(()) => Ok(()),
            Err(_) => clipboard_insert(text),
        },
    }
}

#[async_trait]
impl TextIo for MacTextIo {
    async fn capture_selection(&self) -> Result<String, TextIoError> {
        tokio::task::spawn_blocking(capture_selection_sync)
            .await
            .map_err(|e| TextIoError::Other(e.to_string()))?
    }

    async fn replace_with_mode(
        &self,
        text: &str,
        mode: ReplaceMode,
    ) -> Result<(), TextIoError> {
        let text = text.to_string();
        tokio::task::spawn_blocking(move || replace_with_mode_sync(&text, mode))
            .await
            .map_err(|e| TextIoError::Other(e.to_string()))?
    }

    async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError> {
        let text = text.to_string();
        tokio::task::spawn_blocking(move || paste_via_clipboard_sync(&text))
            .await
            .map_err(|e| TextIoError::Other(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessibility_mode_falls_back_to_clipboard_on_ax_failure() {
        let out = resolve_replace_with_fallback(
            ReplaceMode::Accessibility,
            |_| Err("ax failed".into()),
            |_| Ok(()),
            "hello",
        );
        assert!(out.is_ok());
    }

    #[test]
    fn accessibility_mode_skips_clipboard_when_ax_succeeds() {
        let mut clipboard_called = false;
        let out = resolve_replace_with_fallback(
            ReplaceMode::Accessibility,
            |_| Ok(()),
            |_| {
                clipboard_called = true;
                Ok(())
            },
            "hello",
        );
        assert!(out.is_ok());
        assert!(!clipboard_called);
    }
}
