# Spec Delta

## Purpose

Turns anonymous participant labels into real names when the conversation itself
reveals them, and carries whichever names the meeting ends up with into its notes and
action-item owners, without ever overriding a name the user chose.

## ADDED Requirements

### Requirement: Names are inferred only from transcript evidence
When a meeting stops with one or more participants still under a default "Person N"
label, the system SHALL ask the meeting's LLM to propose a name for each, from cues in
the transcript such as being addressed by name or introducing themselves. A proposed
name SHALL be applied only when the proposal includes a quote that appears verbatim in
the meeting's transcript and contains that name. Proposals that fail this check MUST be
discarded. The request MUST NOT include calendar data or anything other than the
transcript and the participant labels.

#### Scenario: A participant is addressed by name
- **WHEN** the transcript contains "You: Thanks, Dan, that helps" immediately followed by a line from "Person 2"
- **THEN** the system may label Person 2 "Dan", citing that line as evidence

#### Scenario: A proposal without verifiable evidence
- **WHEN** the model proposes "Priya" for Person 3 with a quote that does not appear in the transcript
- **THEN** Person 3 keeps the label "Person 3"

#### Scenario: Nobody needs a name
- **WHEN** every participant already has a user-typed or inferred name, or no participant labels exist
- **THEN** no name-inference request is made

#### Scenario: Name inference fails
- **WHEN** the name-inference request errors or returns something unparseable
- **THEN** participants keep their "Person N" labels and the notes are still written

### Requirement: User-typed names always win
Each participant name SHALL record whether it is a default label, an inferred name, or
a name the user typed. Inference MUST NOT replace a user-typed name. The user SHALL be
able to confirm an inferred name, which makes it user-typed, or replace it with any
other name.

#### Scenario: Inferred name shown as a suggestion
- **WHEN** a participant's name was inferred
- **THEN** the meeting view marks it as inferred and offers to confirm or rename it

#### Scenario: User already renamed the participant
- **WHEN** the user renamed Person 1 to "Sam" before the meeting stopped, and inference proposes "Samuel"
- **THEN** the participant stays "Sam"

### Requirement: Notes and action items use participant names
The final notes SHALL be written from a transcript that shows each line's current
participant name. When the person who committed to an action item is a labelled
participant, the item's owner SHALL be that participant's name or label, not a role
inferred from the conversation. Renaming a participant SHALL update the owner of that
meeting's action items that carry the participant's previous name.

#### Scenario: Owner is the speaker who agreed
- **WHEN** "Person 2" says "I'll send the deck by Friday"
- **THEN** the resulting action item's owner is "Person 2" (or that participant's name, if one is known by the final pass)

#### Scenario: Renaming after the notes were written
- **WHEN** the user renames "Person 2" to "Dan" after the meeting finished
- **THEN** that meeting's action items owned by "Person 2" show "Dan" as owner, and action items in other meetings are unchanged
