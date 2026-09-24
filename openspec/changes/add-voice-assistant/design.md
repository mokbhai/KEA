# Design

## Context

See `proposal.md` - Why. The constraints that shape this design are specific to the
existing codebase and to macOS:

- **There is no tool-calling.** `LlmEngine::complete` is `LlmRequest -> LlmResponse`,
  text in and text out, and every engine behind the registry - hosted, local, and
  OpenAI-compatible - implements only that.
- **Reading other applications is already solved.** Accessibility text capture, generic
  AX traversal, screen capture with on-device OCR, and frontmost-app context all exist
  and are already permissioned. The assistant needs no new TCC grant to read.
- **Speech is already solved.** The TTS feature, its engine registry (system, sherpa,
  hosted), the voice and speed settings, and a playback path that takes a player callback
  all exist. Speaking an answer is wiring, not new machinery.
- **Streaming STT is display-only by construction.** Its module documentation states it
  is allowed "to be lossy, to fall behind, to be wrong, and not to exist at all". Any
  design that routes user-visible consequences through it contradicts that contract.
- **The microphone is single-owner and burst-shaped.** `AudioIo` takes `&mut self`
  throughout and is built around arm-record-stop. Under hotkey activation this is
  exactly the right shape, and the assistant uses it as dictation does.
- **macOS draws the microphone indicator, not the app.** Capture flows through system
  media frameworks and privacy authorization; no third-party entitlement suppresses it.
  Under hotkey activation the indicator is lit only during a session, which is the
  behaviour users already accept from dictation.

## Goals / Non-Goals

**Goals:**

- Ask a question and hear a useful answer, in one keypress.
- Prove that spoken intent routing works before paying anything for hands-free.
- Wrong routing cannot destroy anything, by construction rather than by care.
- Argument values are never taken from a transcript known to be unreliable.
- Adding an action is a registry entry, not a new code path through the assistant.
- Leave `AudioIo` alone.

**Non-Goals:**

- Hands-free activation. See `add-assistant-wake-word`.
- Second-pass lattice rescoring. The streaming and offline engines are unrelated model
  families with no shared encoder, so the cheap form of two-pass is unavailable.
- Matching a platform assistant's breadth. The registry is finite and declared, which is
  also true of every shipping assistant - see Decisions.
- Any action with side effects, and therefore any confirmation or undo machinery.

## Decisions

### Activation is a hotkey, and hands-free is a separate change

A wake phrase was the original framing, and it is the single most expensive part of a
voice assistant: continuous capture, a permanently lit system microphone indicator, a
measurable battery draw, a second model to download, keyword-spotting false triggers,
and a change to the microphone ownership model. None of it makes the assistant's answers
better. It only removes a keypress.

Under a hotkey the assistant borrows the microphone for the length of a session and
returns it, which is precisely what dictation already does. `AudioIo` needs no change,
the indicator behaves as it does for dictation, there is no idle cost, and there are no
false triggers because activation is deliberate.

There is also a correctness argument, and it is the stronger one. KEA's distinctive
actions - summarize what is in front of me, rewrite what I selected - resolve "this"
against the frontmost application. Deliberate activation guarantees the frontmost
application is still the one the user was looking at. Any activation path that routes
through a system assistant surface puts that guarantee in question.

**Decision:** a configurable binding activates the assistant. Hands-free activation is
specified in `add-assistant-wake-word` and depends on this change.

**Alternatives considered:** shipping the wake phrase first - rejected; it front-loads
every cost and risk of the feature before the routing underneath has been shown to work,
and the routing is the part that might not.

### Routing by JSON contract, not provider tool APIs

`LlmEngine::complete` has no tool-calling, and the engine registry deliberately spans
hosted providers, local servers, and arbitrary OpenAI-compatible endpoints. Adopting
provider tool APIs would make routing quality depend on which engine the user bound, and
would leave local models - the ones privacy-minded users will pick for an assistant that
reads their screen - on a degraded path.

**Decision:** routing prompts for a JSON object naming one registry action and its
arguments, parsed and validated against the action's declared schema. Every engine routes
identically. A response that does not parse, or that names an action outside the
registry, invokes nothing.

**Trade-off:** accepted as worse than native tool-calling on models tuned for it. The
uniformity is worth more than the accuracy here, because the failure mode - a malformed
response invoking nothing - is safe. Revisit by adding a structured-output capability to
`EngineCaps` once there is evidence the parse failure rate matters.

### A finite declared registry, because that is what assistants actually are

The ambition is "it does anything I say". No shipping assistant works that way. Siri
routes to a closed set of intents and App Schemas; Android routes to declared App
Actions. Both feel open-ended because the catalog is broad and there is a graceful
fallback, not because anything interprets arbitrary commands. The 2026 replacement of
Google Assistant by Gemini did not change this: the model replaced the routing, not the
existence of a registry.

**Decision:** the assistant selects from a declared registry, and a request matching
nothing is answered conversationally rather than forced onto the nearest entry. The
conversational answer is the graceful fallback that makes a finite catalog feel open.

### Two-pass transcription, with the second pass scoped to arguments

Google's on-device two-pass shares an encoder between a streaming RNN-T and a LAS second
pass that *rescores* the first pass's hypotheses, reporting 17-22% relative WER
improvement at a fraction of a full re-decode's cost. That option is unavailable here:
the streaming and offline engines are separate model families sharing nothing, so KEA's
second pass is a complete re-decode from raw audio - the most expensive form.

Paying it on every utterance is unjustified when most requests do not need it. Intent
classification is robust to word error; a misheard verb usually still routes correctly.
Argument values are not: proper nouns are what streaming models lose first, and they are
exactly the tokens an argument cannot survive losing.

**Decision:** the streaming hypothesis drives display, endpointing, and routing. The
offline re-decode runs only when the selected action carries argument values drawn from
speech, and only that action's arguments are taken from it. An action with no spoken
arguments resolves without waiting.

**Alternatives considered:** always re-decode - rejected as latency paid on every request
for a benefit most requests cannot use. Never re-decode - rejected; it is the case that
sends a misheard proper noun into an action's arguments.

### No side effects at all, instead of a tiered permission model

An earlier version of this design built three risk tiers - immediate, undoable, confirmed
- and then populated only the safest one, on the reasoning that the contract should exist
before it was needed.

That is speculative generality. Two of the three tiers had no members, the confirmation
UI they justified had nothing to confirm, and every reader of the registry had to carry a
concept that never discriminated between any two entries. A mechanism that never fires is
not a safety property; it is untested code that looks like one.

The simpler rule is also the stronger one: **nothing the assistant can invoke has side
effects on the user's data.** One predicate, checkable by reading the registry, and it
fails closed - an action with side effects cannot be added without deleting the rule
rather than quietly landing in a tier.

This is what makes a probabilistic router acceptable. Two uncertain layers sit in front of
every invocation: the transcript may be wrong and the routing may be wrong. With no side
effects anywhere, the worst compound failure costs the user an irrelevant answer and the
time to ask again.

**Decision:** the registry contract forbids side effects. Tiers, confirmation prompts, and
undo affordances are not built.

**Alternatives considered:** keeping the tiers unpopulated - rejected above. Shipping
mutating actions behind confirmation - rejected; routing quality is unknown until the
feature is in real use, and a confirmation prompt in front of a misheard argument is a
worse experience than not offering the action at all.

**When this needs revisiting:** the first genuinely useful action with side effects. At
that point a tier model is designed against a real member, which is the right time to
design one.

### Spoken answers, and what that forces

Speech is the primary output, and it changes two things that a text answer would not care
about.

**Length becomes correctness.** Text can be skimmed, scrolled back, and abandoned
half-read. Speech is linear and takes real time, so a long spoken answer is not merely
verbose - it is unusable, and the user's only recourse is to interrupt. The answer is
therefore constrained to be brief at the point it is generated, rather than trimmed for
display afterwards.

**Decision:** one short answer, and the same text is both spoken and shown. Not a brief
spoken version alongside a fuller written one - two answers that can disagree is a defect
surface bought for very little, and a follow-up turn already covers wanting more.

**Interruption is mandatory.** Any output the user cannot stop is a failure mode, and a
spoken answer that continues after the user has heard enough - or has heard that the
question was misunderstood - is the most common one this feature will have. Stopping
playback is a first-class control, and starting a follow-up implies it.

**Both channels on by default, independently switchable.** Speaking is the point of the
feature, so it is on. Showing the text is what catches a misheard question and makes the
answer copyable, so it is also on. They are independent because the reasons to disable
each are unrelated: speech goes off in a shared office, the panel goes off when it
occludes something.

**Degradation:** with no usable speech engine, the answer is still shown and the system
says it could not be spoken. Speech is the default, never a prerequisite - the same stance
this design takes for the streaming recognizer.

### Degrade rather than require the streaming recognizer

The streaming model is not downloaded automatically, and its engine reports
`ModelNotInstalled` as the normal case. Requiring it would put a model download in front
of the assistant's first use.

**Decision:** without it, a session shows that it is listening rather than showing
partial text, and endpoints on silence using the existing energy-based speech detection.
The request is still captured, transcribed offline, routed, and acted on. Live partials
are an enhancement, not a dependency - the same stance the dictation HUD already takes.

## Risks / Trade-offs

- **Two probabilistic layers before any action.** → No action has side effects; the
  request text and selected action are shown to the user; every invocation lands in the
  activity record.
- **A brief answer may be too brief, and the user cannot skim to find that out.** → The
  same text is shown while it is spoken, and a follow-up turn is the documented way to
  get more. Watch for users repeatedly asking follow-ups to reach one obvious fact.
- **Spoken output is disruptive in shared spaces and leaks the answer to the room.** →
  Speaking is switchable independently of the panel, and playback is stoppable mid-answer.
- **Routing depends on a model the user chose, including small local ones.** → Malformed
  or out-of-registry responses invoke nothing and are reported. Degradation is a refusal,
  never a wrong action.
- **A finite registry will not match the expectation set by commercial assistants.** →
  The conversational fallback carries the miss, and the registry is a data structure, so
  breadth is additive rather than architectural.
- **The offline re-decode is uninstrumented; only insertion is timed.** → Instrument it
  as part of this work. It decides when the second pass is worth skipping.
- **Actions that read the focused application send its contents to a provider.** → The
  registry requires such actions to disclose what was read and that it was sent; the
  user's chosen engine may be local.

## Migration Plan

Purely additive. Nothing existing changes behaviour: dictation, rewrite, meetings, and
TTS are untouched, and the assistant does nothing until its binding is pressed. Rollback
is unbinding it.

## Open Questions

- **Does an App Intent invoked through the system assistant still see the user's
  frontmost application?** This gates the App Intents layer entirely. KEA's distinctive
  actions resolve "this" against the frontmost app, and if invoking the system assistant
  takes focus first, those actions cannot be exposed that way - leaving App Intents
  useful only for context-free actions the platform assistant largely already covers.
  Answerable with a trivial intent that logs the frontmost bundle identifier. Deferrable
  only because App Intents is out of scope here; it must be answered before that layer is
  planned.
- Measured cost of the offline re-decode. Deferrable: it tunes when the second pass is
  worth skipping, not whether arguments come from it.
- Whether routing quality justifies native tool-calling for engines that support it.
  Deferrable by construction: the JSON contract's failure mode is a refusal.
