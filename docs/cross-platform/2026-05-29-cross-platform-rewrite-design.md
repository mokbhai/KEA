# Cross-Platform Rewrite — Design

- **Date:** 2026-05-29
- **Status:** Approved design (pending implementation plan)
- **Author:** Vox maintainers
- **Supersedes:** the macOS-only Swift app (`VoxNative.xcodeproj`)

## 1. Summary

Vox is today a native macOS Swift app (~45 Swift files) built entirely on
Apple-only frameworks. This document specifies a **complete rewrite** as a
single cross-platform codebase that runs on **macOS, Windows, and Linux** and,
at the end of the final phase, reaches **full functional parity** with today's
shipping product.

The new app is built on **Tauri 2.x**: a **Rust** core/integration layer plus a
**web UI**, packaged as a small native binary per OS. The existing Swift app is
retired once the rewrite reaches parity.

The work is organized into **phases by build order**, not by feature reduction.
Every phase yields a working, launchable app. The features are not cut from
later phases — the last phase delivers remote + offline parity, working on all
three operating systems.

## 2. Goals / Non-Goals

### Goals

- One codebase, three OS targets (macOS, Windows, Linux), replacing the Swift app.
- Full parity at completion: AI text rewrite, speech-to-text (remote **and**
  offline), text-to-speech, presets/prompt catalog, global hotkeys, in-place
  text replacement, settings UI, secure credential storage, tray, permissions.
- Linux supported on **both X11 and Wayland**.
- Small footprint and a single maintainable codebase.

### Non-Goals

- Preserving any Swift code (clean rewrite; logic is reimplemented in Rust).
- Parakeet model support (Apple-CoreML-only; see Decision D1).
- Mobile (iOS/Android) targets.
- Streaming partial transcription in the first parity pass (parity matches the
  current product, which transcribes complete utterances).

## 3. Approved Decisions

- **D0 — Stack:** Tauri 2.x (Rust core + web UI). Rationale: every viable path
  rewrites the hard ~70% (OS integration + local inference) regardless; Tauri
  maximizes single-codebase reuse and maps 1:1 onto Vox's needs with mature
  crates, while keeping a small footprint vs. Electron or three native
  frontends.
- **D1 — Local model:** Standardize offline STT on **whisper.cpp** (via
  `whisper-rs`) on all platforms. **Drop Parakeet** (no portable runtime).
- **D2 — Linux display servers:** Support **both X11 and Wayland fully**.
  Wayland uses the XDG `org.freedesktop.portal.GlobalShortcuts` portal for
  hotkeys and `uinput`/evdev for synthetic input; X11 uses native APIs.
- **D3 — In-place replacement baseline:** **Clipboard + synthetic paste**
  (save clipboard → set rewritten text → synthesize Cmd/Ctrl+V → restore
  clipboard) on all OSes. macOS may keep an Accessibility-based insertion path
  as an enhancement.
- **D4 — Replace, don't co-maintain:** The unified Tauri app becomes the app on
  all three OSes; the Swift app is retired after parity is verified.

## 4. Architecture

Tauri 2.x app, single repository (Cargo workspace + web app):

```
vox/
├─ core/          Rust, platform-agnostic
│   ├─ rewrite/   providers (OpenAI, OpenAI-compatible), presets, prompt catalog, modes
│   ├─ speech/    remote transcriber, TTS synth, model registry/manager
│   ├─ settings/  settings schema + store (serde)
│   └─ secrets/   keyring abstraction
├─ platform/      Rust, per-OS behind traits + #[cfg(target_os)] / runtime detection
│   ├─ hotkeys    global hotkeys (global-hotkey; Wayland GlobalShortcuts portal)
│   ├─ textio     clipboard (arboard) + synthetic paste (enigo / uinput) + selection capture
│   ├─ audio      microphone capture (cpal) + audio playback
│   └─ system     tray, autostart, notifications, permissions
├─ infer/         Rust, local ASR via whisper-rs (whisper.cpp); GPU feature flags
├─ src-tauri/     Tauri shell: wires core+platform+infer into commands/events
└─ ui/            web app (Vite + React or Svelte): settings, speech overlay, tray menu
```

**Design principle:** the portable ~30% (rewrite, remote speech, TTS, settings,
secrets — today `Rewrite/*`, remote `Speech/*`, `Core/Storage/*`) is
reimplemented once in Rust. Each hard capability gets **one trait** with
OS-specific implementations selected by `cfg`/runtime detection, so consumers
depend on the interface, not the platform.

### Data flow (rewrite)

1. Global hotkey fires (`platform/hotkeys`).
2. Capture current selection (`platform/textio`: copy via synthetic Ctrl/Cmd+C
   or accessibility read).
3. `core/rewrite` builds the request (preset + prompt) and calls the provider
   (`reqwest`).
4. Result is written back in place (`platform/textio`: clipboard + synthetic
   paste, then clipboard restore).
5. UI overlay (`ui/`) shows progress/errors via Tauri events.

### Data flow (speech-to-text)

1. Push-to-talk hotkey starts mic capture (`platform/audio`, `cpal`).
2. Audio buffered to the selected engine:
   - **Remote:** stream/POST to OpenAI / OpenAI-compatible (`core/speech`).
   - **Offline:** feed to `infer/` (whisper.cpp) with the downloaded model.
3. Transcript inserted at cursor (`platform/textio`).
4. Speech overlay UI shows level meter + state via Tauri events.

## 5. Component Mapping (Swift → new)

| Today (Swift / Apple)                         | New (cross-platform)                                  |
|-----------------------------------------------|-------------------------------------------------------|
| `UI/Settings/*` SwiftUI (15 files)            | one web UI in `ui/`                                   |
| `MenuBarController` (NSStatusItem)            | Tauri tray                                            |
| `UI/Feedback/*` notch/overlay panels          | Tauri always-on-top webview overlay                   |
| Global hotkeys (Carbon/CGEvent)              | `global-hotkey`; Wayland GlobalShortcuts portal       |
| `PasteboardText*` + Accessibility replace     | `arboard` + `enigo` (synthetic paste); uinput/Wayland |
| `NSServices` context menu                     | macOS-only extra (Phase 5)                            |
| WhisperKit + Parakeet (CoreML)                | `whisper-rs` (whisper.cpp) — Parakeet dropped (D1)    |
| `RemoteSpeechTranscriber` (URLSession)        | `core/speech` remote (`reqwest`)                      |
| `OpenAIRewriteProvider` (URLSession)          | `core/rewrite` providers (`reqwest`)                  |
| `TextToSpeech` (OpenAI synth + AVFoundation)  | `core/speech` TTS synth + `platform/audio` playback   |
| `CredentialsStore` (Keychain)                 | `keyring` (Keychain / Credential Manager / libsecret) |
| `ApplicationSupportPaths`                     | Tauri `path` APIs + `core/settings`                   |
| `PresetStore` / `SettingsStore` / `PromptCatalog` | `core/settings` + `core/rewrite` (serde-backed)   |

## 6. Cross-Platform Integration Matrix

| Capability            | macOS                          | Windows                       | Linux (X11)            | Linux (Wayland)                         |
|-----------------------|--------------------------------|-------------------------------|------------------------|-----------------------------------------|
| Global hotkeys        | `global-hotkey` (Carbon)       | `global-hotkey` (RegisterHotKey) | `global-hotkey` (X11) | `GlobalShortcuts` portal (fallback uinput) |
| Clipboard             | `arboard`                      | `arboard`                     | `arboard`              | `arboard` (wl-clipboard backend)        |
| Synthetic paste/keys  | `enigo` (CGEvent)              | `enigo` (SendInput)           | `enigo` (XTest)        | `uinput`/evdev (ydotool-style)          |
| Mic capture           | `cpal` (CoreAudio)             | `cpal` (WASAPI)               | `cpal` (ALSA/Pulse)    | `cpal` (Pulse/Pipewire)                  |
| Audio playback        | `cpal`/`rodio`                 | `cpal`/`rodio`                | `cpal`/`rodio`         | `cpal`/`rodio`                           |
| Local ASR accel       | Metal                          | CUDA / Vulkan / CPU           | CUDA / Vulkan / CPU    | CUDA / Vulkan / CPU                      |
| Secrets               | Keychain                       | Credential Manager            | libsecret              | libsecret                                |
| Tray                  | Tauri                          | Tauri                         | Tauri (libappindicator) | Tauri (libappindicator)                |
| Autostart             | LaunchAgent                    | Registry Run key              | XDG autostart          | XDG autostart                            |

**Permissions to handle:** macOS Accessibility + Microphone prompts; Linux
`uinput` device access (group/udev rule) for Wayland synthetic input; Wayland
portal consent for global shortcuts.

## 7. Phased Build Order

Each phase ends with a working, launchable app. Features are not dropped from
later phases; the order is build sequence. The final phase reaches full parity.

### Phase 0 — Foundation
- Tauri 2.x scaffold; Cargo workspace (`core`, `platform`, `infer`, `src-tauri`).
- Web UI shell (`ui/`) with tray + empty settings window.
- `core/settings` store + `core/secrets` (keyring) working.
- CI matrix building runnable artifacts on macOS, Windows, Linux.
- **Outcome:** app launches and persists settings on all three OSes.

### Phase 1 — Rewrite (remote)
- `core/rewrite`: OpenAI + OpenAI-compatible providers, presets, prompt catalog,
  rewrite modes; settings UI for providers/keys/presets.
- `platform/hotkeys` + `platform/textio`: hotkey → capture selection → rewrite →
  in-place replace (clipboard + synthetic paste, D3) on all OSes incl. Wayland.
- **Outcome:** the core value proposition working on all three OSes.

### Phase 2 — Remote speech-to-text
- `platform/audio` mic capture; push-to-talk hotkey.
- `core/speech` remote transcription (OpenAI / OpenAI-compatible).
- Insert transcript at cursor; speech overlay UI (level meter + state).
- **Outcome:** dictation via hosted engines on all three OSes.

### Phase 3 — Local (offline) speech-to-text
- `infer/` whisper.cpp via `whisper-rs`; GPU feature flags (Metal / CUDA /
  Vulkan / CPU fallback).
- Model registry, download, storage, and management UI (replacing the WhisperKit
  download manager).
- **Outcome:** offline dictation on all three OSes.

### Phase 4 — Text-to-speech
- `core/speech` TTS synth (OpenAI) + `platform/audio` playback.
- Selection TTS flow + settings.
- **Outcome:** selection read-aloud on all three OSes.

### Phase 5 — Parity polish + distribution
- macOS extras: `NSServices` context-menu equivalent, notch/overlay polish,
  Accessibility insertion enhancement.
- Autostart, notifications, first-run permission flows per OS.
- Packaging & signing: macOS notarized `.dmg`/`.app`; Windows MSI + NSIS
  (code-signing cert required); Linux AppImage + `.deb`.
- Auto-update; migrate the release workflow (`.github/workflows/release.yml`,
  `scripts/*`) to the new build; updated docs; retire the Swift app.
- **Outcome:** full functional parity with today's product, working on all three
  OSes.

## 8. Definition of "Full Parity" (acceptance for Phase 5)

- Rewrite via global hotkey with in-place replacement — macOS, Windows, Linux
  (X11 + Wayland).
- Speech-to-text, both remote and offline (whisper.cpp) — all OSes.
- Text-to-speech for selected text — all OSes.
- Presets, prompt overrides, provider configuration, secure key storage — all OSes.
- Tray, settings UI, first-run permissions, autostart — all OSes.
- Signed/packaged installers per OS with auto-update.

## 9. Risks & Mitigations

- **Wayland synthetic input/hotkeys (D2):** highest-risk area; varies by
  compositor. Mitigation: prefer XDG portals; fall back to `uinput` with a
  documented udev rule; prototype on GNOME + KDE early in Phase 1.
- **whisper.cpp GPU build matrix:** CUDA/Vulkan/Metal build complexity in CI.
  Mitigation: ship CPU build as guaranteed baseline; enable GPU via feature
  flags and per-OS CI jobs.
- **Windows code-signing:** required to avoid SmartScreen warnings; needs a
  purchased cert. Mitigation: track as a Phase 5 procurement item; ship
  unsigned dev builds until then.
- **Clipboard-restore races (D3):** restoring the clipboard after paste can race
  with the target app. Mitigation: small post-paste delay + content verification;
  expose Accessibility path on macOS as the higher-fidelity option.
- **Model quality regression vs. CoreML WhisperKit/Parakeet:** whisper.cpp large
  models approximate but differ. Mitigation: offer model sizes up to large-v3;
  document the trade-off.

## 10. Testing Strategy

- **Rust unit tests** mirroring current `VOXTests`: rewrite service/providers,
  settings/preset stores, remote transcriber, TTS request building.
- **Integration smoke tests** per OS in CI: hotkey → capture → replace; mic →
  transcribe (remote + a tiny offline model).
- **Manual platform matrix** checklist for Phase 5 acceptance (X11 + Wayland
  compositors, Windows, macOS).

## 11. Open Items (deferred, not blocking)

- Exact web UI framework (React vs. Svelte) — decide in Phase 0.
- Whether to keep a macOS Accessibility insertion path or rely solely on D3.
- Windows code-signing certificate procurement.
