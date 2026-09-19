//! macOS text I/O: synthetic ⌘C / ⌘V with clipboard save/restore (D4).
//! Optional Accessibility insertion via [`super::macos_ax`] when
//! [`super::ReplaceMode::Accessibility`] is selected (D12).
//!
//! Both synthetic paths need **Accessibility** permission: macOS drops
//! `CGEventPost`ed keystrokes from an untrusted process without telling the
//! sender, so they are refused up front rather than reported as successful
//! no-ops (see [`require_event_injection`]).
//!
//! # Everything here is unobservable by default
//!
//! This is the only part of KEA where the OS refuses to say whether the thing
//! it was asked to do happened. `CGEventPost` returns `void`. The frontmost
//! app reads the pasteboard on its own schedule, in its own process, and
//! reports nothing back. So the module is built around the few signals that
//! *are* real, and it writes every one of them to the log at `info` (visible
//! on the Logs page with no configuration, which is the point — a paste that
//! fails once an hour cannot be reproduced under a debugger):
//!
//! * `AXIsProcessTrusted` — whether our keystrokes are delivered at all.
//! * `IsSecureEventInputEnabled` — whether *anyone's* are.
//! * `NSPasteboard.changeCount` — whether a copy or a write actually landed.
//! * [`super::macos_ax::focus_summary`] — which app and element the keystroke
//!   was aimed at.
//! * Elapsed time per stage, because the restore-too-early race below is
//!   invisible without it.
//!
//! # Manual verification
//! 1. Grant Accessibility permission to the host app.
//! 2. Select text in TextEdit (or any app) and call `capture_selection()` — clipboard is preserved (original contents restored after the synthetic copy).
//! 3. Call `replace("rewritten")` — selection should become `rewritten`; prior clipboard contents restored after the settle window.
//! 4. Call `replace_with_mode("rewritten", ReplaceMode::Accessibility)` with AX permission — selection updates via AX when supported.
//! 5. Revoke Accessibility in System Settings and repeat 2–3: both must now fail with the grant instructions instead of appearing to work.
//! 6. Dictate into Slack, Chrome and VS Code while still holding ⌥⇧ — the slow, Electron-shaped apps are where the old 80ms restore lost the race.
//! 7. Headless CI cannot drive real focus or key injection — unit tests cover [`super::ClipboardPlan`], the trust gate, the restore decision and the AX seams only.

use super::{ClipboardPlan, ReplaceMode, TextIo, TextIoError};
use async_trait::async_trait;
use std::thread;
use std::time::{Duration, Instant};

use super::macos_keys::{self, KEY_C, KEY_V};
use super::macos_pasteboard;

/// Stateless macOS backend; each operation runs on a blocking thread.
#[derive(Debug, Default)]
pub struct MacTextIo;

impl MacTextIo {
    pub fn new() -> Self {
        Self
    }
}

/// How long the frontmost app gets to consume the pasteboard before the user's
/// own clipboard is put back.
///
/// **This number was the bug.** It used to be 80ms. A synthetic ⌘V is
/// asynchronous all the way down: the window server delivers the key event to
/// the frontmost app, the app's run loop gets to it when it gets to it, and
/// only then does it read `NSPasteboard`. 80ms is comfortably enough for
/// TextEdit and comfortably *not* enough for an Electron app (Slack, VS Code,
/// Discord), a browser with a busy main thread, or any app that was just
/// woken from being idle — the exact places people dictate into. Losing that
/// race restores the user's old clipboard *before* the app reads it, so the
/// app pastes the old clipboard, or nothing, and both look like "the paste
/// didn't work" while every layer above reports success.
///
/// 400ms is chosen to cover a loaded Electron main thread rather than to feel
/// snappy: nothing is blocked on it that the user can see — the transcript is
/// already delivered, and this is only the invisible tidy-up afterwards.
const PASTE_SETTLE: Duration = Duration::from_millis(400);

/// How long the frontmost app gets to answer a synthetic ⌘C.
///
/// Same asynchrony as [`PASTE_SETTLE`], one direction earlier, and previously
/// 50ms. Reading too early returns the *previous* clipboard, which
/// `capture_selection` would then hand back as if it were the user's
/// selection — silently rewriting the wrong text.
const COPY_SETTLE: Duration = Duration::from_millis(150);

/// Attempts to get the transcript onto the pasteboard before giving up.
///
/// A pasteboard write can lose to another process declaring ownership at the
/// same instant — clipboard managers poll it several times a second — and
/// `arboard` reports that as success because `NSPasteboard` did accept the
/// write. Reading it back is the only way to know, and retrying is cheap.
const CLIPBOARD_WRITE_ATTEMPTS: usize = 3;

/// What the user is told when the OS will not deliver our synthetic keystrokes.
///
/// Named after the System Settings pane so the message is actionable without a
/// trip to the docs; `open_accessibility_settings` opens exactly this pane.
pub(crate) const AX_NOT_TRUSTED: &str =
    "KEA needs Accessibility permission to type into other apps. \
Grant it in System Settings → Privacy & Security → Accessibility, then try again.";

/// What the user is told when the transcript is on the clipboard but could not
/// be pasted. Always ends with the manual way out, because there is one.
pub(crate) fn paste_failed_but_copied(reason: &str) -> String {
    format!("{reason} The text is on your clipboard — press ⌘V to paste it.")
}

/// Explains a secure-input block in the user's terms.
///
/// This is not our permission to fix, and the usual cause (a password field
/// that still has focus, or Terminal's Secure Keyboard Entry) is not something
/// the user will connect to "dictation stopped working" on their own.
pub(crate) const SECURE_INPUT_BLOCKED: &str =
    "macOS is blocking synthetic keystrokes because an app has secure input on \
(a focused password field, or Terminal's \"Secure Keyboard Entry\").";

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

/// Whether the user's original clipboard should go back after a paste.
///
/// Pulled out of the I/O so the decision can be tested, because both wrong
/// answers are bad in ways that are hard to see in a manual run:
///
/// * Restoring when the paste failed throws away the transcript — the user's
///   words are then nowhere at all, which is the "it didn't even save to the
///   clipboard" half of the report. Leaving it means ⌘V still works.
/// * Restoring when somebody else has since written to the pasteboard silently
///   destroys *their* entry (a clipboard manager's, or something the user
///   copied in the meantime).
pub(crate) fn should_restore_clipboard(paste_succeeded: bool, clipboard_is_still_ours: bool) -> bool {
    paste_succeeded && clipboard_is_still_ours
}

/// One `info` line naming every condition that decides whether a synthetic
/// keystroke can work, taken immediately before it is posted.
///
/// Gathered together rather than logged where each is used so that a single
/// line in the log answers the whole question — a user pasting their log into
/// an issue should not have to correlate five timestamps.
fn log_injection_preconditions(op: &str, trusted: bool) {
    let flags = macos_keys::current_modifier_flags();
    tracing::info!(
        op,
        ax_trusted = trusted,
        secure_input = macos_keys::secure_input_enabled(),
        held_modifiers = %macos_keys::describe_modifiers(flags),
        focus = %super::macos_ax::focus_summary(),
        "textio: about to post a synthetic chord"
    );
}

/// Writes `text` and reads it back, retrying a write that did not stick.
fn set_clipboard_text_verified(
    clipboard: &mut arboard::Clipboard,
    text: &str,
) -> Result<(), TextIoError> {
    let mut last_problem = String::from("clipboard write did not take effect");
    for attempt in 1..=CLIPBOARD_WRITE_ATTEMPTS {
        if let Err(e) = clipboard.set_text(text) {
            last_problem = e.to_string();
            tracing::warn!(attempt, error = %last_problem, "textio: clipboard write failed");
        } else {
            match clipboard.get_text() {
                Ok(read_back) if read_back == text => {
                    tracing::info!(
                        attempt,
                        bytes = text.len(),
                        "textio: clipboard write verified"
                    );
                    return Ok(());
                }
                Ok(other) => {
                    last_problem = format!(
                        "clipboard held {} bytes of different text after the write",
                        other.len()
                    );
                    tracing::warn!(attempt, problem = %last_problem, "textio: clipboard write did not stick");
                }
                Err(e) => {
                    last_problem = e.to_string();
                    tracing::warn!(attempt, error = %last_problem, "textio: clipboard read-back failed");
                }
            }
        }
        // Another process declaring pasteboard ownership is the usual cause,
        // and it is over in a few milliseconds.
        thread::sleep(Duration::from_millis(25));
    }
    Err(TextIoError::Other(format!(
        "could not put the text on the clipboard: {last_problem}"
    )))
}

fn capture_selection_sync() -> Result<String, TextIoError> {
    let started = Instant::now();
    // Before anything touches the clipboard: an untrusted process cannot
    // deliver the ⌘C either, and the read below would then hand back
    // whatever the user had copied earlier as if it were their selection —
    // so rewrite would silently rewrite the wrong text.
    let trusted = super::macos_ax::is_ax_trusted();
    log_injection_preconditions("capture_selection", trusted);
    require_event_injection(trusted)?;

    // Save the user's original clipboard before ⌘C overwrites it.
    let original = {
        let mut clipboard = arboard::Clipboard::new()
            .map_err(|e| TextIoError::Other(format!("clipboard unavailable: {e}")))?;
        ClipboardPlan::capture(&mut clipboard)?
    };

    let before = macos_pasteboard::change_count();
    macos_keys::post_command_chord(KEY_C).map_err(TextIoError::Other)?;
    thread::sleep(COPY_SETTLE);
    let after = macos_pasteboard::change_count();

    if !macos_pasteboard::changed_between(before, after) {
        // The pasteboard never changed owner, so the ⌘C was not acted on. The
        // clipboard still holds the user's old data, and returning it here is
        // exactly how a rewrite ends up rewriting text the user never
        // selected. Refuse instead.
        tracing::warn!(
            op = "capture_selection",
            change_count_before = ?before,
            secure_input = macos_keys::secure_input_enabled(),
            focus = %super::macos_ax::focus_summary(),
            "textio: the synthetic copy did not reach any app"
        );
        let reason = if macos_keys::secure_input_enabled() {
            SECURE_INPUT_BLOCKED.to_string()
        } else {
            "the copy did not reach the app you were in — click into the text and select it again."
                .to_string()
        };
        return Err(TextIoError::Other(reason));
    }

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
        tracing::warn!(op = "capture_selection", "textio: the copied selection was empty");
        return Err(TextIoError::Other("no selection".into()));
    }

    tracing::info!(
        op = "capture_selection",
        bytes = text.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "textio: captured the selection"
    );
    Ok(text)
}

/// Clipboard save → set text → synthetic ⌘V → restore (D4 paste path).
fn paste_via_clipboard_sync(text: &str) -> Result<(), TextIoError> {
    let started = Instant::now();

    // Checked before the clipboard is written, not after: without the grant
    // the ⌘V never arrives, and setting (then restoring) the clipboard
    // would be pure side effect on the way to a lie.
    let trusted = super::macos_ax::is_ax_trusted();
    log_injection_preconditions("paste", trusted);
    require_event_injection(trusted)?;

    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| TextIoError::Other(format!("clipboard unavailable: {e}")))?;
    let plan = ClipboardPlan::capture(&mut clipboard)?;
    tracing::info!(
        op = "paste",
        saved_bytes = plan.restore_value().map(str::len),
        had_non_text = plan.had_non_text(),
        "textio: saved the user's clipboard"
    );

    if let Err(e) = set_clipboard_text_verified(&mut clipboard, text) {
        // The transcript never made it onto the clipboard, so there is nothing
        // here worth protecting — but a partial write may have clobbered the
        // user's data, and leaving it destroyed on the way out of a failure
        // would be gratuitous.
        if let Err(restore_err) = plan.restore(&mut clipboard) {
            tracing::warn!(
                op = "paste",
                error = %restore_err,
                "textio: could not put the user's clipboard back after a failed write"
            );
        }
        return Err(e);
    }
    let ours = macos_pasteboard::change_count();

    // From here the transcript is on the clipboard, so neither failure below
    // may return early: both still have to go past the restore decision, which
    // is what keeps the text reachable with a manual ⌘V instead of putting the
    // user's old clipboard back on top of it.
    let posted = if macos_keys::secure_input_enabled() {
        tracing::warn!(op = "paste", "textio: secure input is on; the keystroke will be dropped");
        Err(SECURE_INPUT_BLOCKED.to_string())
    } else {
        match macos_keys::post_command_chord(KEY_V) {
            Ok(()) => {
                tracing::info!(
                    op = "paste",
                    bytes = text.len(),
                    settle_ms = PASTE_SETTLE.as_millis() as u64,
                    "textio: posted ⌘V, waiting before restoring the clipboard"
                );
                thread::sleep(PASTE_SETTLE);
                Ok(())
            }
            Err(e) => {
                tracing::error!(op = "paste", error = %e, "textio: could not post the paste chord");
                Err(e)
            }
        }
    };

    // Only put the user's clipboard back if the paste got as far as being
    // posted *and* nothing else has claimed the pasteboard since our write;
    // otherwise the restore would destroy either the transcript or a clipboard
    // manager's entry.
    let still_ours = !macos_pasteboard::changed_between(ours, macos_pasteboard::change_count());
    if should_restore_clipboard(posted.is_ok(), still_ours) {
        plan.restore(&mut clipboard)?;
        tracing::info!(
            op = "paste",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "textio: paste posted and the clipboard restored"
        );
    } else {
        tracing::info!(
            op = "paste",
            posted = posted.is_ok(),
            still_ours,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "textio: left the clipboard as it is"
        );
    }

    posted.map_err(|e| TextIoError::Other(paste_failed_but_copied(&e)))
}

fn replace_with_mode_sync(text: &str, mode: ReplaceMode) -> Result<(), TextIoError> {
    match mode {
        ReplaceMode::ClipboardPaste => paste_via_clipboard_sync(text),
        ReplaceMode::Accessibility => match super::macos_ax::insert_via_accessibility(text) {
            Ok(()) => {
                tracing::info!(op = "replace", mode = "accessibility", "textio: inserted via AX");
                Ok(())
            }
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

    /// A failed paste must leave the transcript reachable. Restoring over it
    /// is the "it didn't even save to the clipboard" complaint: the user's
    /// words end up in no app and on no clipboard, with nothing to retry from.
    #[test]
    fn a_failed_paste_keeps_the_text_on_the_clipboard() {
        assert!(!should_restore_clipboard(false, true));
    }

    /// And a successful one tidies up after itself, which is the whole reason
    /// the save/restore dance exists.
    #[test]
    fn a_successful_paste_restores_the_user_clipboard() {
        assert!(should_restore_clipboard(true, true));
    }

    /// A clipboard manager that logged our write has since taken ownership.
    /// Restoring would overwrite whatever it (or the user) put there.
    #[test]
    fn a_clipboard_claimed_by_someone_else_is_left_alone() {
        assert!(!should_restore_clipboard(true, false));
    }

    #[test]
    fn the_failure_message_names_the_way_out() {
        let message = paste_failed_but_copied(SECURE_INPUT_BLOCKED);
        assert!(message.contains("secure input"));
        assert!(message.contains("⌘V"), "the user must be told they can paste it themselves");
    }

    /// The settle window is the thing that actually fixed the reported bug;
    /// a future "make it feel snappier" edit that takes it back under the
    /// Electron paste latency would restore the bug without failing anything
    /// else.
    #[test]
    fn the_paste_settle_window_clears_a_slow_apps_paste_latency() {
        assert!(
            PASTE_SETTLE >= Duration::from_millis(300),
            "80ms lost the race against Electron apps; do not go back under 300ms"
        );
        assert!(COPY_SETTLE >= Duration::from_millis(120));
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
    /// undelivered ⌘C leaves the *previous* clipboard in place, and
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
