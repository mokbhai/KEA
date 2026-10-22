# Tasks

## 1. Core: storage and settings

- [x] 1.1 Add `SpeakerSource::Inferred` (`"inferred"`) with `as_str`/`from_str`/serde round-trip; verify the existing stored-spelling test is extended and passes
- [x] 1.2 Widen `NewSegment.speaker: Option<SpeakerChannel>` to `speaker_key: Option<String>` and update every caller; verify `cargo test -p kea-core -p kea-features` passes unchanged
- [x] 1.3 Add `MeetingRepo::ensure_participant(meeting_id, key, display_name)` (source `channel`, no-op if the row exists); verify with a store test that a second call does not overwrite a user name
- [x] 1.4 Add `MeetingRepo::set_inferred_speaker_name` that writes only where `source != 'user'`; verify a store test that it never replaces a user-typed name
- [x] 1.5 Make `set_speaker_name` and `set_inferred_speaker_name` rewrite exact-match action-item owners for that meeting and re-render the `meeting_notes.action_items` prose in the same transaction; verify a store test that another meeting's owners are untouched
- [x] 1.6 Add `MeetingRepo::remap_segment_speakers(meeting_id, mapping, new_names)` in one transaction (rewrite `speaker_key`, drop orphaned non-user rows, carry a user name onto a surviving key); verify store tests for split→merge and the user-name-carried cases
- [x] 1.7 Add `voice_separation: bool` (`#[serde(default)]`) to `MeetingSettings`; verify a payload without the field deserializes as `false`

## 2. Core: voice clustering (pure)

- [x] 2.1 Create `crates/core/src/meetings/voices.rs` with `VoiceClusters`: online nearest-centroid assignment, `JOIN_SIM`, 16-cluster cap, minimum of 2 windows per segment, `spk{n}` keys and `Person {n+1}` names; verify unit tests with synthetic embeddings (same voice later reuses a key, new voice opens one, too-few windows returns `None`, cap respected)
- [x] 2.2 Add the stop-time pass: average-linkage agglomerative clustering to `MERGE_SIM`, per-segment majority relabel, greedy overlap mapping onto old keys, first-appearance renumbering, per-segment-mean fallback above 6,000 windows; verify unit tests where a split cluster is merged and a mapped key survives renumbering
- [x] 2.3 Add the window slicer (1.5 s window / 0.75 s hop, skips windows under the speech floor) as a pure function over `&[f32]`; verify unit tests on synthesized buffers

## 3. Inference: speaker embeddings

- [x] 3.1 Add a `SpeakerEmbedder` trait and a sherpa-backed implementation over `SpeakerEmbeddingExtractor` in `crates/infer` behind the `sherpa` feature, loading `DIARIZATION_EMBEDDING_ID`, refusing a mismatched sample rate, and running on `spawn_blocking`; verify the missing-model error test and a `cargo build` without the `sherpa` feature
- [x] 3.2 Add a fake embedder for tests in `kea-features` that returns scripted vectors per call; verify it is used by the feature tests below

## 4. Feature: live separation in the meeting loop

- [x] 4.1 Thread `Option<&mut VoiceClusters>` and the embedder through `run_meeting_poll_segment` and `finalize_meeting`; embed the system half for `remote` segments (two-source) or the mixed signal (mic-only), skip `local`/`mixed`; verify a feature test where three scripted remote voices produce `spk0..spk2` and local stays `local`
- [x] 4.2 Create participant rows lazily on first cluster appearance and emit the key on `MeetingSegmentEvent`; verify the live event carries `spk1` and the speaker list gains `Person 2`
- [x] 4.3 In mic-only mode with voice separation active, do not seed "You"; verify a feature test that no segment is labelled `local`
- [x] 4.4 Make every embedder error degrade to the channel label without failing the poll; verify a feature test with a failing embedder keeps recording and labels `remote`
- [x] 4.5 Run the stop-time re-cluster in `finalize_meeting` before notes, keeping live labels when it fails; verify a feature test where a split voice ends as one participant and the notes prompt shows one label

## 5. Feature: name inference and speaker-aware notes

- [x] 5.1 Add `build_speaker_names_request` and `parse_speaker_names` in `synthesis.rs` with validation (verbatim evidence, name in evidence, 1–40 chars, known label, unique names); verify unit tests for each rejection
- [x] 5.2 Call name inference in `finalize_meeting` after the re-cluster and before notes, only when a `channel`-source `Person N` row exists, applying via `set_inferred_speaker_name`; verify feature tests: applied name appears in the notes prompt, no request when nothing to name, failure still writes notes
- [x] 5.3 Add the owner-is-the-labelled-speaker sentence to the final and interim notes prompts; verify the prompt-content test asserts it

## 6. App and UI

- [x] 6.1 Hold `VoiceClusters` and the embedder on `ActiveMeetingSession`, created at start only when the setting is on and both diarization models are installed; emit a one-time notice when unavailable; verify by `cargo build -p vox` (src-tauri) and a meeting started with models absent records with channel labels
- [x] 6.2 Expose `voice_separation` through the meeting settings commands and `ui/src/api.ts`; verify `ui` type-check and the api test
- [x] 6.3 Add the Meetings settings toggle with model install state and the existing download action; verify a `MeetingsPage` test that enabling without models offers the download
- [x] 6.4 Show inferred names in `MeetingDetail` with an "inferred" badge and a confirm action (calls `set_meeting_speaker_name` with the same name); verify a `MeetingDetail` test for badge and confirm

## 7. Verification

- [x] 7.1 Run `cargo test --workspace`, `cargo clippy --all-targets`, and `npm test` in `ui`; all pass
- [ ] 7.2 Record a real call with at least three remote participants (voice separation on), confirm distinct `Person N` labels live, a consistent set after stop, an inferred name where one was spoken, and action-item owners using those names; record the observed `JOIN_SIM`/`MERGE_SIM` choice in `voices.rs`
