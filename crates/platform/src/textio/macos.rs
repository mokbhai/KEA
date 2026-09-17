//! macOS text I/O: synthetic Cmd+C / Cmd+V with clipboard save/restore (D4).
//! Optional Accessibility insertion via [`super::macos_ax`] when
//! [`super::ReplaceMode::Accessibility`] is selected (D12).
//!
//! Both synthetic paths need **Accessibility** permission: macOS drops
//! `CGEventPost`ed keystrokes from an untrusted process without telling the
//! sender, so they are refused up front rather than reported as successful
//! no-ops (see [`require_event_injection`]).
//!
//! # Manual verification
//! 1. Grant Accessibility permission to the host app.
//! 2. Select text in TextEdit (or any app) and call `capture_selection()` — clipboard is preserved (original contents restored after the synthetic copy).
//! 3. Call `replace("rewritten")` — selection should become `rewritten`; prior clipboard contents restored after ~80ms.
//! 4. Call `replace_with_mode("rewritten", ReplaceMode::Accessibility)` with AX permission — selection updates via AX when supported.
//! 5. Revoke Accessibility in System Settings and repeat 2–3: both must now fail with the grant instructions instead of appearing to work.
//! 6. Headless CI cannot drive real focus or key injection — unit tests cover [`super::ClipboardPlan`], the trust gate and AX seams only.

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

/// What the user is told when the OS will not deliver our synthetic keystrokes.
///
/// Named after the System Settings pane so the message is actionable without a
/// trip to the docs; `open_accessibility_settings` opens exactly this pane.
pub(crate) const AX_NOT_TRUSTED: &str =
    "KEA needs Accessibility permission to type into other apps. \
Grant it in System Settings → Privacy & Security → Accessibility, then try again.";

/// Refuses a synthetic-keystroke operation when this process is not trusted for
/// Accessibility.
///
/// `CGEventPost` returns `void`. Since macOS 10.14 the window server simply
/// drops keyboard events posted by a process that TCC has not granted
/// Accessibility, and the caller is told nothing at all. Everything downstream
/// therefore reported success on a paste that never happened: `enigo` returned
/// `Ok`, `insert_at_cursor` returned `Ok`, dictation wrote an `ok` action row,
/// the success cue played and the UI said "Typed into the app you were last
/// in" — while the focused text field stayed empty. That is the whole bug:
/// not a failed paste, a failure with no way to notice it.
///
/// Observed on KEA 0.2.0 with Accessibility showing "Denied" in its own
/// Permissions panel: microphone capture and Whisper transcription both
/// completed, `paste_via_clipboard_sync` returned `Ok(())`, and nothing was
/// inserted. The same code inserted correctly from a process TCC did trust.
///
/// Carbon hotkeys (`RegisterEventHotKey`) need no such grant, which is why the
/// shortcut kept working and made the failure look like a paste bug rather than
/// a permission one.
pub(crate) fn require_event_injection(trusted: bool) -> Result<(), TextIoError> {
    if trusted {
        return Ok(());
    }
    Err(TextIoError::Other(AX_NOT_TRUSTED.into()))
}

/// Releases modifiers the user is probably still holding.
///
/// These run straight off a hotkey, so the physical keys are usually still
/// down — with Cmd+Shift+R that makes the synthetic Cmd+C arrive as
/// Cmd+Shift+C, firing whatever owns *that* shortcut (a clipboard manager,
/// say) instead of copying. Cmd+Shift+V is worse: it is "paste and match
/// style" in many apps. A synthetic release is harmless if the key is already
/// up, and the later physical release just repeats it.
fn release_conflicting_modifiers(enigo: &mut Enigo) {
    for key in [Key::Shift, Key::Control, Key::Alt] {
        // Best effort: a modifier we cannot release must not abort the copy.
        let _ = enigo.key(key, Direction::Release);
    }
    // Let the window server settle the flag change before the chord lands.
    thread::sleep(Duration::from_millis(20));
}

fn synthesize_copy(enigo: &mut Enigo) -> Result<(), TextIoError> {
    release_conflicting_modifiers(enigo);
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
    release_conflicting_modifiers(enigo);
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
    // Before anything touches the clipboard: an untrusted process cannot
    // deliver the Cmd+C either, and the read below would then hand back
    // whatever the user had copied earlier as if it were their selection —
    // so rewrite would silently rewrite the wrong text.
    require_event_injection(super::macos_ax::is_ax_trusted())?;

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
    // Checked before the clipboard is written, not after: without the grant
    // the Cmd+V never arrives, and setting (then restoring) the clipboard
    // would be pure side effect on the way to a lie.
    require_event_injection(super::macos_ax::is_ax_trusted())?;

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

    #[test]
    fn untrusted_process_is_refused_with_an_actionable_message() {
        let err = require_event_injection(false).unwrap_err();
        assert_eq!(err, TextIoError::Other(AX_NOT_TRUSTED.into()));
        // The message has to name the pane the user must open; "permission
        // denied" alone leaves them exactly as stuck as the silent failure.
        assert!(err.to_string().contains("Accessibility"));
        assert!(require_event_injection(true).is_ok());
    }

    /// Regression test for the dictation transcript that never reached the
    /// focused text field.
    ///
    /// Before the trust check this returned `Ok(())`: the clipboard was
    /// written, `CGEventPost` was called, macOS dropped the keystrokes because
    /// the process was not trusted, and every layer above reported success. An
    /// `Ok` here is the bug, not a pass.
    ///
    /// It drives the real `insert_at_cursor` on purpose — a test against the
    /// private helper would not have caught a caller that forgot to consult it.
    #[tokio::test]
    async fn insert_at_cursor_refuses_when_accessibility_is_not_granted() {
        let _guard = super::super::macos_ax::AxTrustOverride::force(false);
        let err = MacTextIo::new()
            .insert_at_cursor("dictated text")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Accessibility"),
            "expected an Accessibility permission error, got: {err}"
        );
    }

    /// The same silent failure in the rewrite direction, where it is worse: an
    /// undelivered Cmd+C leaves the *previous* clipboard in place, and
    /// `capture_selection` would return that as if it were the selection.
    #[tokio::test]
    async fn capture_selection_refuses_when_accessibility_is_not_granted() {
        let _guard = super::super::macos_ax::AxTrustOverride::force(false);
        let err = MacTextIo::new().capture_selection().await.unwrap_err();
        assert!(
            err.to_string().contains("Accessibility"),
            "expected an Accessibility permission error, got: {err}"
        );
    }
}
