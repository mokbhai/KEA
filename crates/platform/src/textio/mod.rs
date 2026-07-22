//! Selected-text capture and in-place replacement (clipboard + synthetic paste).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_ax;
#[cfg(not(target_os = "macos"))]
pub mod stub;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplaceMode {
    /// Clipboard save → set text → synthetic Cmd/Ctrl+V → restore (D4 baseline).
    #[default]
    ClipboardPaste,
    /// macOS Accessibility API insertion (D12); falls back to clipboard on failure.
    Accessibility,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TextIoError {
    #[error("{0}")]
    Other(String),
}

/// Captures the current selection and replaces it in place via the OS clipboard.
#[async_trait]
pub trait TextIo: Send + Sync {
    async fn capture_selection(&self) -> Result<String, TextIoError>;

    /// Replace the current selection using the default clipboard+paste path (D4).
    async fn replace(&self, text: &str) -> Result<(), TextIoError> {
        self.replace_with_mode(text, ReplaceMode::ClipboardPaste)
            .await
    }

    /// Replace the current selection using the given mode.
    async fn replace_with_mode(
        &self,
        text: &str,
        mode: ReplaceMode,
    ) -> Result<(), TextIoError> {
        let _ = mode;
        self.replace(text).await
    }

    /// Insert text at the caret without requiring a prior selection (dictation).
    /// Defaults to the same clipboard+paste path as `replace`; OS impls may override.
    async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError> {
        self.replace(text).await
    }
}

/// Saved clipboard contents restored after a synthetic paste (D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPlan {
    saved_text: Option<String>,
    had_non_text: bool,
}

impl ClipboardPlan {
    /// Test helper: build a restore plan from a known text snapshot.
    pub fn save(saved: &str) -> Self {
        Self {
            saved_text: Some(saved.to_string()),
            had_non_text: false,
        }
    }

    pub fn restore_value(&self) -> Option<&str> {
        self.saved_text.as_deref()
    }

    pub fn had_non_text(&self) -> bool {
        self.had_non_text
    }

    pub fn capture(clipboard: &mut arboard::Clipboard) -> Result<Self, TextIoError> {
        match clipboard.get_text() {
            Ok(text) => Ok(Self {
                saved_text: Some(text),
                had_non_text: false,
            }),
            Err(arboard::Error::ContentNotAvailable) => Ok(Self {
                saved_text: None,
                had_non_text: true,
            }),
            Err(error) => Err(TextIoError::Other(error.to_string())),
        }
    }

    pub fn restore(&self, clipboard: &mut arboard::Clipboard) -> Result<(), TextIoError> {
        if self.had_non_text {
            clipboard
                .clear()
                .map_err(|e| TextIoError::Other(e.to_string()))?;
            return Ok(());
        }
        if let Some(text) = &self.saved_text {
            clipboard
                .set_text(text)
                .map_err(|e| TextIoError::Other(e.to_string()))?;
        } else {
            clipboard
                .clear()
                .map_err(|e| TextIoError::Other(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTextIo {
        selection: String,
        replaced: std::sync::Mutex<Option<String>>,
        last_mode: std::sync::Mutex<Option<ReplaceMode>>,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace_with_mode(
            &self,
            text: &str,
            mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            *self.replaced.lock().unwrap() = Some(text.to_string());
            *self.last_mode.lock().unwrap() = Some(mode);
            Ok(())
        }
    }

    /// Simulated clipboard for testing save/restore correctness across
    /// capture → replace flows without requiring a real window server.
    struct SimulatedClipboard {
        contents: std::sync::Mutex<Option<String>>,
    }

    impl SimulatedClipboard {
        fn new(initial: &str) -> Self {
            Self {
                contents: std::sync::Mutex::new(Some(initial.to_string())),
            }
        }

        fn plan(&self) -> ClipboardPlan {
            let guard = self.contents.lock().unwrap();
            match guard.as_deref() {
                Some(s) => ClipboardPlan::save(s),
                None => ClipboardPlan {
                    saved_text: None,
                    had_non_text: true,
                },
            }
        }

        fn restore(&self, plan: &ClipboardPlan) {
            let mut guard = self.contents.lock().unwrap();
            *guard = plan.restore_value().map(|v| v.to_string());
        }

        fn set(&self, value: &str) {
            let mut guard = self.contents.lock().unwrap();
            *guard = Some(value.to_string());
        }

        fn get(&self) -> Option<String> {
            self.contents.lock().unwrap().clone()
        }
    }

    /// Fake that models the clipboard-preservation pattern: capture_selection
    /// saves the original clipboard first, synthesizes the copy, reads the
    /// selection, then restores the original clipboard before returning.
    struct ClipboardAwareFakeTextIo {
        selection: String,
        clipboard: SimulatedClipboard,
        replaced: std::sync::Mutex<Option<String>>,
    }

    #[async_trait]
    impl TextIo for ClipboardAwareFakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            let plan = self.clipboard.plan();
            // Simulate Cmd+C overwriting clipboard with selection.
            self.clipboard.set(&self.selection);
            let text = self.clipboard.get().unwrap();
            // Restore original clipboard immediately.
            self.clipboard.restore(&plan);
            Ok(text)
        }

        async fn replace(&self, text: &str) -> Result<(), TextIoError> {
            // paste_via_clipboard_sync pattern: save → set → paste → restore.
            let plan = self.clipboard.plan();
            self.clipboard.set(text);
            *self.replaced.lock().unwrap() = Some(text.to_string());
            self.clipboard.restore(&plan);
            Ok(())
        }
    }

    #[test]
    fn restore_clipboard_plan_saves_and_restores() {
        let saved = "original";
        let plan = ClipboardPlan::save(saved);
        assert_eq!(plan.restore_value(), Some(saved));
        assert!(!plan.had_non_text());
    }

    #[test]
    fn clipboard_plan_non_text_is_preserved_flag() {
        let plan = ClipboardPlan {
            saved_text: None,
            had_non_text: true,
        };
        assert!(plan.had_non_text());
        assert_eq!(plan.restore_value(), None);
    }

    #[tokio::test]
    async fn clipboard_preserved_after_capture_selection() {
        let user_clipboard = "user-copied-data";
        let fake = ClipboardAwareFakeTextIo {
            selection: "selected text".into(),
            clipboard: SimulatedClipboard::new(user_clipboard),
            replaced: std::sync::Mutex::new(None),
        };

        let captured = fake.capture_selection().await.unwrap();
        assert_eq!(captured, "selected text");
        // Clipboard must be restored to user's original data after capture.
        assert_eq!(
            fake.clipboard.get().as_deref(),
            Some(user_clipboard)
        );
    }

    #[tokio::test]
    async fn clipboard_preserved_through_capture_and_paste_full_flow() {
        let user_clipboard = "user-copied-data";
        let fake = ClipboardAwareFakeTextIo {
            selection: "original selection".into(),
            clipboard: SimulatedClipboard::new(user_clipboard),
            replaced: std::sync::Mutex::new(None),
        };

        // capture_selection saves original, synthesizes copy, restores.
        let captured = fake.capture_selection().await.unwrap();
        assert_eq!(captured, "original selection");
        assert_eq!(
            fake.clipboard.get().as_deref(),
            Some(user_clipboard)
        );

        // paste_via_clipboard_sync: saves current clipboard (=user's original
        // since capture already restored it), sets rewritten text, pastes,
        // restores user's original. This is the bug fix: without
        // capture_selection restoring, paste would save the SELECTION, not the
        // user's data, and restore the wrong thing.
        fake.replace("rewritten text").await.unwrap();
        assert_eq!(
            fake.clipboard.get().as_deref(),
            Some(user_clipboard)
        );
        assert_eq!(
            fake.replaced.lock().unwrap().as_deref(),
            Some("rewritten text")
        );
    }

    #[tokio::test]
    async fn fake_textio_roundtrip() {
        let fake = FakeTextIo {
            selection: "hello".into(),
            replaced: std::sync::Mutex::new(None),
            last_mode: std::sync::Mutex::new(None),
        };
        let captured = fake.capture_selection().await.unwrap();
        assert_eq!(captured, "hello");
        fake.replace("world").await.unwrap();
        assert_eq!(fake.replaced.lock().unwrap().as_deref(), Some("world"));
        assert_eq!(
            *fake.last_mode.lock().unwrap(),
            Some(ReplaceMode::ClipboardPaste)
        );
    }

    #[tokio::test]
    async fn fake_textio_replace_with_mode() {
        let fake = FakeTextIo {
            selection: String::new(),
            replaced: std::sync::Mutex::new(None),
            last_mode: std::sync::Mutex::new(None),
        };
        fake.replace_with_mode("x", ReplaceMode::Accessibility)
            .await
            .unwrap();
        assert_eq!(fake.replaced.lock().unwrap().as_deref(), Some("x"));
        assert_eq!(
            *fake.last_mode.lock().unwrap(),
            Some(ReplaceMode::Accessibility)
        );
    }

    #[tokio::test]
    async fn fake_textio_inserts_at_cursor() {
        let fake = FakeTextIo {
            selection: String::new(),
            replaced: std::sync::Mutex::new(None),
            last_mode: std::sync::Mutex::new(None),
        };
        fake.insert_at_cursor("dictated").await.unwrap();
        assert_eq!(
            fake.replaced.lock().unwrap().as_deref(),
            Some("dictated")
        );
    }
}
