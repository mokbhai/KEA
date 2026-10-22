# Proposal

## Why

A meeting transcript today knows only two speakers: "You" (the microphone) and "Others" (system audio). On a call with five or ten people, every remote line is "Others", so the notes cannot say who said what, and action-item owners end up as role guesses ("Onboarding lead", "New hire") instead of the people who agreed to the task. The voice models needed to fix this are already shipped for file transcription (`pyannote-segmentation-3-0` + `campplus-sv-en-voxceleb-16k`, via sherpa-onnx). Meetings just can't use them, because a meeting's audio is dropped segment by segment and the existing diarizer needs the whole recording.

## What Changes

- **Live voice separation for meetings (opt-in).** Each speech segment gets voice embeddings computed while its audio is still in memory. The embeddings are grouped online into per-meeting speakers labelled "Person 1", "Person 2", … In a two-source meeting this applies to the remote side, and the microphone stays "You". In a mic-only meeting (one shared mic in a room) it applies to everything.
- **Re-clustering at stop.** When the meeting ends, all of its embeddings are clustered again in one pass. That fixes early mistakes, such as one person split in two or two people merged, so a label means the same person for the whole meeting. Names the user typed survive the remap.
- **Embeddings are never persisted.** They live in memory for the active meeting only and are discarded after the stop pass. No audio is written to disk and no voiceprint outlives the meeting.
- **Name inference from the conversation.** Before the final notes pass, one LLM request proposes a real name for each "Person N" from cues in the transcript ("Thanks, Dan", "Hi, I'm Priya"). A proposal is applied only when its evidence quote is found verbatim in the transcript. It is marked as inferred, the user can confirm or rename it, and it never overwrites a name the user typed.
- **Speaker-aware notes.** The notes prompt is told to use speaker labels as action-item owners when the person who committed is a labelled speaker. Renaming a speaker updates the owner on that meeting's action items.
- **Graceful fallback.** With the setting off, the models missing, or the runtime failing, meetings behave exactly as today ("You"/"Others").

## Capabilities

### New Capabilities
- `meetings/live-diarization`: separating a meeting's participants by voice during recording, stable per-meeting speaker labels, the re-cluster at stop, the privacy limits on embeddings, and fallback behaviour.
- `meetings/speaker-naming`: inferring participant names from transcript evidence, how inferred, default and user-typed names take precedence over each other, and carrying speaker names into notes and action-item owners.

### Modified Capabilities
<!-- None: openspec/specs/ has no existing meeting specs. -->

## Impact

- **`crates/infer`**: a new streaming-friendly speaker-embedding API over sherpa-onnx's `SpeakerEmbeddingExtractor`, behind the existing `sherpa` feature. It reuses the already-catalogued embedding model.
- **`crates/core`**: a pure online/offline clustering module in `meetings/`, a new `SpeakerSource::Inferred` value, repo methods to remap segment speaker keys and rename action-item owners, and a `voice_separation` flag on `MeetingSettings`. No migration is needed: the existing `speaker_key` and `meeting_speakers` columns are already an open set, and `SpeakerSource` is stored as text.
- **`crates/features/src/meeting.rs`**: embeddings are computed in `run_meeting_poll_segment` and on the stop-time tail, and `finalize_meeting` gains the re-cluster and name-inference steps.
- **`src-tauri`**: the active-meeting state holds the in-memory embedding buffer. The settings command and the speaker-rename command change.
- **`ui`**: a Meetings setting toggle that requires the diarization models, a "Person N" display, and an inferred-name badge with a confirm action in `MeetingDetail`.
- **Cost**: CPU inference per segment (CAM++ is small). There is one extra LLM call per meeting at stop, made only when unnamed "Person N" speakers exist. There is no new download beyond the two existing diarization models.
- **Out of scope**: recognising people across meetings (a biometric database, a separate decision), and matching against calendar attendees (calendar data must not reach an engine).
