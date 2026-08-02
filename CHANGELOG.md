# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and the project follows Semantic Versioning.

## [Unreleased]

## [0.2.0] - 2026-08-03

### Added

- Floating dictation HUD overlay with a live waveform.
- Synthesized dictation cue sounds with an on/off setting.
- Rebuilt onboarding as a four-step setup wizard.
- Shared feature-page template across dictation, rewrite, meetings, and text-to-speech.
- Cancel control for in-flight model downloads, on both the engine picker and the Models page.
- Signed auto-update artifacts: releases now publish `latest.json` and a signed `.app.tar.gz` for `tauri-plugin-updater`.

### Changed

- Restyled the remaining application surfaces and completed an accessibility audit.
- Meeting transcription ends segments at a natural pause instead of on a fixed clock.
- Model downloads stream to disk instead of buffering, and the picker only offers engines the build actually ships.
- Release packaging builds the app once instead of building, cleaning, and rebuilding it.

### Fixed

- Added the missing Tauri v2 capability file. Without it the app resolved an empty ACL and rejected `plugin:event|listen`, so no `dictation:*`, `meeting:*`, or `model:download:*` event ever reached the frontend — which is what made a stalled model download unrecoverable.
- Model downloads recover from a dead connection: connect and per-read stall timeouts, a panic-catching wrapper so a task always emits a terminal event, and tracing at both boundaries.
- Calibrated palette tokens that failed WCAG 2.1 contrast in one or both themes, plus a regression test that reads the tokens straight out of `index.css`.
- Held modifier keys are released before the synthetic copy/paste during text injection.
- API keys are now actually persisted to the platform keychain.
- Provider connection tests report the underlying failure instead of a generic error.
- Capability defaults propagate resolver bindings correctly, and `delete_model` is hardened against unexpected input.
- Cross-platform CI: the Ubuntu keyring test skips where no credential store exists, and Windows pins the target to the static MSVC runtime (sherpa-onnx ships MT-only prebuilts, and whisper.cpp needs CMP0091 forced NEW before it honours the runtime variable).

### Security

- Updater artifacts are signed with an ed25519 key and verified by the client against the public key embedded in the app.

## [0.1.0] - 2026-07-17

### Added

- KEA cross-platform desktop app powered by Tauri, Rust, and React with a three-layer plugin architecture (engines, features, platform).
- Text rewrite feature with built-in modes, custom presets, provider configuration, and global hotkeys.
- Dictation with local Whisper models and OpenAI-compatible remote speech transcription.
- Meeting transcription with screen-capture audio, segmented transcripts, and synthesized notes and titles.
- Text-to-speech with OpenAI TTS integration, local playback, and read-aloud support.
- Light/dark theme system with persistent preferences and per-feature configuration pages.
- History and Logs pages for reviewing past transactions and runtime diagnostics.
- Model management for downloading and switching local Whisper speech models.
- Hotkey binding interface with key-capture recording for global shortcuts.
- KEA-first Makefile, documentation, and release tooling.
- Model download progress events delivered to the UI.

### Changed

- Replaced the retired Python/PyObjC runtime with the KEA Tauri/Rust/React cross-platform stack.
- Moved configuration and credential storage to SQLite and the platform keychain.

### Fixed

- Fixed hotkey rebinding so Cmd modifier assertions work on non-macOS platforms.
- Added Windows icon asset required by Tauri bundling.
- Fixed UI invoke argument keys to use camelCase for Tauri command marshalling.
- Fixed sidebar layout to scroll independently from the main content area.

### Security

## [2.1.1] - 2026-05-29

### Changed

- Resolved the WhisperKit tokenizer from the downloaded model folder instead of a dedicated tokenizer subdirectory.
- Added native pre-commit checks to guard local commits.

### Fixed

- Surfaced rewrite provider HTTP errors so failed rewrites report the underlying status instead of a generic failure.

## [2.1.0] - 2026-05-11

### Added

- Added selection text-to-speech support with provider configuration, credential storage, and settings controls.

### Changed

- Refined speech hotkey handling and notch status feedback so recording and speech state transitions behave more consistently.
- Stabilized the local CI and release packaging workflow.

### Fixed

- Fixed prompt override editing so changes persist while typing.

### Security

## [2.0.2] - 2026-04-21

### Added

### Changed

### Fixed

- Fixed the `Ask Vox` service prompt so Xcode 16.4 no longer rejects its AppKit alert flow for crossing the main actor boundary during release builds.
- Pinned the `FluidAudio` package reference to the verified revision used in local checks so release builds stop drifting with the upstream `main` branch.

### Security

## [2.0.1] - 2026-04-21

### Added

### Changed

### Fixed

- Fixed the native CI and release pipelines so builds and tests run against the supported macOS 15 deployment target instead of requiring macOS 26 runners.

### Security

## [2.0.0] - 2026-04-17

### Added

- Added automatic startup routing to `Settings > Permissions` in the native app on first launch and whenever Accessibility or Microphone access is missing.

### Changed

- Dropped Python and PyObjC support from the repository so VOX now ships, builds, tests, and releases as a Swift-only macOS app.

### Fixed

### Security

## [1.4.10] - 2026-04-14

### Added

- Added a speech provider toggle so dictation can use either local Whisper models or a remote OpenAI-compatible transcription API.
- Added speech-specific remote configuration fields for base URL, API key, and model selection in Preferences.

### Changed

- Defaulted remote speech transcription to `gpt-4o-transcribe` and kept rewrite-provider settings separate from speech-provider settings.

### Fixed

- Fixed remote speech transcription against Codex-LB style backends by routing `backend-api` URLs to the correct `/transcribe` endpoint and handling legacy saved model names.

### Security

## [1.4.9] - 2026-04-09

### Added

### Changed

### Fixed

- Fixed the merged typecheck regression test file so CI linting and test gates pass on `main`.

### Security

## [1.4.8] - 2026-04-09

### Added

- Added macOS text-to-speech output helpers and voice assistant configuration defaults.
- Added wake word detection support with an OpenWakeWord-backed engine abstraction.

### Changed

- Improved popover and recording toast overlays so they follow the active screen and fullscreen spaces more reliably.
- Capped retained log lines in the app log file to keep runtime logs from growing without bound.

### Fixed

- Fixed wake word PCM conversion and model loading so OpenWakeWord integration receives the expected data.
- Fixed text-to-speech result handling so speech synthesis failures can surface correctly.
- Fixed release-blocking mypy regressions in the text-to-speech adapter and logging handler stream management.

### Security

## [1.4.7] - 2026-03-26

### Fixed

- Fixed type mismatch in RecordingToast wave animation where height variable was assigned both int and float values.

## [1.4.6] - 2026-03-25

### Added

- Simplified popover to single scrollable row with post-rewrite state display.

### Changed

- Improved RecordingToast with compact size and wave visualization.

### Fixed

- Fixed popover_on_selection setting not persisting in preferences.

## [1.4.5] - 2026-03-24

### Added

- Added centralized prompts module (`vox/prompts.py`) for all system prompts to improve code organization and maintainability.
- Added `AUDIO_REFINEMENT_PROMPT` constant for speech-to-text cleanup based on OpenWhispr's production-tested prompt.

### Changed

- Refactored system prompts from `vox/api.py` into dedicated `vox/prompts.py` module for better separation of concerns.
- Updated `vox/ui.py` and `vox/preferences.py` to use `get_prompt()` function from prompts module.

### Fixed

- Fixed circular import issue between `vox/api.py` and `vox/prompts.py` by using string literals for dictionary keys instead of enum values.
- Added `RewriteModePromptsDict` wrapper class to support both enum and string key access for backward compatibility in tests.
- Fixed 6 test cases in `tests/test_api.py` that had incorrect prompt retrieval patterns.

## [1.4.4] - 2026-03-24

### Added

- Added `set-version` Makefile target for convenient version updates across all files.

### Changed

### Fixed

### Security

## [1.4.3] - 2026-03-24

### Added

- Added version management documentation to CLAUDE.md.

### Changed

### Fixed

- Fixed variable redefinition error in speech.py that was causing ruff to fail.

### Security

## [1.4.2] - 2026-03-24

### Added

- Added real-time transcription progress indicator with progress bar during speech-to-text processing.

### Changed

- Increased audio level VU meter sensitivity for better microphone feedback (level bar now reaches higher percentages at normal speaking volume).

### Fixed

- Fixed lambda closure issues in progress callbacks that could cause incorrect callback reference capture during transcription.
- Fixed subprocess worker detection to use `sys.executable` instead of `shutil.which("python")` for better frozen app compatibility.
- Fixed recording toast visibility state during speech refinement - toast now properly hides after AI enhancement completes or fails.
- Fixed missing error handling for hotkey text selection failures.

## [1.4.1] - 2026-03-24

### Added

- Added toggle mode for speech-to-text: quick tap of the hotkey keeps recording active, tap again to transcribe.

### Changed

- Clarified speech-to-text documentation to distinguish between toggle mode and hold mode behavior.

### Fixed

- Fixed speech hotkey behavior so quick taps don't prematurely stop recording (proper toggle support).
- Updated tests to reflect new toggle mode behavior for speech hotkeys.

### Security

## [1.4.0] - 2026-03-24

### Added

- Added streaming speech transcription with chunked processing and voice activity detection to improve live dictation handling.
- Added a speech auto-pass threshold preference so auto-formatting can wait for longer transcripts before rewriting.
- Added platform abstraction layers and Linux implementations for preferences, clipboard, notifications, hotkeys, and app shell support.

### Changed

- Improved dictation paste behavior and speech settings handling across the speech flow.
- Switched configuration and secret storage to platform-specific adapters so API credentials and app data use the active OS integration.

### Fixed

- Fixed speech-to-text packaging issues that could break release builds or runtime setup.
- Fixed API key persistence fallback behavior when secure storage is unavailable.

### Security

## [1.3.1] - 2026-03-19

### Added

### Changed

### Fixed

- Fixed the About preferences page so its full content is reachable with the existing preferences scroll view.

### Security

## [1.3.0] - 2026-03-19

### Added

- Added reusable named rewrite presets including Make assertive, Turn into bullet points, Translate to Hindi, and Slack tone.
- Added preset actions to the selection popover so text can be rewritten directly from reusable instructions.
- Added a Speech preference to restore the previous clipboard contents after Vox pastes a transcript or auto-formatted result.
- Added GitHub repository and release links to the About preferences page.

### Changed

- Refactored selection copy and paste handling into a shared pasteboard helper module used by rewrite and speech flows.
- Updated the popover layout to support a richer grid of built-in modes and reusable preset actions.

### Fixed

- Speech transcription and speech auto-formatting now preserve the user’s prior clipboard item after pasting generated text.
- Pasteboard snapshots now clone clipboard items safely, fixing macOS errors when archiving clipboard contents across pasteboards.

## [1.2.1] - 2026-03-19

### Added

- Added background rotating file logging for runtime errors under `~/Library/Application Support/Vox/logs/vox.log`.
- Added an Apache 2.0 `LICENSE` file and project metadata declaring the repository license.
- Added test coverage for the shared logging utilities.

### Changed

- Replaced noisy console debug output with structured application logging across startup, services, hotkeys, selection monitoring, updates, speech, notifications, and config persistence.
- Documented the new runtime log location in the README and development docs to make troubleshooting easier.

### Fixed

- Uncaught main-thread and background-thread exceptions are now captured in the app log, improving diagnosis of service and worker-thread failures.

## [1.2.0] - 2026-03-19

### Added

- Added a new Prompts preferences page so each built-in rewrite mode can use a user-editable system prompt override.
- Added app update support with manual "Check for Updates" actions, a release downloader, and install-on-relaunch flow for packaged Vox app builds.
- Added an automatic update preference that checks for newer GitHub Releases on launch.
- Added test coverage for prompt overrides, update checks, downloads, and installer preconditions.

### Changed

- Reworked rewrite prompt construction to wrap source text in a structured payload so the model treats the selection or transcript as text to transform, not instructions to follow.
- Expanded preset rewrite prompts to be more explicit about preserving meaning and obeying the selected rewrite mode.
- Extended the About preferences page with update controls and moved prompt customization into its own dedicated page.

### Fixed

- Fixed cases where Fix Grammar and other preset rewrites could drift into the domain implied by the input text instead of applying the selected rewrite instruction.
- Fixed prompt overrides so they now apply consistently across services, hotkeys, the popover, and speech auto-formatting.

## [1.1.0] - 2026-03-19

### Added

- Added an optional speech auto-formatting flow that sends transcribed text through a selected rewrite mode before it is pasted.
- Added speech preferences for enabling auto-formatting and choosing which rewrite mode to apply after transcription.
- Added test coverage for speech auto-format configuration and post-transcription behavior.

### Changed

- Updated GitHub Actions workflow dependencies to current Node 24-ready action majors.

### Fixed

- Invalid stored speech auto-format modes now fall back safely to Fix Grammar instead of failing during transcription completion.

## [1.0.0] - 2026-03-19

### Added

- Initial stable release of Vox for macOS.
- Context-menu rewriting in any app with built-in modes for Improve, Fix Grammar, Professional, Concise, Friendly, and custom "Ask Vox..." prompts.
- Global rewrite hotkeys and menu bar preferences for configuring models, shortcuts, launch-at-login, and API connectivity.
- Offline speech-to-text powered by local Whisper models, including downloadable model management, microphone input monitoring, and configurable speech hotkeys.

### Changed

- API keys are stored securely in the macOS Keychain instead of the config file.
- Release packaging now produces signed `.app`, `.zip`, and `.dmg` artifacts suitable for GitHub Releases.

### Fixed

- Migration and persistence edge cases around API key storage and preference updates.
- Hotkey, popover, and text replacement flows for rewrite and speech interactions across supported macOS apps.

## [0.1.4] - 2026-03-19

### Added

- Speech-to-text support powered by local Whisper models.
- "Improve" and "Ask Vox..." rewrite flows.
- Additional preferences coverage for speech, selection monitoring, and UI behavior.

### Changed

- API keys are now stored in the macOS Keychain instead of the config file.
- Hotkey and popover handling were refined for speech and rewrite flows.

### Fixed

- Several migration and persistence edge cases around API key storage.
