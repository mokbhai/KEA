# Proposal

## Why

KEA can already hear the user, read the app in front of them, and reach an LLM — but
only ever to transform text the user selected first. Every capability is locked behind a
hotkey and a selection. There is no way to ask KEA for something, and nothing it can do
on the user's behalf beyond putting words into a text field.

A spoken assistant closes that gap using primitives the project already owns: streaming
STT, accessibility and OCR reads of the focused app, app context, global hotkeys, the
LLM engine registry, and text-to-speech. The missing piece is not capture, inference, or
playback — it is a way to route what was said to an answer.

This change deliberately ships that routing **without** hands-free activation. A wake
phrase is the expensive part of a voice assistant — continuous capture, a permanently
lit system microphone indicator, battery draw, a second model, and false triggers — and
none of it improves the assistant's answers. Activating from a hotkey costs nothing new,
keeps the user's frontmost application frontmost (which is what makes "summarize this"
resolvable at all), and proves the routing before any of that is paid for. Hands-free
activation follows in `add-assistant-wake-word`.

## What Changes

- **Ask, and hear the answer.** The assistant is an answer agent first. Press a binding
  distinct from dictation's, ask a question, and get a short answer spoken aloud and
  shown on screen.
- **Answers are brief, because they are spoken.** A single short answer is delivered
  rather than a long one; the user asks a follow-up for more. Speech is linear and
  cannot be skimmed, so length is a correctness property here, not a style preference.
- **Spoken and shown, each switchable.** Speaking and displaying are independent, and
  both are on unless the user turns one off. Speech uses KEA's existing TTS engines and
  voice settings.
- **Hotkey activation.** A configurable binding, separate from dictation, rewrite, and
  the other hotkey commands. The microphone is held only for the duration of a session.
- **Tools in service of the answer.** The assistant can read what the user is looking at
  — the selection, the focused window, the screen via OCR — and can invoke a small set of
  application actions. An LLM returning structured JSON picks at most one, and answering
  directly is the default outcome rather than a routing failure.
- **Nothing the assistant does can be undone, because nothing it does needs undoing.**
  Every registry entry is free of side effects on the user's data. This is a flat
  constraint rather than a tiered permission model: a wrongly routed request costs the
  user an irrelevant answer, never their work.
- **Two-pass transcription for arguments.** The streaming hypothesis decides when the
  utterance ended and which action was meant; any action carrying argument values
  re-decodes the buffered audio with the offline engine first, because streaming models
  lose proper nouns first and those are exactly the tokens an argument cannot survive
  losing.
- Runs are recorded in the existing `actions` ledger so the user can see what the
  assistant did on their behalf.

Explicitly **not** in this change:
- **Wake-phrase activation.** Specified in `add-assistant-wake-word`, which depends on
  this change.
- **System voice-trigger integration (App Intents).** Registering KEA's actions with the
  platform would let the system assistant invoke them hands-free at no battery cost and
  with no microphone indicator. It is attractive and is sequenced after this change, but
  it is gated on an unresolved question — see `design.md`, Open Questions.
- Apple Events / Automation permission and AppleScript app control. Reading happens
  through Accessibility and OCR, which the user has already granted.
- Any action that changes the user's data. Sending, deleting, editing, and spending are
  out of scope, and the registry contract forbids them rather than gating them.
- Changing what the offline engine produces for dictation. Using the streaming hypothesis
  as dictation's final text is a separate question and a separate change.

## Capabilities

### New Capabilities

- `voice-assistant/session`: The assistant session lifecycle — how a session starts,
  live partial display, silence endpointing, answer delivery aloud and on screen,
  follow-up turns, and cancellation.
- `voice-assistant/intent-routing`: Turning an utterance into an answer or a chosen
  action with arguments, including answering as the default outcome, ambiguity, and the
  two-pass rule that argument values come from the offline transcript.
- `voice-assistant/action-registry`: The catalog of actions the assistant may invoke,
  their argument schemas, the rule that none of them may change or destroy anything, the
  disclosure owed when one reads another application, and the audit record every
  invocation leaves.

### Modified Capabilities

None. The project has no existing specs under `openspec/specs/`; this is its first
change, and the assistant adds behavior alongside dictation and rewrite rather than
altering them.

## Impact

**New code**
- An assistant feature in `crates/features/`, alongside `dictation.rs` and `rewrite.rs`.
- An action registry and dispatcher over the side-effect-free set: `read_focused`
  (selection, window, or OCR), `open_app`, `start_meeting`, and `rewrite_focused`.
  Answering is the default path and is not a registry entry.
- A popover window and UI surface in `ui/`, sibling to the dictation HUD overlay.

**Modified code**
- `crates/engines/src/traits.rs`: `LlmEngine::complete` is `String -> String` with no
  tool-calling. Routing uses prompt-and-parse JSON rather than provider tool APIs, so
  local and OpenAI-compatible backends route identically.
- Hotkey bindings and settings: an assistant binding distinct from every existing
  command, an assistant LLM binding, a TTS binding, and the speak/show output switches.

**Not modified**
- `crates/platform/src/audio/`. The assistant borrows the microphone for the length of a
  session, exactly as dictation does. The exclusive-borrow, burst-shaped `AudioIo`
  contract is sufficient and is left alone — this is the single largest simplification
  from deferring the wake phrase.

**Dependencies**
- The streaming recognizer is used for live partials and endpointing. Today `open()`
  returns `ModelNotInstalled` and nothing downloads it automatically; the assistant must
  degrade to a non-streaming session rather than requiring the model.
- Speech reuses the existing TTS feature, engines, and voice settings. An assistant with
  no usable speech engine still answers on screen and says it could not speak.

**Risk**
- Two probabilistic layers — mishearing, then misrouting — sit in front of every action.
  The side-effect-free registry is what makes that acceptable: the worst outcome of a
  wrong route is a window opened or an irrelevant answer spoken.
