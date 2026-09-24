# Proposal

## Why

`add-voice-assistant` ships the assistant behind an explicit hotkey: the user presses a
binding, speaks, and the assistant acts. That covers every capability the assistant has
except one — being able to use it without touching the keyboard.

This change adds the wake phrase. It is deliberately sequenced after the hotkey
assistant, because hands-free activation carries every cost the assistant has that the
hotkey does not: continuous microphone capture, a permanently lit system microphone
indicator, a battery draw, a second model to download, and false triggers. None of
those are worth paying before the routing underneath has been shown to work.

## What Changes

- A wake phrase (default `hi kea`) becomes a second way to start an assistant session,
  alongside the hotkey binding that `add-voice-assistant` established.
- Detection runs as a dedicated small keyword-spotting stage. The full streaming
  recognizer opens only after the trigger fires, mirroring the cascade every shipping
  voice assistant uses, and for the same reason: the always-on stage must be orders of
  magnitude cheaper than the stage it gates.
- Continuous listening is opt-in, off by default, visibly indicated, and suspendable
  without opening settings.
- Wake-word listening is the lowest-priority holder of the microphone and yields to
  dictation, meeting capture, and any other explicit recording.

Not in this change:
- Any change to routing, the action registry, or the risk tiers. This change adds an
  activation path and nothing else; everything downstream of activation is unchanged.
- Suppressing the macOS microphone indicator, which is not possible for a third-party
  application and is not a goal. See `add-voice-assistant/design.md`.

## Capabilities

### New Capabilities

- `voice-assistant/wake-word`: Detecting a spoken wake phrase from continuous audio and
  activating an assistant session, under the consent, disclosure, and
  microphone-arbitration constraints that always-on listening requires.

### Modified Capabilities

None. Activation by wake phrase is additive to the session lifecycle that
`add-voice-assistant` establishes; a session behaves identically however it was started.

## Impact

**Depends on** `add-voice-assistant`. The session, routing, and action-registry
capabilities must exist before there is anything for a wake phrase to activate.

**New code**
- A keyword-spotting stage over continuous microphone capture, and its model download.
- Settings for enabling listening and choosing the phrase.
- A persistent indicator with a one-click suspend.

**Modified code**
- `crates/platform/src/audio/`: continuous capture that yields to explicit capture.
  `AudioIo` is exclusive-borrow and burst-shaped today; the assistant under a hotkey
  borrows it exactly as dictation does, but a wake phrase needs a holder that persists
  between sessions and steps aside without failing.

**Costs accepted by this change**
- The macOS microphone indicator is lit whenever listening is enabled.
- An idle CPU and battery draw that must be measured on the oldest supported Mac.
- A keyword-spotting model download on enabling.
- False triggers, which open a microphone and begin a session the user did not ask for.
