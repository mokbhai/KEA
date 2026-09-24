# Spec Delta

## Purpose

The assistant conversation surface: how a session starts, and what the user sees and
hears between that moment and the moment it closes, including live feedback while
speaking, deciding when the request has ended, delivering the answer aloud and on
screen, and asking follow-up questions.

## ADDED Requirements

### Requirement: A session starts from an explicit user action
The system SHALL activate an assistant session only in response to a deliberate user
action, and SHALL NOT capture microphone audio for the assistant at any other time.

#### Scenario: User activates the assistant
- **WHEN** the user performs the assistant's activation action
- **THEN** an assistant session is activated
- **AND** microphone capture for the assistant begins

#### Scenario: Assistant is not in use
- **WHEN** no assistant session has been activated
- **THEN** no microphone audio is captured for the assistant

#### Scenario: Activation while another feature holds the microphone
- **WHEN** the user activates the assistant while dictation or meeting capture is recording
- **THEN** the system reports that the microphone is in use
- **AND** the in-progress recording is not interrupted

### Requirement: An activated session presents a request surface
When a session is activated, the system SHALL present a surface showing that the
assistant is listening, without taking keyboard focus from the user's current
application.

#### Scenario: Session opens
- **WHEN** an assistant session is activated
- **THEN** a surface appears indicating that the assistant is listening
- **AND** keyboard focus remains with the application the user was using

### Requirement: Speech is shown as it is recognised
While the user is speaking their request, the system SHALL display the recognised text
as it becomes available, so the user can see whether they were heard before the request
is acted on.

#### Scenario: User speaks a request
- **WHEN** the user speaks after activation
- **THEN** the recognised text is displayed and updated while they are still speaking

#### Scenario: Live recognition is unavailable
- **WHEN** live recognition cannot produce text for the current request
- **THEN** the surface indicates that the assistant is listening without displaying partial text
- **AND** the request is still captured and processed when the user stops speaking

### Requirement: The request ends when the user stops speaking
The system SHALL treat a sustained pause in speech as the end of the request and
proceed without requiring a button press. The user SHALL also be able to end the
request explicitly.

#### Scenario: User pauses after speaking
- **WHEN** the user stops speaking for the endpointing interval
- **THEN** the request is treated as complete
- **AND** the assistant begins processing it

#### Scenario: User ends the request explicitly
- **WHEN** the user explicitly submits the request before the pause elapses
- **THEN** the request is treated as complete at that point

#### Scenario: User says nothing
- **WHEN** a session is activated and no speech follows
- **THEN** the session closes without sending anything to a language model

### Requirement: The session reports what it is doing
The system SHALL distinguish, in the surface, between listening to the user, working on
the request, and presenting a result, so that a slow request is not mistaken for a
failure.

#### Scenario: Request is being processed
- **WHEN** the request has ended and the assistant is working on it
- **THEN** the surface indicates that work is in progress

#### Scenario: Request fails
- **WHEN** the assistant cannot complete the request
- **THEN** the surface states that it failed and why
- **AND** the session remains open so the user can retry or rephrase

### Requirement: Answers are brief enough to be spoken
The system SHALL constrain the assistant's answers to a length suitable for listening
to. A single answer is delivered; the user asks a follow-up for more rather than
receiving a longer one.

#### Scenario: Question with a long possible answer
- **WHEN** the user asks something that could be answered at length
- **THEN** the answer given is brief
- **AND** the same text is what is both shown and spoken

#### Scenario: User wants more
- **WHEN** the user wants detail beyond the brief answer
- **THEN** they obtain it by asking a follow-up in the same session

### Requirement: Answers are spoken and shown
The system SHALL speak the assistant's answer aloud and display it in the session
surface. Speaking and displaying are each independently switchable by the user, and both
are on unless the user has turned one off.

#### Scenario: Default configuration
- **WHEN** the assistant answers and the user has changed no output setting
- **THEN** the answer is spoken aloud
- **AND** the answer is displayed in the session surface

#### Scenario: User turns speech off
- **WHEN** the user has turned speaking off
- **THEN** the answer is displayed and not spoken

#### Scenario: User turns the display off
- **WHEN** the user has turned the display off
- **THEN** the answer is spoken and no answer text is displayed

#### Scenario: Speech is unavailable
- **WHEN** the answer cannot be spoken because no speech engine is available
- **THEN** the answer is still displayed
- **AND** the system reports that it could not be spoken

### Requirement: Speech can be stopped while it is playing
The system SHALL allow the user to stop a spoken answer before it finishes, and SHALL
stop it immediately when they do.

#### Scenario: User stops playback
- **WHEN** the user stops the spoken answer part-way through
- **THEN** playback stops immediately
- **AND** the answer text remains available in the session surface

#### Scenario: A follow-up begins while speaking
- **WHEN** the user begins a follow-up turn while an answer is still being spoken
- **THEN** playback stops
- **AND** the follow-up is captured

### Requirement: The displayed answer can be kept
The system SHALL allow the user to copy a displayed answer.

#### Scenario: User copies an answer
- **WHEN** an answer is displayed
- **THEN** the user can copy it

### Requirement: Follow-up turns do not require re-activation
While a session is open, the system SHALL allow the user to speak a follow-up request
without performing the activation action again, and SHALL make the preceding turns of
that session available as context for the follow-up.

#### Scenario: User asks a follow-up
- **WHEN** a result is displayed and the user starts a follow-up turn
- **THEN** the follow-up is captured without the assistant being activated again
- **AND** the earlier turns of the session inform the response

### Requirement: Activating during an open session does not start a second one
While a session is open, the system SHALL NOT start an additional session in response
to the activation action, and SHALL instead direct the action at the open session.

#### Scenario: User activates while a session is open
- **WHEN** the user performs the activation action while a session is open
- **THEN** no additional session is activated

### Requirement: The user can cancel at any point
The system SHALL allow the user to cancel a session while listening, while processing,
or while a result is displayed. Cancelling during processing MUST stop the request from
having further effect where it has not already completed.

#### Scenario: Cancel while listening
- **WHEN** the user cancels while the assistant is listening
- **THEN** the session closes and the captured audio is discarded

#### Scenario: Cancel while processing
- **WHEN** the user cancels while a request is being processed
- **THEN** the session closes
- **AND** no action that had not already begun is carried out

#### Scenario: Cancel while an answer is being spoken
- **WHEN** the user cancels while an answer is being spoken
- **THEN** playback stops immediately
- **AND** the session closes

### Requirement: Session history does not outlive the session
The system SHALL discard a session's turns when the session closes, and SHALL NOT make
them available as context to a later session.

#### Scenario: New session after a closed one
- **WHEN** a session closes and a new session is later activated
- **THEN** the new session has no access to the previous session's turns
