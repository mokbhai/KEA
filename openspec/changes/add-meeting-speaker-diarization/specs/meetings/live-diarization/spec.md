# Spec Delta

## Purpose

Separates the participants of a recorded meeting by voice, so a transcript with more
than two people says which of them spoke each line, without keeping audio or
voiceprints beyond the meeting.

## ADDED Requirements

### Requirement: Voice separation is opt-in and needs the diarization models
The system SHALL separate meeting participants by voice only when the user has turned
on the meetings voice-separation setting and both diarization models (speaker
segmentation and speaker embedding) are installed. The setting MUST default to off.

#### Scenario: Setting off
- **WHEN** a meeting is recorded with voice separation turned off
- **THEN** every segment is labelled exactly as before this change ("You", "Others", or the mixed label)

#### Scenario: Models missing at start
- **WHEN** voice separation is on but either diarization model is not installed when a meeting starts
- **THEN** the meeting records with channel labels only, and the user is told voice separation is unavailable until the models are downloaded

#### Scenario: Turning the setting on without the models
- **WHEN** the user turns voice separation on and a diarization model is missing
- **THEN** the settings view offers to download the missing models instead of silently enabling a setting that cannot run

### Requirement: Remote participants are told apart during the meeting
While a meeting records with voice separation active, the system SHALL assign each
speech segment to a per-meeting participant label of the form "Person N", numbered in
order of first appearance from 1. A segment labelled with a participant SHALL show
that label in the live transcript as soon as the segment appears.

#### Scenario: Two-source meeting with several remote voices
- **WHEN** a meeting captures both microphone and system audio, and three different remote voices speak
- **THEN** microphone segments are labelled "You", and remote segments are labelled "Person 1", "Person 2" and "Person 3" according to whose voice they carry

#### Scenario: Mic-only meeting in a room
- **WHEN** a meeting captures only the microphone and several people in the room speak
- **THEN** segments are labelled "Person 1", "Person 2", … by voice, and none of them is labelled "You" by assumption

#### Scenario: Speaker changes inside a chunk
- **WHEN** two or more people speak within one captured chunk (a hand-over shorter than the pause that ends a chunk)
- **THEN** the chunk is split at the speaker turns and each turn is transcribed and stored as its own line with its own label and time range; turns under about a second are folded into the neighbouring turn, and if no turn can be found the chunk is stored whole as before

#### Scenario: Ambiguous channel
- **WHEN** a two-source segment is attributed to neither channel (both sources were active, as on laptop speakers where the mic also hears the call)
- **THEN** it is assigned to a participant from the system audio alone, which carries only the call; if the system audio holds too little speech to identify a voice, it keeps the mixed label

#### Scenario: Segment too short to identify
- **WHEN** a segment carries too little speech to compute a reliable voice embedding
- **THEN** it keeps its channel label ("Others" or the mixed label) rather than being forced onto a participant

#### Scenario: Same voice later in the meeting
- **WHEN** a participant labelled "Person 2" speaks again ten minutes later
- **THEN** the new segment is labelled "Person 2", not a new participant

### Requirement: Participant labels are made consistent when the meeting stops
When a meeting with voice separation stops, the system SHALL reassign every
participant-labelled segment using all of the meeting's voice evidence at once, before
the final notes are written. Labels SHALL be renumbered by order of first appearance.
A name the user typed for a participant during the meeting MUST stay attached to the
same voice after reassignment.

#### Scenario: One voice was split in two during the meeting
- **WHEN** live grouping labelled one person as both "Person 2" and "Person 4"
- **THEN** after stop, all of that person's segments carry one label, and the transcript, notes and speaker list no longer show the extra participant

#### Scenario: User renamed a participant mid-meeting
- **WHEN** the user renamed "Person 1" to "Dan" during the meeting, and the stop-time pass renumbers participants
- **THEN** the segments of Dan's voice are still shown as "Dan"

#### Scenario: Stop-time pass fails
- **WHEN** the stop-time reassignment cannot run
- **THEN** the live labels are kept unchanged and the meeting still completes with notes

### Requirement: Voice evidence does not outlive the meeting
The system MUST NOT write meeting audio or voice embeddings to disk, and MUST discard
a meeting's voice embeddings once its stop-time pass has finished or the meeting has
ended in error. Participant labels SHALL be scoped to one meeting: "Person 1" in one
meeting carries no identity into any other meeting.

#### Scenario: Meeting finished
- **WHEN** a meeting with voice separation has been finalized
- **THEN** no voice embedding for it exists in memory or in any database or file

#### Scenario: App quits mid-meeting
- **WHEN** the app exits while a meeting is recording
- **THEN** the embeddings are lost with the process, and the segments already stored keep their live labels

#### Scenario: Two meetings with the same person
- **WHEN** the same person speaks in two separate meetings
- **THEN** the system makes no link between their labels in the two meetings

### Requirement: Voice separation failures never fail the meeting
A failure to load the embedding model or to compute an embedding SHALL leave the
affected segments with their channel labels and MUST NOT stop, pause or fail the
recording.

#### Scenario: Embedding model fails to load mid-meeting
- **WHEN** the embedding runtime errors while a meeting is recording
- **THEN** recording and transcription continue, and later segments are labelled by channel only
