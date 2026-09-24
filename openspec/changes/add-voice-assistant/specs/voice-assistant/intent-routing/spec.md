# Spec Delta

## Purpose

Turns a spoken request into an answer, or — where the request calls for one — into a
single action from the assistant's registry with its arguments, while ensuring that a
request the assistant did not understand is never resolved by guessing at an action.

## ADDED Requirements

### Requirement: A request resolves to at most one action
The system SHALL route a completed request to at most one action from the action
registry. It MUST NOT invoke multiple actions from a single request. A request that
resolves to no action is answered instead.

#### Scenario: Request names a known action
- **WHEN** the user's request corresponds to an action in the registry
- **THEN** that action is selected
- **AND** no other action is invoked for that request

### Requirement: Routing is limited to the declared registry
The system SHALL select only from actions currently present in the registry, and SHALL
NOT invoke behavior that no registry entry declares, regardless of what the language
model returns.

#### Scenario: Model names something not in the registry
- **WHEN** the routing step returns an action name that the registry does not contain
- **THEN** no action is invoked
- **AND** the assistant reports that it cannot do what was asked

### Requirement: Arguments conform to the action's declared schema
The system SHALL validate the arguments for a selected action against that action's
declared argument schema before invoking it, and SHALL NOT invoke an action whose
required arguments are missing or invalid.

#### Scenario: Required argument is missing
- **WHEN** a selected action requires an argument that the request did not supply
- **THEN** the action is not invoked
- **AND** the assistant asks the user for the missing value

#### Scenario: Argument fails validation
- **WHEN** a supplied argument does not conform to the action's declared schema
- **THEN** the action is not invoked
- **AND** the assistant reports what it could not interpret

### Requirement: Argument values come from the accurate transcript
When a selected action carries argument values taken from the user's speech, the system
SHALL derive those values from the transcript produced by the accuracy-oriented speech
engine, not from the live recognition hypothesis used for display and endpointing.

#### Scenario: Action with a spoken argument value
- **WHEN** a selected action takes an argument whose value came from the user's speech
- **THEN** that value is taken from the accurate transcript of the request audio

#### Scenario: Action with no spoken argument values
- **WHEN** a selected action requires no argument values drawn from speech
- **THEN** the request may be resolved without waiting for the accurate transcript

### Requirement: Answering is the default outcome
The system SHALL answer the user's request directly unless the request calls for an
action the registry declares. A request that corresponds to no action is answered, not
forced onto the nearest available entry, and answering is a success rather than a
failure to route.

#### Scenario: General question
- **WHEN** the user asks something that no registry action covers
- **THEN** the assistant answers the question
- **AND** no action is invoked

#### Scenario: Request that names an action
- **WHEN** the user's request corresponds to a declared action
- **THEN** that action is invoked rather than described in an answer

#### Scenario: Request that is close to an action but is a question about it
- **WHEN** the user asks a question about what an action would do rather than asking for it
- **THEN** the assistant answers the question
- **AND** the action is not invoked

### Requirement: An ambiguous request is clarified, not guessed
When a request could plausibly resolve to more than one action, or to an action whose
target cannot be determined, the system SHALL ask the user to clarify rather than
choosing on their behalf.

#### Scenario: Request matches several actions
- **WHEN** a request corresponds to more than one registry action
- **THEN** the assistant asks the user which was meant
- **AND** no action is invoked until the user answers

#### Scenario: Target cannot be resolved
- **WHEN** a request refers to a target that cannot be determined from the current context
- **THEN** the assistant asks the user what the request refers to

### Requirement: Routing failures are reported
When the routing step fails — because the language model is unreachable, returns a
malformed result, or times out — the system SHALL report the failure to the user and
SHALL NOT invoke any action.

#### Scenario: Routing is unreachable
- **WHEN** the language model used for routing cannot be reached
- **THEN** the assistant reports that it could not process the request
- **AND** no action is invoked

#### Scenario: Routing returns a malformed result
- **WHEN** the routing step returns a result that cannot be interpreted
- **THEN** no action is invoked
- **AND** the assistant reports that it could not process the request

### Requirement: The user can see what was understood
Before or alongside acting, the system SHALL make visible to the user the request text
it acted on and the action it selected.

#### Scenario: Action is invoked
- **WHEN** the assistant invokes an action
- **THEN** the request text and the selected action are visible to the user
