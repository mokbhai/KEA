//! Action handlers whose dependencies are platform traits.
//!
//! The two here need nothing from the application's state, so they live in this
//! crate and are constructed from a platform trait object. The handlers that do
//! need state — starting a meeting, rewriting a selection — are built where that
//! state lives, which is what [`ActionHandler`] being a trait buys.

use std::sync::Arc;

use async_trait::async_trait;
use kea_core::assistant::dispatch::{ActionFailure, ActionHandler, ActionOutcome, Disclosure};
use kea_core::assistant::ResolvedArgs;
use kea_platform::apps::{AppLaunchError, AppLauncher};
use kea_platform::screen::ScreenReader;
use kea_platform::textio::TextIo;

/// Read what the user is looking at.
///
/// **Three sources, tried in order, and the order is a statement about both
/// intent and cost.**
///
/// 1. The selection — the user saying *this part*. It wins whenever it exists,
///    because highlighting something is an explicit narrowing of the question.
/// 2. The focused element's whole text, over Accessibility. The answer to "what
///    does this say" asked with nothing highlighted. Free, and exact.
/// 3. A screenshot of the focused window, OCR'd. Accessibility refuses on most
///    web and Electron surfaces, which is a large share of what people look at,
///    so without this tier the action fails in browsers, Slack and editors.
///
/// Tier three is last on purpose. It is the most invasive thing KEA does — a
/// screenshot of a window the user did not point at, not initiated by them —
/// so it runs only once the free and exact reads have come back with nothing,
/// and the disclosure says which tier answered.
pub struct ReadFocused {
    textio: Arc<dyn TextIo>,
    screen: Arc<dyn ScreenReader>,
}

impl ReadFocused {
    pub fn new(textio: Arc<dyn TextIo>, screen: Arc<dyn ScreenReader>) -> Self {
        Self { textio, screen }
    }
}

#[async_trait]
impl ActionHandler for ReadFocused {
    fn id(&self) -> &'static str {
        "read_focused"
    }

    async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
        // A selection that is present but blank is not a selection. Apps
        // report whitespace for "nothing highlighted" often enough that
        // treating it as content would answer questions about an empty string.
        let selection = self
            .textio
            .capture_selection()
            .await
            .ok()
            .filter(|s| !s.trim().is_empty());

        if let Some(text) = selection {
            return Ok(ActionOutcome {
                text: Some(text),
                disclosure: Some(Disclosure {
                    read: "your selected text".into(),
                    sent_externally: true,
                }),
            });
        }

        if let Ok(text) = self.textio.capture_focused_text().await {
            if !text.trim().is_empty() {
                return Ok(ActionOutcome {
                    text: Some(text),
                    disclosure: Some(Disclosure {
                        read: "the text in the window you're looking at".into(),
                        sent_externally: true,
                    }),
                });
            }
        }

        match self.screen.read_focused_window().await {
            Ok(text) if !text.trim().is_empty() => Ok(ActionOutcome {
                text: Some(text),
                // Named differently from the tiers above because it is a
                // different thing to have happened to you: the user is told a
                // picture of their window was taken, not merely that text was
                // read.
                disclosure: Some(Disclosure {
                    read: "a screenshot of the window you're looking at".into(),
                    sent_externally: true,
                }),
            }),
            // Every source came back empty or refused. Reported rather than
            // answered from nothing: an assistant that invents an answer about
            // content it could not read is worse than one that says so.
            _ => Err(ActionFailure::Unavailable {
                what: "anything readable on screen".into(),
            }),
        }
    }
}

/// Bring an application to the front.
pub struct OpenApp {
    launcher: Arc<dyn AppLauncher>,
}

impl OpenApp {
    pub fn new(launcher: Arc<dyn AppLauncher>) -> Self {
        Self { launcher }
    }
}

#[async_trait]
impl ActionHandler for OpenApp {
    fn id(&self) -> &'static str {
        "open_app"
    }

    async fn run(&self, args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
        // `validate_args` has already refused a missing required argument, so
        // reaching here without one is a wiring fault, not user input.
        let name = args.text("app").ok_or_else(|| {
            ActionFailure::Failed("no application name reached the handler".into())
        })?;

        match self.launcher.open(name) {
            // No disclosure: nothing was read and nothing was sent.
            Ok(()) => Ok(ActionOutcome {
                text: Some(format!("Opened {name}.")),
                disclosure: None,
            }),
            Err(AppLaunchError::NotFound { name }) => Err(ActionFailure::Unavailable {
                what: format!("an application named {name}"),
            }),
            Err(e @ AppLaunchError::Unsupported) => Err(ActionFailure::Failed(e.to_string())),
            Err(AppLaunchError::Failed { reason, .. }) => Err(ActionFailure::Failed(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_platform::textio::{ReplaceMode, TextIoError};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTextIo {
        selection: Option<Result<String, String>>,
        focused: Option<Result<String, String>>,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            match &self.selection {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(TextIoError::Other(e.clone())),
                None => Err(TextIoError::Other("no selection".into())),
            }
        }
        async fn replace_with_mode(&self, _t: &str, _m: ReplaceMode) -> Result<(), TextIoError> {
            Ok(())
        }
        async fn capture_focused_text(&self) -> Result<String, TextIoError> {
            match &self.focused {
                Some(Ok(s)) => Ok(s.clone()),
                Some(Err(e)) => Err(TextIoError::Other(e.clone())),
                None => Err(TextIoError::Other("unreadable".into())),
            }
        }
    }

    #[derive(Default)]
    struct FakeScreen {
        text: Option<String>,
    }

    #[async_trait]
    impl ScreenReader for FakeScreen {
        async fn read_focused_window(&self) -> Result<String, kea_platform::screen::ScreenError> {
            match &self.text {
                Some(t) => Ok(t.clone()),
                None => Err(kea_platform::screen::ScreenError::Unavailable(
                    "no screen recording permission".into(),
                )),
            }
        }
    }

    fn no_screen() -> Arc<dyn ScreenReader> {
        Arc::new(FakeScreen::default())
    }

    #[derive(Default)]
    struct FakeLauncher {
        fail: Option<AppLaunchError>,
        opened: Mutex<Vec<String>>,
    }

    impl AppLauncher for FakeLauncher {
        fn open(&self, name: &str) -> Result<(), AppLaunchError> {
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            self.opened.lock().unwrap().push(name.to_string());
            Ok(())
        }
    }

    fn args_with_app(name: &str) -> ResolvedArgs {
        use kea_core::assistant::ArgValue;
        let mut a = ResolvedArgs::default();
        a.replace("app", ArgValue::Text(name.into()));
        a
    }

    #[tokio::test]
    async fn a_selection_wins_over_the_whole_window() {
        let io = FakeTextIo {
            selection: Some(Ok("just this bit".into())),
            focused: Some(Ok("the entire document".into())),
        };
        let out = ReadFocused::new(Arc::new(io), no_screen())
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("just this bit"));
    }

    #[tokio::test]
    async fn an_empty_selection_falls_through_to_the_window() {
        let io = FakeTextIo {
            selection: Some(Ok("   ".into())),
            focused: Some(Ok("the entire document".into())),
        };
        let out = ReadFocused::new(Arc::new(io), no_screen())
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("the entire document"));
    }

    #[tokio::test]
    async fn a_failed_selection_falls_through_to_the_window() {
        let io = FakeTextIo {
            selection: Some(Err("no accessibility".into())),
            focused: Some(Ok("the entire document".into())),
        };
        let out = ReadFocused::new(Arc::new(io), no_screen())
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("the entire document"));
    }

    #[tokio::test]
    async fn reading_discloses_what_was_read_and_that_it_left_the_machine() {
        let io = FakeTextIo {
            selection: Some(Ok("secret".into())),
            focused: None,
        };
        let out = ReadFocused::new(Arc::new(io), no_screen())
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        let d = out.disclosure.expect("a read must disclose");
        assert!(d.read.contains("selected"));
        assert!(d.sent_externally);
    }

    #[tokio::test]
    async fn every_source_failing_reports_unavailable_rather_than_answering() {
        let io = FakeTextIo {
            selection: Some(Err("nope".into())),
            focused: Some(Err("nope".into())),
        };
        let err = ReadFocused::new(Arc::new(io), no_screen())
            .run(&ResolvedArgs::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ActionFailure::Unavailable { .. }));
    }

    #[tokio::test]
    async fn accessibility_refusing_falls_through_to_a_screenshot() {
        // The browser and Electron case: AX answers nothing, and without this
        // tier the assistant is useless in exactly those apps.
        let io = FakeTextIo {
            selection: Some(Err("no selection".into())),
            focused: Some(Err("unsupported element".into())),
        };
        let screen = Arc::new(FakeScreen {
            text: Some("404 Not Found".into()),
        });
        let out = ReadFocused::new(Arc::new(io), screen)
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("404 Not Found"));
    }

    #[tokio::test]
    async fn a_screenshot_read_says_it_was_a_screenshot() {
        let io = FakeTextIo {
            selection: None,
            focused: None,
        };
        let screen = Arc::new(FakeScreen {
            text: Some("some text".into()),
        });
        let out = ReadFocused::new(Arc::new(io), screen)
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        let d = out.disclosure.expect("a read must disclose");
        assert!(
            d.read.contains("screenshot"),
            "the user must be told a picture was taken, got {:?}",
            d.read
        );
    }

    #[tokio::test]
    async fn accessibility_answering_never_reaches_the_screenshot_tier() {
        // The invasive tier must not run when a free and exact read succeeded.
        let io = FakeTextIo {
            selection: Some(Err("no selection".into())),
            focused: Some(Ok("the document text".into())),
        };
        let screen = Arc::new(FakeScreen {
            text: Some("SHOULD NOT BE USED".into()),
        });
        let out = ReadFocused::new(Arc::new(io), screen)
            .run(&ResolvedArgs::default())
            .await
            .unwrap();
        assert_eq!(out.text.as_deref(), Some("the document text"));
        assert!(!out.disclosure.unwrap().read.contains("screenshot"));
    }

    #[tokio::test]
    async fn opening_an_app_passes_the_name_through() {
        let launcher = Arc::new(FakeLauncher::default());
        let out = OpenApp::new(launcher.clone())
            .run(&args_with_app("Mail"))
            .await
            .unwrap();
        assert_eq!(launcher.opened.lock().unwrap().as_slice(), ["Mail"]);
        assert!(out.text.unwrap().contains("Mail"));
    }

    #[tokio::test]
    async fn opening_an_app_reads_nothing_so_discloses_nothing() {
        let out = OpenApp::new(Arc::new(FakeLauncher::default()))
            .run(&args_with_app("Mail"))
            .await
            .unwrap();
        assert!(out.disclosure.is_none());
    }

    #[tokio::test]
    async fn an_unknown_app_is_unavailable_not_a_failure() {
        let launcher = FakeLauncher {
            fail: Some(AppLaunchError::NotFound {
                name: "Male".into(),
            }),
            ..Default::default()
        };
        let err = OpenApp::new(Arc::new(launcher))
            .run(&args_with_app("Male"))
            .await
            .unwrap_err();
        match err {
            ActionFailure::Unavailable { what } => assert!(what.contains("Male")),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unsupported_platform_fails_rather_than_pretending_to_open() {
        let launcher = FakeLauncher {
            fail: Some(AppLaunchError::Unsupported),
            ..Default::default()
        };
        let err = OpenApp::new(Arc::new(launcher))
            .run(&args_with_app("Mail"))
            .await
            .unwrap_err();
        assert!(matches!(err, ActionFailure::Failed(_)));
    }
}
