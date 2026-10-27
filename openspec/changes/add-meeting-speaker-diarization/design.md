# Design

## Context

- **Attribution today** (`crates/core/src/meetings/attribution.rs`) decides `local` / `remote` / `mixed` per segment by comparing the RMS of the mic and system halves that `SpeechSegment` carries. The halves are dropped right after the STT call, and nothing is ever written to disk.
- **Storage is already open-ended.** `meeting_segments.speaker_key` is a free-text column, documented as ready for `spk0`, `spk1`, …. `meeting_speakers(meeting_id, speaker_key, display_name, source)` holds the names, and `source` is text (`channel` | `user`).
- **The prompt label.** `format_transcript_for_synthesis` labels each line with the speaker row's `display_name`. A line with no key is labelled `Speaker`.
- **File transcription** already diarizes with sherpa-onnx's whole-file `OfflineSpeakerDiarization` (pyannote segmentation + CAM++ embedding, `crates/infer/src/sherpa_diarize.rs`). That API needs the full waveform, which a meeting never has.
- **Sherpa availability.** sherpa-onnx 1.13.3 also exposes `SpeakerEmbeddingExtractor` (embed any PCM chunk; `dim()` is 192 for CAM++). The `sherpa` feature is on in every default build, because `parakeet`, `streaming` and `tts-local` all enable it.
- **Live state.** The active-meeting session lives in `ActiveMeetingSession` in `src-tauri/src/commands.rs`, next to the poll cursor and the interim schedule, and is dropped at stop.

## Goals / Non-Goals

**Goals:**
- Pure, testable clustering in `kea-core`, with no audio device and no model needed for tests.
- No schema migration.
- Zero behaviour change with the setting off.

**Non-Goals:**
- **Splitting one transcript line between two speakers.** A segment is labelled with its majority voice. Splitting text at a turn change needs word timings that not every bound STT engine returns, so it is a follow-up.
- **Naming participants live, mid-meeting.** Name inference runs once, at stop. Interim notes show "Person N".
- **Tuning the threshold per user.** One constant is used, revisited with data.

## Decisions

### 1. Window embeddings with `SpeakerEmbeddingExtractor`, not per-segment `OfflineSpeakerDiarization`

Each speech segment is cut into windows of about 1.5 s with a 0.75 s hop. Windows below the speech floor are skipped, and the rest are embedded with the CAM++ extractor. The segment's speaker is the cluster that wins most of its windows.

Alternative considered: run the whole-file diarizer on each segment. It returns cluster indices that are local to that segment, with no embedding to compare across segments, so "speaker 0" in one segment means nothing in the next. It also loads two models per call.

The segmentation model is therefore unused for meetings in v1. Only the embedding model is required, but the setting still asks for both, so the toggle and the Models page treat diarization as one bundle the user downloads once.

**Minimum evidence:** a segment needs at least 2 embeddable windows (about 2 s of speech). Below that it keeps its channel label (spec: "Segment too short to identify").

### 2. Online clustering: nearest centroid with a cosine threshold

`crates/core/src/meetings/voices.rs` (new, pure) holds a `VoiceClusters` value:

- clusters, each with a running mean centroid and a count, plus every `(segment sequence, window embedding)` pair seen so far;
- `assign(windows) -> Option<cluster index>`, which compares each window with every centroid;
- a window joins the best centroid when the cosine similarity is ≥ `JOIN_SIM`, and otherwise opens a new cluster;
- the segment takes the majority cluster of its windows, and the centroids are updated after the decision;
- a hard cap of 16 clusters: past the cap, the window joins the nearest cluster regardless of similarity.

Alternative considered: sherpa's `SpeakerEmbeddingManager`. It searches named, enrolled speakers and has no notion of a running centroid or of reclustering, so it doesn't fit.

Keys are `spk{index}`, reusing `kea_core::transcript::speakers::speaker_key`, and display names are `Person {index+1}`. The name is not "Speaker N" (the file-transcript wording) because the notes prompt already uses a bare `Speaker` to mean "could not be attributed". "Speaker 3" next to "Speaker" would blur exactly the distinction the prompt relies on.

Which segments are embedded:

| capture mode | channel decision | embedded audio | label if assigned |
|---|---|---|---|
| two-source | `remote` | system half | `spkN` |
| two-source | `local` | none | `local` ("You") |
| two-source | `mixed` | system half (amended 2026-10-23: on speakers nearly every segment is mixed) | `spkN`, or `mixed` if too little speech |
| mic-only | n/a | mixed signal | `spkN` |

For two-source meetings, embedding the system half rather than the mixed signal keeps the local user's voice and echo out of the remote clusters.

### 3. Re-cluster at stop: agglomerative clustering over all windows, then map old keys onto new clusters

In `finalize_meeting`, after the tail segment and before name inference and notes:

1. Run average-linkage agglomerative clustering over every stored window embedding, stopping at `MERGE_SIM`.
2. Relabel each segment by the majority of its windows.
3. Map the new clusters to old keys greedily by window overlap, largest first, so a user-renamed `spk0` keeps its name on the cluster that holds most of its windows.
4. Renumber the unmapped clusters by first appearance.
5. Rewrite `speaker_key` on the changed segments, and delete `meeting_speakers` rows left with no segments, unless their source is `user`. A user-named participant that the merge absorbed has its name moved onto the surviving key, if that key is not itself user-named.

All of this happens in one repo transaction, `remap_segment_speakers`.

**Fragments (amended 2026-10-27).** A real call with two remote people came out as seven participants, all but one of them under 40 s. On AMI meetings (4 speakers, clean and through a 16 kbit/s Opus call path), the pass labelled 7, 13, 13 and 15 people. Even sherpa's own whole-meeting diarizer, with the same two models, found 19 speakers at threshold 0.7 and 6 at 1.0. CAM++ cannot reliably place a few seconds of meeting audio, and no merge threshold separated the same-speaker and different-speaker distributions. So after each segment takes its majority label, any cluster under about 30 s of speech (40 windows) that is also under a quarter of the largest cluster is folded into the most similar cluster that isn't itself a fragment. On the same four AMI runs that gives 5, 4, 3 and 6 people. A participant who says a single sentence is the price: they are labelled as whoever they sound most like.

The cost is O(n²) over windows. A one-hour meeting with about half its time remote speech is around 2,400 windows, which takes well under a second on the CPU. Above 6,000 windows the pass clusters the per-segment means instead of individual windows.

Alternative considered: keep live labels as final. That was rejected because online clustering decides early, with little evidence, and the spec requires stable labels.

### 4. Embeddings live in `ActiveMeetingSession`, in memory only

`ActiveMeetingSession` gains `voices: Option<VoiceClusters>`. The value is `None` when the setting is off or the extractor failed to load. The extractor itself is created once per meeting at start, on a blocking thread, and is also held there.

Dropping the session at stop or on error drops the embeddings, which satisfies "voice evidence does not outlive the meeting" structurally rather than through a cleanup step that someone could forget.

Alternative considered: a `meeting_voice_embeddings` table, deleted at finalize. It survives a crash, which is precisely the property we don't want for biometric data, and it needs a migration.

`run_meeting_poll_segment` and `finalize_meeting` take `Option<&mut VoiceClusters>` plus the extractor handle. The feature crate stays ignorant of Tauri state.

### 5. `NewSegment.speaker` becomes a key, not a `SpeakerChannel`

The `speaker: Option<SpeakerChannel>` field on `NewSegment` widens to `speaker_key: Option<String>`. `SpeakerChannel` keeps `as_str()` for the channel keys. `ensure_speaker` gains a sibling, `ensure_participant(meeting_id, key, display_name)`, which writes source `channel`.

Participant rows are created lazily, when a cluster first appears. Unlike the "You" / "Others" rows, they can't be seeded at start.

In a two-source meeting with voice separation active, the "Others" row is created only if a remote segment actually lands unassigned. Otherwise the speaker list would show an "Others" nobody is labelled as.

### 6. `SpeakerSource::Inferred`, and name inference as its own request before the final notes

A third source value, `inferred`, is added. Precedence is `user` > `inferred` > `channel`, and `set_speaker_name` (the user path) always writes `user`. A new `set_inferred_speaker_name` writes only where `source != 'user'`.

**The request.** At stop, after the re-cluster, if any participant row still has source `channel` and a `Person N` name, one LLM request is sent with the formatted transcript and the list of labels. It returns `{"names":[{"speaker":"Person 2","name":"Dan","evidence":"…"}]}`. It uses the same tolerant JSON parse as the notes, with no repair round trip: a failed parse simply means no names.

**Validation in core, before anything is written:**
- the evidence is a verbatim substring of the formatted transcript;
- the name appears in the evidence, compared case-insensitively;
- the name is 1–40 characters;
- the speaker label exists;
- no two participants are given the same name.

Alternative considered: a `speaker_names` key inside the notes JSON. It is one fewer call, but the notes would then be written against "Person 2" and need a rewrite pass afterwards. Running the names first means the notes prompt already sees "Dan". The extra call only happens when there is something to name.

The calendar title is still applied after notes, unchanged. Calendar data never enters this request (spec).

### 7. Owners follow renames

`set_speaker_name` and `set_inferred_speaker_name` also run, in the same transaction, `UPDATE meeting_action_items SET owner = :new WHERE meeting_id = :m AND owner = :old`, then re-render the `meeting_notes.action_items` prose column from the rows. They don't use `persist_notes`: there is no LLM call here. They use the same `render_action_items_prose`.

Exact-match on the old display name is deliberate. Fuzzy matching ("Dan" vs "Daniel") would rewrite owners nobody renamed.

The prompt change is one sentence in both the final and the interim system prompts: when the person who committed is a labelled speaker, the owner is that label. No prompt version bump is needed, because what `prompt_version` records ("written from a transcript that said who spoke") is unchanged.

### 8. Setting

`MeetingSettings.voice_separation: bool`, `#[serde(default)]` (false), the same pattern as `interim_notes` and `calendar_titles`. The meetings settings UI shows the toggle with the diarization models' install state, and reuses the Models page download for the two `DIARIZATION_*_ID` entries.

## Risks / Trade-offs

- **[Similar voices or compressed call audio merge two people]** → `MERGE_SIM` is tuned conservatively, because a wrong split is easier to fix by renaming than a wrong merge. Merging two participants from the UI is a natural follow-up, not in this change.
- **[Crosstalk inside one segment is labelled by its majority voice]** → This is a documented non-goal. The segment cutter already ends segments at pauses, which keeps most segments to one speaker.
- **[A hallucinated name]** → It must quote the transcript verbatim and contain the name. It is shown as inferred, with a confirm action, and never overwrites a user name.
- **[CPU contention with STT on the same thread pool]** → Embedding runs on `spawn_blocking`, about 5 ms per window on CAM++. It is skipped entirely for local and mixed segments in two-source mode.
- **[Thresholds tuned on English VoxCeleb]** → CAM++ is the model already shipped. The thresholds are constants in one module, with a test fixture of synthetic embeddings, so retuning is a one-line change.
- **[Crash mid-meeting loses the stop-time pass]** → Live labels are already persisted per segment, so the meeting keeps reasonable labels. This loss is the price of not persisting voiceprints, and it is accepted.

## Migration Plan

- No database migration. `inferred` is a new text value in an existing text column. Old builds reading it would fail `SpeakerSource::from_str`, but downgrade is not a supported path.
- The feature ships default-off. Rollback means turning the setting off, which is behaviourally identical to today.

## Open Questions

- The exact values of `JOIN_SIM` and `MERGE_SIM` (starting points: 0.55 and 0.6 cosine similarity). They will be settled on a few recorded calls during implementation, and changing them affects no spec or task.
