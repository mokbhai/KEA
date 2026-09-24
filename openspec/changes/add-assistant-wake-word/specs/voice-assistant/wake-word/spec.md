# Spec Delta

## Purpose

Detects a spoken wake phrase from continuous microphone audio and activates the
assistant, under the disclosure, consent, and microphone-arbitration constraints that
always-on listening demands of a desktop application.

## ADDED Requirements

### Requirement: Always-on listening is opt-in
The system SHALL NOT capture continuous microphone audio for wake-phrase detection
unless the user has explicitly enabled the assistant. The setting MUST default to
disabled, and MUST remain disabled after an update that introduces it.

#### Scenario: Fresh install does not listen
- **WHEN** the application is installed and launched for the first time
- **THEN** continuous wake-phrase listening is not active
- **AND** no microphone capture occurs until a feature the user invoked requires it

#### Scenario: Existing user updates into the feature
- **WHEN** a user who already used the application updates to a version that includes the assistant
- **THEN** wake-phrase listening is disabled
- **AND** the user is not opted in by the update

### Requirement: Wake phrase activates an assistant session
The system SHALL activate an assistant session when the configured wake phrase is
recognised in speech, and SHALL NOT activate on speech that does not contain it.

#### Scenario: Wake phrase spoken
- **WHEN** listening is enabled and the user speaks the configured wake phrase
- **THEN** an assistant session is activated
- **AND** the audio spoken after the phrase is captured as the user's request

#### Scenario: Ordinary speech near the microphone
- **WHEN** listening is enabled and the user speaks without the wake phrase
- **THEN** no assistant session is activated
- **AND** no transcript of that speech is sent to any language model

### Requirement: The wake phrase is configurable
The system SHALL allow the user to choose the wake phrase from a set of supported
phrases, and SHALL apply a change without requiring a restart.

#### Scenario: User changes the phrase
- **WHEN** the user selects a different supported wake phrase
- **THEN** subsequent activations require the new phrase
- **AND** the previous phrase no longer activates a session

### Requirement: Active listening is visibly indicated
While continuous listening is active, the system SHALL present a persistent, visible
indication that the microphone is being monitored. The indication MUST be visible
without opening the application's settings.

#### Scenario: Listening is enabled
- **WHEN** wake-phrase listening is active
- **THEN** a persistent indicator shows that the microphone is being monitored

#### Scenario: Listening is suspended
- **WHEN** wake-phrase listening is suspended or disabled
- **THEN** the indicator reflects that the microphone is no longer being monitored

### Requirement: Listening can be stopped immediately
The system SHALL provide a way to suspend or disable wake-phrase listening that is
reachable without navigating into settings, and SHALL stop microphone capture for
wake-phrase detection as soon as it is used.

#### Scenario: User suspends listening from the indicator
- **WHEN** the user disables listening from the persistent indicator
- **THEN** continuous microphone capture for wake-phrase detection stops
- **AND** no further wake phrase activates a session until listening is re-enabled

### Requirement: Non-activating audio is not retained
The system SHALL NOT persist audio or transcripts of speech that did not activate a
session. Audio examined for the wake phrase and found not to contain it MUST be
discarded, and MUST NOT be written to disk or transmitted off the device.

#### Scenario: Speech that never activates
- **WHEN** listening is active and the user talks for an extended period without saying the wake phrase
- **THEN** no audio from that period is written to disk
- **AND** no transcript from that period is transmitted to any external service

### Requirement: Wake-phrase listening yields the microphone to explicit capture
When the user explicitly starts dictation, meeting capture, or another feature that
records the microphone, that feature SHALL take precedence over wake-phrase listening.
The system SHALL report the resulting state rather than failing silently.

#### Scenario: Dictation starts while listening is active
- **WHEN** the user starts dictation while wake-phrase listening is active
- **THEN** dictation captures the microphone and proceeds unaffected
- **AND** the indicator reflects whether the wake phrase is still being monitored

#### Scenario: Explicit capture ends
- **WHEN** the explicit capture that took precedence finishes
- **AND** wake-phrase listening is still enabled
- **THEN** wake-phrase listening resumes without user action

### Requirement: Activation requires its supporting model to be present
The assistant SHALL NOT report itself as listening when the speech model required for
wake-phrase detection is unavailable. The system MUST tell the user what is missing and
how to obtain it, rather than appearing enabled and never activating.

#### Scenario: Model is not installed
- **WHEN** the user enables the assistant and the required speech model is not installed
- **THEN** the system states that the model is required and offers to obtain it
- **AND** the assistant is not presented as active until the model is available
