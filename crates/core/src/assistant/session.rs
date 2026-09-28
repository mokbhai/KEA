//! The assistant session: what state a request is in, and what may change it.
//!
//! Pure. No audio, no engines, no windows — those live in the app layer, which
//! drives this. The point of separating them is that the interesting rules are
//! all about *ordering* (a failure leaves the session open; speaking stops when
//! a follow-up starts; cancelling from any state ends it) and ordering rules
//! are the ones worth testing without a microphone attached.

use serde::Serialize;

/// What the session is doing, as the surface reports it.
///
/// Four states rather than a boolean "busy", because the spec's requirement is
/// that a slow request is not mistaken for a failure: the user must be able to
/// tell "still listening" from "working on it" from "here is the answer".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SessionState {
    /// Capturing the user's request.
    Listening,
    /// The request has ended; routing and any action are in flight.
    Processing,
    /// An answer is on screen. `speaking` tracks playback so the surface can
    /// offer to stop it.
    Presenting { speaking: bool },
    /// Something went wrong. The session stays open so the user can retry or
    /// rephrase without re-activating.
    Failed { message: String },
}

impl SessionState {
    /// Whether an answer is being read aloud right now.
    pub fn is_speaking(&self) -> bool {
        matches!(self, SessionState::Presenting { speaking: true })
    }
}

/// Everything that can move a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The user stopped talking, or submitted explicitly.
    RequestEnded,
    /// An answer is ready. `spoken` is whether playback was started.
    Answered { text: String, spoken: bool },
    /// Playback finished on its own.
    SpeechFinished,
    /// The user stopped playback, or something else did.
    SpeechStopped,
    /// The request could not be completed.
    Failed { message: String },
    /// The user began another turn.
    FollowUpStarted,
}

/// One open session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    state: SessionState,
    /// Completed turns, oldest first. Context for follow-ups, and discarded
    /// with the session — never persisted, never seen by a later session.
    turns: Vec<(String, String)>,
    /// The request being captured or processed right now.
    pending_request: Option<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A freshly activated session, listening.
    pub fn new() -> Self {
        Self {
            state: SessionState::Listening,
            turns: Vec::new(),
            pending_request: None,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Prior turns, for the routing prompt.
    pub fn history(&self) -> &[(String, String)] {
        &self.turns
    }

    /// Record what the user asked, once it is transcribed.
    pub fn set_request(&mut self, text: impl Into<String>) {
        self.pending_request = Some(text.into());
    }

    pub fn request(&self) -> Option<&str> {
        self.pending_request.as_deref()
    }

    /// Apply an event, returning the new state.
    ///
    /// Events that do not apply to the current state are ignored rather than
    /// rejected. Every one of them is a race the app layer cannot prevent — a
    /// playback-finished callback arriving after the user already cancelled, a
    /// second endpoint from a stopped stream — and treating a late message as
    /// an error would turn every such race into a visible fault.
    pub fn apply(&mut self, event: SessionEvent) -> &SessionState {
        self.state = match (&self.state, event) {
            (SessionState::Listening, SessionEvent::RequestEnded) => SessionState::Processing,

            // A failure may arrive in either working state: capture can fail
            // before the request ends.
            (
                SessionState::Listening | SessionState::Processing,
                SessionEvent::Failed { message },
            ) => SessionState::Failed { message },

            (SessionState::Processing, SessionEvent::Answered { text, spoken }) => {
                // The turn is only complete once it has an answer, which is
                // what keeps a failed turn out of a follow-up's context.
                let asked = self.pending_request.take().unwrap_or_default();
                self.turns.push((asked, text));
                SessionState::Presenting { speaking: spoken }
            }

            (
                SessionState::Presenting { .. },
                SessionEvent::SpeechFinished | SessionEvent::SpeechStopped,
            ) => SessionState::Presenting { speaking: false },

            // A follow-up from any state restarts capture. From `Presenting`
            // it also implies stopping playback, which the app layer does on
            // seeing the transition — talking over the user is the single
            // rudest thing this feature could do.
            (_, SessionEvent::FollowUpStarted) => SessionState::Listening,

            // Retrying after a failure: the session stayed open precisely so
            // this works without re-activating.
            (SessionState::Failed { .. }, SessionEvent::RequestEnded) => SessionState::Processing,

            (current, _) => current.clone(),
        };
        &self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(text: &str, spoken: bool) -> SessionEvent {
        SessionEvent::Answered {
            text: text.into(),
            spoken,
        }
    }

    #[test]
    fn a_new_session_is_listening() {
        assert_eq!(Session::new().state(), &SessionState::Listening);
    }

    #[test]
    fn the_request_ending_moves_to_processing() {
        let mut s = Session::new();
        assert_eq!(
            s.apply(SessionEvent::RequestEnded),
            &SessionState::Processing
        );
    }

    #[test]
    fn an_answer_presents_and_reports_whether_it_is_being_spoken() {
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        assert_eq!(
            s.apply(answered("Paris.", true)),
            &SessionState::Presenting { speaking: true }
        );
        assert!(s.state().is_speaking());
    }

    #[test]
    fn an_answer_that_is_not_spoken_still_presents() {
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        assert_eq!(
            s.apply(answered("Paris.", false)),
            &SessionState::Presenting { speaking: false }
        );
        assert!(!s.state().is_speaking());
    }

    #[test]
    fn a_failure_leaves_the_session_open_for_a_retry() {
        // The spec's requirement: the user rephrases without re-activating.
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        s.apply(SessionEvent::Failed {
            message: "provider unreachable".into(),
        });
        assert!(matches!(s.state(), SessionState::Failed { .. }));

        s.apply(SessionEvent::FollowUpStarted);
        assert_eq!(s.state(), &SessionState::Listening);
    }

    #[test]
    fn capture_can_fail_before_the_request_ends() {
        let mut s = Session::new();
        s.apply(SessionEvent::Failed {
            message: "microphone unavailable".into(),
        });
        assert!(matches!(s.state(), SessionState::Failed { .. }));
    }

    #[test]
    fn speech_finishing_leaves_the_answer_on_screen() {
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        s.apply(answered("Paris.", true));
        assert_eq!(
            s.apply(SessionEvent::SpeechFinished),
            &SessionState::Presenting { speaking: false }
        );
    }

    #[test]
    fn stopping_speech_leaves_the_answer_on_screen() {
        // Stopping playback must not clear the text: the user stopped the
        // voice, not the answer.
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        s.apply(answered("a long answer", true));
        assert_eq!(
            s.apply(SessionEvent::SpeechStopped),
            &SessionState::Presenting { speaking: false }
        );
    }

    #[test]
    fn a_follow_up_returns_to_listening_from_any_state() {
        for setup in [
            SessionState::Listening,
            SessionState::Processing,
            SessionState::Presenting { speaking: true },
        ] {
            let mut s = Session::new();
            s.state = setup.clone();
            assert_eq!(
                s.apply(SessionEvent::FollowUpStarted),
                &SessionState::Listening,
                "from {setup:?}"
            );
        }
    }

    #[test]
    fn a_completed_turn_becomes_context_for_the_next() {
        let mut s = Session::new();
        s.set_request("what is the capital of france");
        s.apply(SessionEvent::RequestEnded);
        s.apply(answered("Paris.", true));
        assert_eq!(
            s.history(),
            [(
                "what is the capital of france".to_string(),
                "Paris.".to_string()
            )]
        );
    }

    #[test]
    fn a_failed_turn_does_not_become_context() {
        // Feeding a question that was never answered back into the next
        // prompt invites the model to answer the old one instead.
        let mut s = Session::new();
        s.set_request("what is this");
        s.apply(SessionEvent::RequestEnded);
        s.apply(SessionEvent::Failed {
            message: "unreachable".into(),
        });
        assert!(s.history().is_empty());
    }

    #[test]
    fn a_late_event_is_ignored_rather_than_faulting() {
        // Playback-finished can arrive after the user has already started a
        // follow-up. That is a race the app layer cannot close, so it must not
        // read as an error.
        let mut s = Session::new();
        s.apply(SessionEvent::RequestEnded);
        s.apply(answered("x", true));
        s.apply(SessionEvent::FollowUpStarted);
        assert_eq!(
            s.apply(SessionEvent::SpeechFinished),
            &SessionState::Listening
        );
    }

    #[test]
    fn an_answer_arriving_while_listening_is_ignored() {
        let mut s = Session::new();
        assert_eq!(s.apply(answered("x", true)), &SessionState::Listening);
    }
}
