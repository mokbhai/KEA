# Spec Delta

## Purpose

The catalog of tools the assistant may use on the user's behalf — reading what is in
front of the user, and a small set of application actions — bounded by the rule that
none of them may change or destroy anything.

## ADDED Requirements

### Requirement: Every action declares a name and its arguments
Each entry in the action registry SHALL declare a stable name and a schema for its
arguments. An entry missing either MUST NOT be invocable.

#### Scenario: Entry is complete
- **WHEN** an action declares a name and an argument schema
- **THEN** the assistant may select and invoke it

#### Scenario: Entry declares no argument schema
- **WHEN** an action does not declare an argument schema
- **THEN** it is not available for selection

### Requirement: The assistant cannot change or destroy anything
Every action in the registry SHALL be free of side effects on the user's data. The
system MUST NOT expose through the assistant any action that modifies, deletes,
transmits, sends, or spends on the user's behalf.

#### Scenario: Registry contents
- **WHEN** the assistant's action registry is assembled
- **THEN** every entry in it either reads state or starts an activity the user can stop
- **AND** no entry modifies, deletes, transmits, or sends anything

#### Scenario: A wrongly routed request
- **WHEN** the assistant selects the wrong action for a request
- **THEN** nothing belonging to the user is altered or lost
- **AND** the user can simply ask again

#### Scenario: An action with side effects is proposed
- **WHEN** an action that would modify or destroy user data is added to the registry
- **THEN** the system rejects it rather than exposing it to the assistant

### Requirement: Every invocation is recorded
The system SHALL record each action invocation, including the action selected and how it
ended — completed, failed, or cancelled — in the application's activity record, so the
user can review what the assistant did.

#### Scenario: Action completes
- **WHEN** an action runs to completion
- **THEN** the invocation and its outcome are recorded

#### Scenario: Action fails
- **WHEN** an action fails
- **THEN** the invocation is recorded as failed

#### Scenario: User cancels the session mid-action
- **WHEN** the user cancels the session while an action is in progress
- **THEN** the invocation is recorded as cancelled
- **AND** it is not recorded as a failure

### Requirement: Actions that read another application disclose what they read
When an action reads content from an application other than the assistant's own, the
system SHALL make clear to the user what was read and, where the content is sent to an
external service, that it was sent.

#### Scenario: Action reads and summarises another application's content
- **WHEN** an action reads content from another application and sends it to an external service
- **THEN** the user is shown what was read and that it was sent externally

#### Scenario: Action reads nothing
- **WHEN** an action needs no content from another application
- **THEN** no disclosure is shown

### Requirement: An unavailable action fails visibly
When a selected action cannot run because a resource, permission, or target it needs is
unavailable, the system SHALL report what is missing and SHALL NOT report the request as
completed.

#### Scenario: Required permission is missing
- **WHEN** a selected action needs a permission the user has not granted
- **THEN** the assistant reports which permission is required
- **AND** the request is not reported as completed

#### Scenario: Target application is unavailable
- **WHEN** a selected action targets an application that cannot be reached
- **THEN** the assistant reports that the target was unavailable
