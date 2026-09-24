# Tasks

## 1. Action registry foundation

- [x] 1.1 Add `crates/core/src/assistant/action.rs` defining `ArgSpec` (name, type, required) and `ActionSpec` (stable id, title, description for the router, args); verify with unit tests that ids are unique across the catalog and that an entry without an argument schema cannot be constructed
- [x] 1.2 Implement `validate_args(&ActionSpec, &serde_json::Value) -> Result<ResolvedArgs, ArgError>` as a pure function distinguishing missing-required from type-mismatch; verify with unit tests covering both errors, an unknown-argument case, and a valid payload
- [x] 1.3 Add `ActionRegistry` with `get(id)` and `list()` over a static catalog, mirroring the shape of `crates/features/src/registry.rs`; verify a unit test that `get` on an id outside the catalog returns `None`
- [x] 1.4 Add `spoken_arg_names(&ActionSpec) -> &[&str]` marking which arguments are drawn from speech (the two-pass trigger from design.md); verify a unit test that an action with no spoken args returns empty, which is the case that skips the offline re-decode

## 2. Routing

- [x] 2.1 Add `crates/core/src/assistant/routing.rs` with `build_routing_prompt(&ActionRegistry, utterance, history) -> String` emitting the registry as a JSON action list, the answer-directly instruction, and the brevity constraint on answers; verify a unit test that every registry id appears in the prompt and that the prompt states both the answer default and the length limit
- [x] 2.2 Implement `parse_routing_response(&str, &ActionRegistry) -> RoutingOutcome` returning `Answer{text}` (the default outcome), `Action{spec, raw_args}`, `Ambiguous{candidates}`, or `Unparseable`; verify unit tests for well-formed JSON, JSON naming an id outside the registry (must be `Unparseable`/rejected, never invoked), prose with no JSON, and JSON wrapped in markdown fences
- [x] 2.3 Wire routing through `LlmEngine::complete` using the existing `SlotResolver` for the assistant's LLM slot; verify with a fake `LlmEngine` in tests that a malformed completion invokes nothing and surfaces an error rather than defaulting to an action
- [x] 2.4 Implement the ambiguity and missing-argument paths as clarification prompts back to the session rather than action invocation; verify unit tests that neither path reaches the dispatcher
- [x] 2.5 Verify with unit tests that a question about what an action would do is answered rather than invoking that action, and that answering is reported as success rather than a routing failure

## 3. Dispatch and the no-side-effects rule

- [x] 3.1 Implement `Dispatcher::invoke` running a validated action and returning its result or a typed failure; verify unit tests for a succeeding stub, a failing stub, and an unknown id
- [x] 3.2 Add a `has_side_effects: bool` marker to `ActionSpec` defaulting to false, and assert in a test that every entry in the shipped catalog is false — so adding an action with side effects fails the build rather than landing silently
- [x] 3.3 Record each invocation through `ActionRepo`/`ActionGuard` (`crates/features/src/feature.rs`) with the action id as `command`; verify tests that a completed run writes `Ok`, a failure writes `Error`, and a session cancelled mid-action writes `Cancelled` rather than `Error`

## 4. The action catalog

- [x] 4.1 Implement `read_focused` reading the frontmost app via `TextIo::capture_selection` with an OCR fallback through `ScreenCapture` + `TextRecognizer`; verify with fakes that an empty selection falls through to OCR and that a failure of both reports "target unavailable" rather than completing
- [x] 4.2 Implement `open_app` over `NSWorkspace` app launch behind the existing platform abstraction pattern, with a stub impl for non-macOS; verify it resolves a bundle id and that the stub returns an unavailable error rather than panicking
- [x] 4.3 Implement `start_meeting` delegating to `run_meeting_start`; verify a test that it reports the already-recording case as unavailable instead of starting a second meeting
- [x] 4.4 Implement `rewrite_focused` reusing `run_rewrite` rather than duplicating the rewrite pipeline; verify a test that it produces text for the session surface and does not write back to the focused app in this change
- [x] 4.5 Add the disclosure payload for actions that read another application and send it to a provider, naming what was read; verify a test that `read_focused` produces it and `open_app` does not

## 5. Assistant feature and activation

- [x] 5.1 Add `crates/features/src/assistant.rs` with `AssistantFeature` implementing `Feature` — `id()`, `required_caps()` declaring `CapKind::Llm`, `CapKind::Stt`, and `CapKind::Tts`, and `commands()` with one command using `platform_accelerator` on a key no existing command uses; export it from `crates/features/src/lib.rs` and verify `cargo test -p kea-features` passes
- [x] 5.2 Add the assistant row to `HOTKEY_ACTIONS` in `src-tauri/src/commands.rs` (the single table its doc comment names as the place hotkeys are added) and extend the array length; verify the existing hotkey collision and effective-lookup tests still pass
- [x] 5.3 Add the assistant entry to the dispatch table in `src-tauri/src/hotkeys.rs` with its own busy flag, so a second press while a session is open does not start a second session; verify a test that the second press is ignored
- [x] 5.4 Register `AssistantFeature` in the app's `FeatureRegistry` at startup and verify the feature id appears in `list_ids()`

## 6. Session orchestration

- [x] 6.1 Implement session state (`Listening`, `Processing`, `Presenting`, `Failed`) and its transitions as a pure state machine; verify unit tests for each transition including failure leaving the session open for retry
- [x] 6.2 Implement session capture: start the mic on activation, stream frames to the streaming engine for partials, and stop on endpoint — following the `start_dictation_run`/`stop_dictation_run` split in `commands.rs` rather than owning the device in the feature crate; verify with `ReplayAudioIo` that a canned buffer produces a completed request
- [x] 6.3 Implement silence endpointing over the existing energy-based `speech_mask` (`crates/platform/src/audio/segment.rs`) plus an explicit-submit path; verify unit tests that a sustained pause ends the request and that a session with no speech at all closes without calling the LLM
- [x] 6.4 Implement the streaming-absent path: when the streaming engine returns `ModelNotInstalled`, the session still captures, transcribes offline, and routes, showing no partial text; verify a test with a streaming engine stubbed to `ModelNotInstalled` that the request still completes
- [x] 6.5 Implement the two-pass rule: route from the streaming hypothesis, and re-decode the buffered audio with the offline engine only when the selected action has spoken args, taking those values from the offline transcript; verify a test that an action with no spoken args never invokes the offline engine and one with spoken args takes its values from it
- [x] 6.6 Implement cancellation at each state, discarding captured audio and preventing any not-yet-started action from running; verify unit tests for cancel-while-listening and cancel-while-processing
- [x] 6.7 Implement follow-up turns carrying prior turns as context, and discard all turns on close; verify tests that a follow-up sees earlier turns and that a fresh session sees none
- [x] 6.8 Speak the answer through `run_tts_with_player` (`crates/features/src/tts.rs`) using the resolved TTS slot and the existing voice/speed settings; verify with a fake TTS engine that the spoken text is exactly the displayed text
- [x] 6.9 Implement stop-playback, and make session cancellation and the start of a follow-up both stop it; verify unit tests for each of the three stop paths
- [x] 6.10 Implement the speech-unavailable path: with no usable TTS engine the answer is still displayed and the failure is reported; verify a test with the TTS slot unresolvable that the session still completes

## 7. UI surface

- [ ] 7.1 Add the assistant window in `src-tauri/src/overlay.rs` or a sibling module, keyed on its own label the way the dictation HUD is, and ensure it does not take keyboard focus; verify by running `make dev` that the frontmost app keeps focus when the window appears
- [x] 7.2 Add assistant events to `src-tauri/src/events.rs` (state, partial text, result, error) following the existing `DictationPartialPayload` shape; verify the payloads serialize in Rust unit tests
- [x] 7.3 Add the listeners to `ui/src/api.ts` alongside `onDictationState`/`onDictationLevel`; verify `ui/src/api.test.ts` covers each new listener
- [x] 7.4 Build `ui/src/components/AssistantHud.tsx` (named for the `DictationHud.tsx` it sits beside in the overlay, not the `AssistantPanel.tsx` this task first said) rendering the four states, live partial text, the answer with a copy control, a stop-speaking control while playback runs, and a cancel control; verify a colocated `.test.tsx` covers each state, matching the per-component test convention in `ui/src/`
- [x] 7.5 Render the request text and the selected action before or alongside acting (the intent-routing visibility requirement); verify a component test asserts both are present when an action is invoked
- [x] 7.6 Honour the speak and show switches in the surface: with showing off the panel presents state and controls but no answer text, and with speaking off no playback control appears; verify component tests for all four combinations of the two switches

## 8. Settings

- [x] 8.1 Add assistant settings (LLM binding, TTS binding, `speak_answers`, `show_answers`) using the key-value `settings` pattern in `crates/core/src/dictation/settings.rs`; both output switches default **on**, so verify round-trip tests including that an absent row reads as enabled for each
- [x] 8.2 Add `ui/src/pages/AssistantPage.tsx` with its hotkey binding picker, AI picker, voice picker, and the speak/show switches, following the existing page conventions; verify a colocated `AssistantPage.test.tsx` as every other page has
- [x] 8.3 Confirm no database migration is required — the `actions` ledger and the `settings` table already carry everything this change persists; verify `make test` passes with no new file under `crates/core/migrations/data/`

## 9. Instrumentation and verification

- [ ] 9.1 Add a timing span around the offline decode in the dictation and assistant paths (only insertion is timed today, at `crates/features/src/dictation.rs:487`); verify the elapsed value appears in the log for one real dictation, answering design.md's open question on re-decode cost
- [x] 9.2 Add an end-to-end test driving activation through routing to a spoken answer with fake engines; verify it asserts the answer was both displayed and sent to the TTS engine, and that no action was invoked
- [x] 9.3 Add an end-to-end test driving activation through routing to a completed action with fake engines; verify it asserts the `actions` ledger row and the presented result
- [x] 9.4 Add an end-to-end test for the refusal path: a malformed routing response invokes no action, reports the failure, and leaves the session open
- [x] 9.5 Run `make lint` and `make test` and verify both pass
- [ ] 9.6 Run `make install` and exercise the assistant against a real app: ask a plain question and confirm the answer is short, spoken, and shown; then run each catalog action, verifying the frontmost application is correctly resolved for `read_focused` and `rewrite_focused`
- [ ] 9.7 Verify the assistant's hotkey does not collide with dictation, rewrite, TTS, meetings, palette, OCR, or undo, by pressing each in turn after `make install`
