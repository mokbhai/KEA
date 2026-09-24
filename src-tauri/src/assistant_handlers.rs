//! The two action handlers that need the running application, not just a
//! platform trait.
//!
//! [`kea_features::assistant::handlers`] holds the rest of the catalog, and the
//! split is not arbitrary. `read_focused` and `open_app` are built from a
//! single platform trait object each, so they live in the feature crate where
//! they can be tested against a fake and where nothing about Tauri leaks in.
//! Starting a meeting and rewriting a selection cannot: a meeting needs the
//! capture gate, the meeting repo, the ledger, the poll tasks and the window
//! that shows it recording, and a rewrite needs the presets, the prompt
//! overrides, the app-profile table and the selection busy flag. All of that is
//! [`AppState`], and [`ActionHandler`] being a trait is precisely what lets
//! these two be assembled here instead of dragging `AppState` into a crate that
//! has no business knowing it exists.
//!
//! **Neither of these changes anything belonging to the user**, which is the
//! rule the whole registry is bounded by
//! (`openspec/changes/add-voice-assistant/design.md`). A meeting is an activity
//! the user can stop, and it writes only its own rows. The rewrite here reads
//! the selection and hands the result back to the assistant's own surface — it
//! does **not** put it in the user's document, and the sink it is given
//! ([`ReadOnlyTextIo`]) is what makes that true by construction rather than by
//! remembering not to.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use kea_core::assistant::dispatch::{
    ActionFailure, ActionHandler, ActionOutcome, Disclosure, Dispatcher,
};
use kea_core::assistant::ResolvedArgs;
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo, RewriteMode};
use kea_core::store::actions::ActionRepo;
use kea_core::store::bindings::BindingRepo;
use kea_features::ProfileOverrides;
use kea_platform::textio::{ReplaceMode, TextIo, TextIoError};
use tauri::AppHandle;

use crate::commands::{
    capture_app_context_now, profile_for, rewrite_input_for_profile, try_acquire_busy,
    RewriteOverride,
};
use crate::AppState;

/// Catalog ids these handlers answer to.
///
/// Consts rather than literals in [`ActionHandler::id`] so the test below can
/// assert they are ids the registry actually has. A handler registered under a
/// name no catalog entry uses is unreachable and silently so: the dispatcher
/// keys on the spec's id, finds nothing, and reports `NoHandler` for an action
/// that was installed all along.
const START_MEETING_ID: &str = "start_meeting";
const REWRITE_FOCUSED_ID: &str = "rewrite_focused";

/// Install the handlers that need application state.
///
/// **This is the one call the app layer adds.** It is deliberately the whole
/// surface of this module: `crate::assistant::build_dispatcher` should not have
/// to know how many handlers live here, or what each of them needs, because
/// every future state-backed action is another line in this function rather
/// than another line there.
///
/// It takes the `AppHandle` as well as the state because a meeting is not
/// merely started, it is *shown* — the segment poll that transcribes it, the
/// level poll that animates it and the state event that tells the window a
/// recording is running all emit through the handle. Calling
/// `kea_features::run_meeting_start` with `AppState` alone would compile and
/// would open the microphone, and the user would get a meeting that records
/// audio, transcribes nothing, and does not appear in the UI they would have to
/// use to stop it. See [`StartMeeting`].
pub fn register_state_handlers(d: &mut Dispatcher, state: &Arc<AppState>, app: &AppHandle) {
    d.register(Arc::new(StartMeeting::new(state.clone(), app.clone())));
    d.register(Arc::new(RewriteFocused::new(state.clone())));
}

// ===========================================================================
// start_meeting
// ===========================================================================

/// Begin a meeting recording.
///
/// Delegates to `commands::start_meeting_inner`, which is the app layer's one
/// meeting start: it runs the refusal gates, calls
/// [`kea_features::run_meeting_start`], parks the session where the stop path
/// looks for it, and spawns the polls. Reaching past it to `run_meeting_start`
/// directly — which is what having only `AppState` to hand would force — would
/// duplicate the gates and skip everything after them, so the assistant's
/// meetings would behave unlike every other meeting in the app. Going through
/// it means the hotkey, `kea://meeting`, the HTTP route and the assistant
/// cannot drift apart.
pub struct StartMeeting {
    state: Arc<AppState>,
    app: AppHandle,
}

impl StartMeeting {
    pub fn new(state: Arc<AppState>, app: AppHandle) -> Self {
        Self { state, app }
    }
}

#[async_trait]
impl ActionHandler for StartMeeting {
    fn id(&self) -> &'static str {
        START_MEETING_ID
    }

    async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
        match crate::commands::start_meeting_inner(&self.state, &self.app).await {
            // Short because it is spoken. The meeting id is not read out: it is
            // a timestamp, it means nothing aloud, and the user is looking at
            // the window that now says Recording.
            Ok(_meeting_id) => Ok(ActionOutcome {
                text: Some("Recording the meeting.".into()),
                // Nothing was read and nothing was sent. The meeting's own
                // transcription discloses itself by being visibly a recording.
                disclosure: None,
            }),
            Err(message) => Err(meeting_failure(&message)),
        }
    }
}

/// Split the meeting start's string errors into "come back later" and "it
/// broke".
///
/// `start_meeting_inner` answers with a `String`, as every `*_inner` does, and
/// its signature is shared with the hotkey, `kea://meeting` and the HTTP route.
/// Reshaping it into a typed error would be the better fix and is not available
/// from here, so the refusals are recognised by their text, in one place, with
/// a test per arm. `crate::api::exec::classify` makes the same split for the
/// same reason and against the same strings; the two are not shared because
/// they map onto different vocabularies — `ActionError` there, [`ActionFailure`]
/// here — and one error enum spanning both would serve neither.
///
/// The requirement being discharged is narrow and specific: a request that
/// could not run must not be reported as completed, and "already recording"
/// must say so rather than become a second meeting. The unrecognised case falls
/// to [`ActionFailure::Failed`] rather than `Unavailable`, which is the safe
/// side: calling a genuine fault "unavailable" invites a retry that fails
/// again, while calling a refusal a failure merely sounds blunter than it needs
/// to.
fn meeting_failure(message: &str) -> ActionFailure {
    let unavailable = |what: &str| ActionFailure::Unavailable {
        what: what.to_string(),
    };

    // Checked before "already": the message for a meeting still synthesising
    // its notes says "wait for it to complete", and it is a different situation
    // from one that is still recording — this one clears on its own.
    if message.contains("wait for it") {
        unavailable("a new recording until the last meeting has finished")
    } else if message.contains("already") {
        unavailable("a new recording while a meeting is already running")
    } else if message.contains("dictation is active") {
        unavailable("the microphone, which dictation is holding")
    } else {
        ActionFailure::Failed(message.to_string())
    }
}

// ===========================================================================
// rewrite_focused
// ===========================================================================

/// Rewrite what the user has selected, and hand the result to the assistant.
///
/// **The rewritten text is not written back into the user's document in this
/// change.** The assistant reaches this action through two probabilistic layers
/// — the transcript may be wrong and the router may be wrong — and the registry
/// is only safe because nothing behind it alters the user's work. A rewrite
/// that replaced a selection would be the first entry to break that rule, and
/// would break it in the one way the design says costs the user something they
/// cannot get back with a second sentence.
///
/// So the answer comes back as words, and the pipeline that produces it is the
/// real one: [`kea_features::run_rewrite`], with the same bindings, presets,
/// prompt overrides, app profile and ledger row a rewrite from the shortcut
/// gets. Writing a second, non-inserting pipeline here — as
/// `commands::preview_rewrite_inner` is for the settings window's try-it box —
/// was the alternative, and it was rejected: the assistant's rewrite would then
/// quietly stop matching the user's rewrite the first time either one changed,
/// and it would leave no ledger row at all.
///
/// What makes the delivery half not happen is [`ReadOnlyTextIo`], which is
/// handed to `run_rewrite` in place of the real app: it reads from the app and
/// absorbs every write. The property is then a fact about the sink rather than
/// a promise about the call site.
pub struct RewriteFocused {
    state: Arc<AppState>,
}

impl RewriteFocused {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ActionHandler for RewriteFocused {
    fn id(&self) -> &'static str {
        REWRITE_FOCUSED_ID
    }

    async fn run(&self, args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
        let state = &self.state;

        // Capturing a selection fires a synthetic ⌘C at whatever is frontmost,
        // and `selection_busy` is the one flag every path that does so shares —
        // the rewrite shortcut, the palette and the screen capture. Two of them
        // interleaved is a corrupted document, not a race that can be lost
        // gracefully, so this refuses rather than queues.
        let Some(_busy) = try_acquire_busy(&state.selection_busy) else {
            return Err(ActionFailure::Unavailable {
                what: "the selection, which a rewrite, the palette or a capture is already using"
                    .into(),
            });
        };

        // Probed before anything else that could change which app is frontmost,
        // which is the ordering `commands::run_selection_rewrite` owns and the
        // reason it exists: by the time a rewrite returns, the user may well
        // have switched away, and a profile resolved against the wrong app
        // rewrites with the wrong settings.
        let ctx = capture_app_context_now(state).await;
        let profile = profile_for(&state.config_pool, ctx.as_ref()).await;

        let textio = kea_platform::new_text_io();
        let selection = textio
            .capture_selection()
            .await
            .map_err(|e| ActionFailure::Failed(e.to_string()))?;

        // Read here rather than left to `run_rewrite`, which would happily send
        // an empty selection to a provider and spend a call saying nothing. A
        // selection that is present but blank is not a selection: apps report
        // whitespace for "nothing highlighted" often enough that treating it as
        // content would rewrite an empty string.
        if selection.trim().is_empty() {
            return Err(ActionFailure::Unavailable {
                what: "any selected text to rewrite".into(),
            });
        }

        let mut input = rewrite_input_for_profile(&state.config_pool, profile.as_ref()).await;
        style_request(args.text("style"))
            .into_override()
            .apply(&mut input, &state.config_pool)
            .await;
        // Set after the override is applied, because `RewriteOverride::apply`
        // re-derives the mode's parameter and must not see a half-built input.
        // Supplying it also stops `run_rewrite` re-capturing: a second ⌘C would
        // land while the assistant's own window is on screen.
        input.source_text = selection;

        let bindings = BindingRepo::new(state.config_pool.clone());
        let actions = ActionRepo::new(state.data_pool.clone());
        let presets = PresetRepo::new(state.config_pool.clone());
        let overrides = PromptOverrideRepo::new(state.config_pool.clone());
        let sink = ReadOnlyTextIo::new(textio);

        let text = kea_features::run_rewrite(
            &state.engines,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &sink,
            input,
            &ProfileOverrides::from_profile(profile.as_ref()),
        )
        .await
        .map_err(ActionFailure::Failed)?;

        // A rewrite that produced text without offering it to the sink means
        // `run_rewrite` grew a second delivery path — which would be the
        // assistant writing into the user's document without this module
        // knowing. Logged rather than failed: the answer in hand is still the
        // right one to speak, and this is a note for whoever changed it.
        if !sink.absorbed_a_write() {
            tracing::warn!(
                "assistant rewrite returned text without passing it through the read-only sink"
            );
        }

        Ok(ActionOutcome {
            text: Some(text),
            disclosure: Some(rewrite_disclosure()),
        })
    }
}

/// What the user is told a rewrite read, and where it went.
///
/// A function rather than a literal inside the handler so the requirement —
/// an action that reads another application and sends it to a provider must say
/// so — has something to assert against without a running app. `sent_externally`
/// is unconditionally true even though the user's engine may be a local one:
/// the disclosure describes the contract of the action, and an assistant that
/// said "this stayed on your machine" because of a setting it read a moment ago
/// would be making a promise it cannot keep.
fn rewrite_disclosure() -> Disclosure {
    Disclosure {
        read: "your selected text".into(),
        sent_externally: true,
    }
}

// ===========================================================================
// The spoken `style` argument
// ===========================================================================

/// What the spoken `style` argument turned out to be asking for.
///
/// Three outcomes rather than `Option<RewriteMode>`, because "no style was
/// said" and "a style was said that is not one of ours" are different requests
/// and collapsing them loses the second one entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StyleRequest {
    /// Nothing was said about style. The user's own rewrite settings — their
    /// active mode, their preset, their app profile — stand untouched, which is
    /// exactly what the rewrite shortcut would have done.
    Settings,
    /// A named style the app already has a mode for.
    Mode(RewriteMode),
    /// Anything else, handed to Ask KEA verbatim.
    Instruction(String),
}

impl StyleRequest {
    /// The override this request applies on top of the saved settings.
    fn into_override(self) -> RewriteOverride {
        match self {
            StyleRequest::Settings => RewriteOverride::default(),
            StyleRequest::Mode(mode) => RewriteOverride {
                mode: Some(mode),
                ..Default::default()
            },
            StyleRequest::Instruction(instruction) => RewriteOverride {
                mode: Some(RewriteMode::AskKea),
                instruction: Some(instruction),
                ..Default::default()
            },
        }
    }
}

/// Read the spoken style into a request.
///
/// **An unrecognised style is not an error.** The obvious implementation maps
/// the word onto a [`RewriteMode`] and refuses when it does not fit, and that
/// implementation fails on most of what people actually say — "rewrite this
/// like a lawyer", "in Spanish", "less formal". Ask KEA exists precisely to
/// take a free-form instruction, so the unmatched case routes there and works,
/// instead of the assistant answering that it does not know that style.
///
/// Translate deliberately has no arm here. Its parameter is a BCP-47 tag, not a
/// language name, so "in Spanish" would need a name-to-tag table that would be
/// wrong for exactly the languages a table like that is always wrong for.
/// Ask KEA translates on the instruction alone, which is worse than Translate's
/// template and much better than a mistagged one. Audio refinement has no arm
/// for a different reason: it is dictation's cleanup pass, not a style anyone
/// asks for by name.
fn style_request(style: Option<&str>) -> StyleRequest {
    let Some(raw) = style.map(str::trim).filter(|s| !s.is_empty()) else {
        return StyleRequest::Settings;
    };

    match mode_for_style(&normalize_style(raw)) {
        Some(mode) => StyleRequest::Mode(mode),
        // The user's own words, not the normalized form: this is going into a
        // prompt, and "a bit more like a pirate" is a better instruction than
        // "like a pirate".
        None => StyleRequest::Instruction(raw.to_string()),
    }
}

/// Reduce a spoken style to the form the lookup table is written in.
///
/// The qualifiers stripped here are the ones that intensify a style without
/// changing which one it is. Negations and comparatives that *invert* it —
/// "less formal", "not so casual" — are deliberately absent: stripping "less "
/// would turn a request into its own opposite, which is the one mistake this
/// function could make that the user cannot see coming. Left unstripped they
/// fall through to Ask KEA and are honoured as written.
fn normalize_style(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut s = lowered.trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());

    // Ordered longest-qualifier-first so "a bit more formal" sheds both.
    for qualifier in ["make it ", "a bit ", "a little ", "much ", "more "] {
        if let Some(rest) = s.strip_prefix(qualifier) {
            s = rest.trim_start();
        }
    }

    s.to_string()
}

/// The built-in mode a normalized style names, if any.
///
/// Adjectives and their adverbs both appear because both are said: the router
/// lifts "concisely" from "say it concisely" as readily as "concise" from "make
/// it concise", and only one of those is a word anybody would think to put in a
/// table.
fn mode_for_style(normalized: &str) -> Option<RewriteMode> {
    match normalized {
        "concise" | "concisely" | "short" | "shorter" | "brief" | "briefer" | "briefly"
        | "succinct" | "terse" | "tighter" | "to the point" => Some(RewriteMode::Concise),

        "professional" | "professionally" | "formal" | "formally" | "business" | "businesslike" => {
            Some(RewriteMode::Professional)
        }

        "friendly" | "casual" | "casually" | "informal" | "informally" | "warm" | "warmer"
        | "relaxed" => Some(RewriteMode::Friendly),

        "grammar"
        | "fix grammar"
        | "fix the grammar"
        | "correct the grammar"
        | "grammatical"
        | "proofread"
        | "proofreading"
        | "spelling" => Some(RewriteMode::FixGrammar),

        "improve" | "improved" | "better" | "polish" | "polished" | "tidy" | "clean up"
        | "cleaner" => Some(RewriteMode::Improve),

        _ => None,
    }
}

// ===========================================================================
// The sink that cannot write
// ===========================================================================

/// A [`TextIo`] that reads the user's application and refuses to write to it.
///
/// This is how "the assistant's rewrite does not touch your document" stops
/// being a rule someone has to remember. [`kea_features::run_rewrite`] ends by
/// putting its result back through the `TextIo` it was given; given this one,
/// the last step is a no-op and the text reaches the caller as a return value
/// instead of the user's editor.
///
/// Only [`TextIo::replace_with_mode`] is overridden, and that is sufficient
/// rather than lucky: `replace` and `insert_at_cursor` are both *defined* in
/// terms of it on the trait, and `swap_in_focused` defaults to refusing. Every
/// write therefore ends up here, and the test below exercises all three paths
/// so a future writing method added to the trait without an arm here is caught
/// by a failing assertion rather than by a user's document changing.
///
/// The alternative was to skip `run_rewrite` and call the non-inserting
/// `commands::preview_rewrite_inner` instead. It was rejected because that
/// function is the settings window's try-it box: it resolves the binding by
/// itself, takes no app profile and writes no ledger row, so the assistant's
/// rewrite would be a second rewrite rather than the same one delivered
/// differently.
pub struct ReadOnlyTextIo {
    inner: Box<dyn TextIo>,
    /// Whether anything asked this sink to write.
    ///
    /// It exists for the test, and it earns its place there: "the real app was
    /// never written to" passes just as happily when nothing tried to write at
    /// all, so without this the strongest assertion available would be a
    /// vacuous one.
    absorbed: AtomicBool,
}

impl ReadOnlyTextIo {
    pub fn new(inner: Box<dyn TextIo>) -> Self {
        Self {
            inner,
            absorbed: AtomicBool::new(false),
        }
    }

    /// Whether a write was offered to this sink and swallowed.
    pub fn absorbed_a_write(&self) -> bool {
        self.absorbed.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl TextIo for ReadOnlyTextIo {
    async fn capture_selection(&self) -> Result<String, TextIoError> {
        self.inner.capture_selection().await
    }

    async fn replace_with_mode(&self, _text: &str, _mode: ReplaceMode) -> Result<(), TextIoError> {
        self.absorbed.store(true, Ordering::Relaxed);
        // `Ok` rather than an error, because nothing went wrong: the rewrite
        // succeeded and its result is being returned instead of inserted.
        // Failing here would close the ledger row as an error and tell the user
        // a rewrite they are about to hear read aloud did not happen.
        Ok(())
    }

    async fn capture_focused_text(&self) -> Result<String, TextIoError> {
        self.inner.capture_focused_text().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_core::assistant::ActionRegistry;
    use std::sync::Mutex;

    // -- what this module installs -----------------------------------------

    #[test]
    fn both_handlers_answer_to_ids_the_catalog_actually_has() {
        // A handler registered under a name no entry uses is unreachable, and
        // silently: the dispatcher looks the spec's id up, finds nothing, and
        // reports the action as having no handler at all.
        let registry = ActionRegistry::default();
        assert!(registry.get(START_MEETING_ID).is_some());
        assert!(registry.get(REWRITE_FOCUSED_ID).is_some());
    }

    #[test]
    fn neither_handler_is_installed_for_an_action_with_side_effects() {
        // The registry's own test asserts this over the whole catalog; asserted
        // again from this side because this module is where the two entries
        // that touch the most application state are actually implemented.
        let registry = ActionRegistry::default();
        for id in [START_MEETING_ID, REWRITE_FOCUSED_ID] {
            assert!(!registry.get(id).unwrap().has_side_effects, "{id}");
        }
    }

    // -- start_meeting refusals --------------------------------------------

    #[test]
    fn a_meeting_already_recording_is_unavailable_rather_than_a_failure() {
        // The requirement this handler exists to satisfy: the second request
        // says a meeting is already running instead of starting another one.
        // That nothing starts is `start_meeting_inner`'s gate; that the user is
        // told the right thing is this mapping.
        let failure = meeting_failure("a meeting is already recording");
        match failure {
            ActionFailure::Unavailable { what } => assert!(what.contains("already running")),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn the_audio_layers_own_already_active_refusal_is_also_unavailable() {
        // There are two gates for one situation — the session slot and the
        // capture device — and they word it differently. Whichever wins the
        // race, the user hears the same thing.
        let failure = meeting_failure("meeting capture is already active");
        assert!(matches!(failure, ActionFailure::Unavailable { .. }));
    }

    #[test]
    fn a_meeting_still_finishing_is_reported_as_temporary_not_as_a_conflict() {
        // This one clears on its own, so it is worth distinguishing: telling
        // the user a meeting is running when it is in fact synthesising notes
        // sends them looking for a recording to stop.
        match meeting_failure("a meeting is finishing; wait for it to complete") {
            ActionFailure::Unavailable { what } => assert!(what.contains("has finished")),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn dictation_holding_the_microphone_names_the_microphone() {
        match meeting_failure("dictation is active; stop dictation before starting a meeting") {
            ActionFailure::Unavailable { what } => assert!(what.contains("microphone")),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn an_unrecognized_meeting_error_is_a_failure_so_nothing_reports_it_as_done() {
        // The spec's rule is that an action which could not run is never
        // reported as completed. Both arms satisfy it; the point of the
        // fallback being `Failed` is that a real fault must not read as
        // "try again in a moment".
        let failure = meeting_failure("no stt engine 'whisper'");
        assert!(matches!(failure, ActionFailure::Failed(_)));
    }

    #[test]
    fn no_meeting_error_is_ever_swallowed() {
        // Every message reaches the user as one failure or the other; there is
        // no path through the classifier that loses one.
        for message in [
            "a meeting is already recording",
            "a meeting is finishing; wait for it to complete",
            "dictation is active; stop dictation before starting a meeting",
            "meeting capture is already active",
            "database is locked",
        ] {
            let failure = meeting_failure(message);
            assert!(
                !failure.to_string().is_empty(),
                "{message:?} produced nothing to say"
            );
        }
    }

    // -- the spoken style argument -----------------------------------------

    #[test]
    fn saying_nothing_about_style_leaves_the_users_own_settings_alone() {
        assert_eq!(style_request(None), StyleRequest::Settings);
        let over = StyleRequest::Settings.into_override();
        assert!(over.mode.is_none());
        assert!(over.preset_id.is_none());
        assert!(over.instruction.is_none());
    }

    #[test]
    fn a_style_that_is_only_whitespace_is_the_same_as_saying_nothing() {
        // The router emits `"style": ""` about as readily as it omits the key,
        // and an empty string must not become an Ask KEA instruction with no
        // instruction in it.
        assert_eq!(style_request(Some("   ")), StyleRequest::Settings);
        assert_eq!(style_request(Some("")), StyleRequest::Settings);
    }

    #[test]
    fn a_named_style_selects_the_mode_the_app_already_has_for_it() {
        for (spoken, expected) in [
            ("concise", RewriteMode::Concise),
            ("professional", RewriteMode::Professional),
            ("friendly", RewriteMode::Friendly),
            ("proofread", RewriteMode::FixGrammar),
            ("improve", RewriteMode::Improve),
        ] {
            assert_eq!(
                style_request(Some(spoken)),
                StyleRequest::Mode(expected),
                "{spoken}"
            );
        }
    }

    #[test]
    fn an_adverb_asks_for_the_same_mode_as_its_adjective() {
        // "say it concisely" and "make it concise" are one request, and the
        // router will produce whichever the user happened to say.
        assert_eq!(
            style_request(Some("concisely")),
            style_request(Some("concise"))
        );
        assert_eq!(
            style_request(Some("formally")),
            style_request(Some("formal"))
        );
    }

    #[test]
    fn an_intensifier_is_not_part_of_which_style_was_asked_for() {
        assert_eq!(
            style_request(Some("more concise")),
            StyleRequest::Mode(RewriteMode::Concise)
        );
        assert_eq!(
            style_request(Some("a bit more formal")),
            StyleRequest::Mode(RewriteMode::Professional)
        );
        assert_eq!(
            style_request(Some("make it friendly")),
            StyleRequest::Mode(RewriteMode::Friendly)
        );
    }

    #[test]
    fn a_negated_style_is_never_turned_into_its_opposite() {
        // The failure this guards against is the worst one available here:
        // stripping "less " off "less formal" would hand the user a formal
        // rewrite when they asked for the reverse, and nothing in the spoken
        // answer would reveal it. Ask KEA honours the phrase as said instead.
        assert_eq!(
            style_request(Some("less formal")),
            StyleRequest::Instruction("less formal".into())
        );
    }

    #[test]
    fn matching_ignores_case_and_the_punctuation_speech_leaves_behind() {
        assert_eq!(
            style_request(Some("Concise.")),
            StyleRequest::Mode(RewriteMode::Concise)
        );
        assert_eq!(
            style_request(Some("  PROFESSIONAL! ")),
            StyleRequest::Mode(RewriteMode::Professional)
        );
    }

    #[test]
    fn a_style_with_no_matching_mode_is_honoured_rather_than_refused() {
        // The whole reason the unmatched case is not an error: this is most of
        // what people actually say, and refusing it would make the action look
        // broken for every request outside a five-word vocabulary.
        assert_eq!(
            style_request(Some("like a pirate")),
            StyleRequest::Instruction("like a pirate".into())
        );
    }

    #[test]
    fn an_unmatched_style_keeps_the_users_own_words_for_the_prompt() {
        // Normalized text is for the lookup table. What goes into a prompt is
        // what the user said, qualifiers and all.
        assert_eq!(
            style_request(Some("A Bit More Like A Lawyer")),
            StyleRequest::Instruction("A Bit More Like A Lawyer".into())
        );
    }

    #[test]
    fn an_unmatched_style_runs_as_ask_kea_with_that_instruction() {
        let over = StyleRequest::Instruction("like a pirate".into()).into_override();
        assert_eq!(over.mode, Some(RewriteMode::AskKea));
        assert_eq!(over.instruction.as_deref(), Some("like a pirate"));
    }

    #[test]
    fn a_matched_style_names_a_mode_and_supplies_no_instruction() {
        // Ask KEA's instruction slot is the one thing that must not leak into a
        // mode that has its own template: `build_llm_request` would have two
        // sources for the same prompt.
        let over = StyleRequest::Mode(RewriteMode::Concise).into_override();
        assert_eq!(over.mode, Some(RewriteMode::Concise));
        assert!(over.instruction.is_none());
    }

    #[test]
    fn a_language_request_falls_to_ask_kea_rather_than_a_guessed_language_tag() {
        // Translate's parameter is a BCP-47 tag. Guessing one from a spoken
        // language name is how a request to translate into Farsi becomes a
        // request to translate into nothing at all.
        assert_eq!(
            style_request(Some("in Spanish")),
            StyleRequest::Instruction("in Spanish".into())
        );
    }

    // -- the disclosure ----------------------------------------------------

    #[test]
    fn rewriting_discloses_what_was_read_and_that_it_left_the_machine() {
        // The registry's rule for any action that reads another application and
        // sends it on: the user is told both halves.
        let d = rewrite_disclosure();
        assert!(d.read.contains("selected"));
        assert!(d.sent_externally);
    }

    // -- the read-only sink ------------------------------------------------

    /// Stands in for the user's application: reads answer, and any write it is
    /// asked for is recorded so a test can assert it never happened.
    struct SpyTextIo {
        selection: String,
        focused: String,
        writes: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl TextIo for SpyTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }
        async fn replace_with_mode(
            &self,
            text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            self.writes.lock().unwrap().push(text.to_string());
            Ok(())
        }
        async fn capture_focused_text(&self) -> Result<String, TextIoError> {
            Ok(self.focused.clone())
        }
    }

    fn spy() -> (ReadOnlyTextIo, Arc<Mutex<Vec<String>>>) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let inner = SpyTextIo {
            selection: "the original sentence".into(),
            focused: "the whole document".into(),
            writes: writes.clone(),
        };
        (ReadOnlyTextIo::new(Box::new(inner)), writes)
    }

    #[tokio::test]
    async fn the_sink_still_reads_the_real_application() {
        // Half the point: the rewrite must see the user's actual text. A sink
        // that refused reads too would be a sink that rewrites nothing.
        let (sink, _) = spy();
        assert_eq!(
            sink.capture_selection().await.unwrap(),
            "the original sentence"
        );
        assert_eq!(
            sink.capture_focused_text().await.unwrap(),
            "the whole document"
        );
    }

    #[tokio::test]
    async fn the_sink_absorbs_a_replacement_instead_of_writing_it_to_the_app() {
        let (sink, writes) = spy();
        sink.replace_with_mode("the rewritten sentence", ReplaceMode::ClipboardPaste)
            .await
            .unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "the assistant's rewrite must not reach the user's document"
        );
        assert!(
            sink.absorbed_a_write(),
            "the assertion above is only meaningful if a write was actually offered"
        );
    }

    #[tokio::test]
    async fn every_way_of_writing_through_the_trait_is_absorbed() {
        // `replace` and `insert_at_cursor` are defined on the trait in terms of
        // `replace_with_mode`, so overriding one is enough today. Exercising
        // all three is what makes a future writing method added to `TextIo`
        // without an arm here fail this test rather than a user's document.
        let (sink, writes) = spy();
        sink.replace("a").await.unwrap();
        sink.insert_at_cursor("b").await.unwrap();
        sink.replace_with_mode("c", ReplaceMode::Accessibility)
            .await
            .unwrap();
        assert!(writes.lock().unwrap().is_empty());
        assert!(sink.absorbed_a_write());
    }

    #[tokio::test]
    async fn the_sink_refuses_to_put_text_back_into_the_focused_element() {
        // Undo's path. It is not a replacement, so it would bypass the override
        // above entirely; the trait's default refusal is what covers it, and
        // this asserts that default has not been traded for a delegation.
        let (sink, writes) = spy();
        assert!(sink.swap_in_focused("new", "old").await.is_err());
        assert!(writes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_sink_nothing_wrote_to_says_so() {
        // The non-vacuity flag has to be able to report false, or it proves
        // nothing when it reports true.
        let (sink, _) = spy();
        let _ = sink.capture_selection().await;
        assert!(!sink.absorbed_a_write());
    }
}
