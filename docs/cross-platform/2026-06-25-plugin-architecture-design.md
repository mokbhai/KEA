# KEA — Cross-Platform Plugin Architecture Design

- **Product name:** **KEA** (rename from Vox; the rename lands in Phase 0)
- **Date:** 2026-06-25
- **Status:** Approved design (pending implementation plan)
- **Author:** KEA maintainers
- **Supersedes:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md`
  (and reframes its Phase 0–5 plans around a plugin architecture)

## 1. Summary

**KEA** is the new name for the product currently shipping as **Vox** — today a
native macOS Swift app (`VoxNative.xcodeproj`) built on Apple-only frameworks.
This document specifies a **complete rewrite** as a single cross-platform
codebase running on **macOS, Windows, and Linux (X11 + Wayland)**, reaching
**full functional parity** with today's product.

The defining property of the new system is that **everything is a plugin**,
organized into three layers:

1. **Feature plugins** — the user-facing capabilities (Rewrite, Dictation,
   Meetings/Granola-like). Self-contained units composed on top of engines.
2. **Engine plugins** — capability providers (LLM, STT, TTS). Pluggable,
   addable at any time, selected by the user.
3. **Platform providers** — OS integration (hotkeys, text I/O, audio,
   permissions, tray, secrets). Trait-based modules; the OS selects the impl.

The app is built on **Tauri 2.x**: a **Rust** core + a **web UI**, packaged as a
small native binary per OS. The Swift app is retired once parity is verified.

This design **reuses the still-valid decisions** from the 2026-05-29 design
(Tauri stack, clipboard+synthetic-paste replacement, keyring secrets, Wayland
portals, phased build order) and **extends it**: a first-class plugin
architecture across all three layers, Parakeet reinstated as a cross-platform
STT engine (the prior design dropped it), a SQLite-backed config + operational
data layer, and a local TTS engine.

## 2. Goals / Non-Goals

### Goals

- One codebase, three OS targets (macOS, Windows, Linux), replacing the Swift app.
- A three-layer plugin architecture where features, engines, and platform
  providers are modules behind stable traits in registries.
- **Per-feature, per-slot customization:** the user binds a concrete
  engine+model+provider into each capability slot of each feature, independently.
- Full parity at completion: Rewrite, Dictation, Meetings; remote **and** offline
  STT; TTS; presets/prompt catalog; global hotkeys; in-place replacement;
  settings UI; secure credential storage; tray; permissions.
- Linux supported on **both X11 and Wayland**.
- Small footprint: heavy/optional engines gate behind cargo features.

### Non-Goals

- Preserving any Swift code (clean rewrite; logic reimplemented in Rust).
- Dynamic/third-party plugin loading (dylib/WASM/marketplace). Plugins are
  **compiled in**; "plugin" means a strict trait + registry contract, not
  runtime code loading. (Revisit post-parity if needed.)
- Mobile (iOS/Android) targets.
- Streaming partial transcription in the first parity pass.

## 3. Approved Decisions

- **D0 — Stack:** Tauri 2.x (Rust core + web UI). _(Carried from 2026-05-29.)_
- **D1 — Plugin model:** Internal **trait + registry**, compiled in. No dynamic
  loading. Three registries (Feature / Engine / Platform). Adding a plugin =
  add a module + one `register()` call.
- **D2 — Three layers, not one flat plugin type:** Features, Engines, and
  Platform providers have different shapes and different selection mechanisms
  (user-enabled features; user-selected engines per slot; OS-selected platform
  providers). They are kept as distinct trait families on purpose.
- **D3 — Per-slot engine binding:** Features declare capability slots; the user
  binds a concrete engine to each slot. Engine selection is per-feature, not
  global. New engines become selectable in all compatible slots with no feature
  changes.
- **D4 — In-place replacement baseline:** Clipboard + synthetic paste on all
  OSes (save → set → synthesize Cmd/Ctrl+V → restore). macOS also keeps an
  Accessibility insertion path as an enhancement (see D12). _(Carried from 2026-05-29 D3.)_
- **D5 — Offline Whisper** via `whisper-rs` (whisper.cpp) on all platforms.
  _(Carried.)_
- **D6 — Parakeet reinstated** as a cross-platform STT engine via **`sherpa-onnx`**
  (runs NVIDIA NeMo Parakeet transducer models on ONNX Runtime; Rust bindings;
  macOS/Windows/Linux). `ort` (raw ONNX) remains a documented fallback if a
  sherpa-onnx integration issue arises. Integration prototyped early in Phase 4.
  _(Reverses 2026-05-29 D1.)_
- **D7 — Linux display servers:** Support **both X11 and Wayland fully**. Wayland
  uses the XDG `org.freedesktop.portal.GlobalShortcuts` portal for hotkeys and
  `uinput`/evdev for synthetic input; X11 uses native APIs. _(Carried.)_
- **D8 — Replace, don't co-maintain:** The unified Tauri app becomes the app on
  all three OSes; the Swift app is retired after parity. _(Carried.)_
- **D9 — Config in SQLite, secrets in keyring:** Settings, presets, prompt
  catalog, and bindings live in `config.db` (SQLite via `sqlx`), not serde files
  — for atomic writes, migrations, and safe concurrent access. Operational data
  lives in a separate `data.db`. **Credentials stay in the OS keyring for now**
  (no DB credential storage / encryption yet), behind a `CredentialStore`
  abstraction so an encrypted-DB option can be added later without caller
  changes. Plaintext keys in a DB file are explicitly avoided.
- **D10 — Web UI: React** (Vite + React). _(Resolves the prior open item.)_
- **D11 — Local TTS via `sherpa-onnx`:** the offline TTS engine reuses
  `sherpa-onnx` (VITS / Piper / Kokoro models on ONNX Runtime) — the **same
  library as the Parakeet STT engine (D6)**, so one ONNX runtime powers both
  local STT and local TTS. Ships alongside the OpenAI TTS engine. (Piper direct
  is the documented alternative.)
- **D12 — Keep macOS Accessibility insertion** as a higher-fidelity enhancement
  alongside the D4 clipboard+paste baseline (Phase 4), not a replacement for it.
- **D13 — KEA name is fixed** regardless of `kea` binary/package collisions
  (ISC Kea DHCP, k8s cluster-autoscaler); namespacing handled at packaging.
- **D14 — Meeting audio capture:** the Meetings feature captures **both mic and
  system/loopback audio** (the other participants), mixed before transcription.
  This is OS-specific (see §8 / Risks) and is the Meetings feature's critical
  platform dependency.

## 4. Architecture

Cargo workspace + web UI, single repository:

```
kea/
├─ crates/
│  ├─ core/           settings (config.db), secrets abstraction, slot resolution,
│  │                  orchestration (rewrite & speech flows), Tauri event bus,
│  │                  store/ (SQLite: config.db + data.db), log/ (tracing)
│  ├─ engines/        registry + trait defs; engine impls behind cargo features
│  │   ├─ llm/        openai, openai-compatible
│  │   ├─ stt/        openai-api, whisper (whisper-rs), [parakeet/sherpa-onnx — Phase 4]
│  │   └─ tts/        [openai-tts + local sherpa-onnx — Phase 4]
│  ├─ features/       feature plugins (rewrite, dictation, meetings) + FeatureRegistry
│  ├─ platform/       hotkeys, textio, audio, permissions, tray, autostart, secrets
│  │                  (per-OS behind traits; runtime detect X11 vs Wayland)
│  └─ infer/          shared local-inference plumbing for whisper.cpp + ONNX
│                     Runtime: model registry/download, GPU flags (Metal/CUDA/Vulkan/CPU)
├─ src-tauri/         thin wiring: core + engines + features + platform -> commands/events
└─ ui/                web UI (settings, speech overlay, tray menu)
```

**Design principle:** consumers depend on traits, never on a concrete engine or
a platform. `core` orchestrates flows and owns the event bus; `src-tauri` is
thin (commands in, events out); `ui/` is presentation only.

### 4.1 Layer 1 — Engine plugins

One trait per capability; each engine is a module that registers itself under a
stable string id.

```rust
trait SttEngine { fn id(&self) -> &str; fn capabilities(&self) -> EngineCaps;
                  async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript>; }
trait TtsEngine { fn id(&self) -> &str;
                  async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm>; }
trait LlmEngine { fn id(&self) -> &str;
                  async fn complete(&self, req: LlmRequest) -> Result<LlmResponse>; }

registry.register_stt(OpenAiSttEngine);   registry.register_stt(WhisperEngine);
registry.register_llm(OpenAiLlmEngine);   // hosted + compatible base-url
```

`LlmEngine` exposes a single general `complete` call; the Rewrite and Meetings
features build their specific prompts (rewrite vs. note/title synthesis) on top
of it. `SttEngine.transcribe` handles a **bounded** audio buffer; long-audio
features (Meetings) segment the stream and call `transcribe` per chunk —
streaming partial transcription remains a non-goal for now (§2).

Engines have no UI of their own. Heavy engines (local Whisper, Parakeet, local
TTS — all pulling whisper.cpp / ONNX Runtime) gate behind cargo features so a
build ships only what it includes.

### 4.2 Layer 2 — Platform providers

One trait per OS-integration capability; a single implementation chosen by
`#[cfg(target_os)]` plus runtime detection (Linux X11 vs Wayland). These are
plugins in *construction* but not in *selection* — the OS picks, not the user.

```rust
trait Hotkeys     { fn register(&self, binding: Binding, action: ActionId); /* ... */ }
trait TextIo      { async fn capture_selection(&self) -> Result<String>;
                    async fn replace(&self, text: &str) -> Result<()>; }
trait AudioIo     { async fn capture_mic(&self) -> Result<AudioStream>;
                    async fn capture_system(&self) -> Result<AudioStream>; // loopback, for Meetings (D14)
                    async fn play(&self, pcm: AudioPcm) -> Result<()>; }
trait Permissions { fn status(&self, kind: PermKind) -> PermStatus;
                    async fn request(&self, kind: PermKind) -> Result<PermStatus>; }
```

### 4.3 Layer 3 — Feature plugins

User-facing capabilities, composed on top of engines + platform. Each feature is
self-contained: it declares the capability slots it needs, brings its own
settings schema, UI surface, commands (hotkey-bindable), and lifecycle.

```rust
trait Feature {
    fn id(&self) -> &str;                  // "rewrite" | "dictation" | "meetings"
    fn required_caps(&self) -> &[CapSlot]; // e.g. meetings -> [stt, llm]
    fn settings_schema(&self) -> Schema;   // feature-owned config
    fn commands(&self) -> Vec<Command>;    // hotkey-bindable actions
    async fn run(&self, ctx: FeatureCtx);  // ctx resolves slots -> concrete engines
}
```

### 4.4 Slot resolution (the customization core)

A feature never names a concrete engine. It asks its `FeatureCtx` for a
capability; the context looks up the user's binding for that feature+slot and
returns the registered engine.

```rust
// Meetings feature, mid-run:
let stt = ctx.engine::<dyn SttEngine>("stt")?;   // -> whisper(large-v3), per user binding
let llm = ctx.engine::<dyn LlmEngine>("llm")?;   // -> openai(gpt-…),     per user binding
```

Bindings persist in `config.db` as `feature_id -> { slot -> { engine_id, model,
provider_ref } }`. Resolution rules:

- Unbound slot, exactly one compatible engine → **auto-bind**.
- Unbound slot, multiple compatible engines → **prompt** the user.
- Bound to an incompatible/missing engine → **error** surfaced in UI.
- New engine registered later → instantly offered in every compatible slot, no
  feature code changes.

Example bindings:

```
Meetings  : stt = whisper(large-v3),  llm = openai(gpt-…)
Dictation : stt = parakeet,           (post-process llm = local-compatible)
Rewrite   : llm = local-compatible(ollama, my-model)
```

## 5. Data Flows

Reuses the proven platform decisions (D4 clipboard+paste, keyring secrets,
D7 Wayland portals).

### Rewrite

1. Global hotkey fires (`platform/hotkeys`).
2. Capture current selection (`platform/textio`).
3. Feature builds the request from preset + prompt.
4. Resolved `LlmEngine.complete()` (with the rewrite prompt) produces the replacement.
5. Write back in place (`platform/textio`: save clipboard → set text →
   synthesize Cmd/Ctrl+V → restore clipboard).
6. Overlay progress/errors via Tauri events.

### Dictation (speech-to-text)

1. Push-to-talk hotkey starts mic capture (`platform/audio`).
2. Audio buffered to the resolved `SttEngine` (OpenAI-API or local Whisper).
3. Transcript inserted at cursor (`platform/textio`).
4. Speech overlay shows level meter + state via Tauri events.

### Meetings (Granola-like)

1. Start capture — mic **and** system/loopback audio (D14), mixed → buffered.
2. Segmented transcription via resolved `SttEngine` (the feature chunks long
   audio and calls `transcribe` per segment).
3. AI synthesis (notes + title) via resolved `LlmEngine.complete`, matching
   today's end-meeting synthesis.
4. Meeting record persisted via `core/store` (`data.db`).
5. UI panel shows transcript + synthesized notes.

## 6. Data, Persistence & Observability

Four tiers, each matched to the kind of data; everything stays local on the
device (only what an engine sends to its provider leaves).

**System-of-record rule (the boundary, stated so it can't drift):**

- **`config.db` = _how the app is set up_** — settings, presets, prompt catalog,
  and feature→slot→engine bindings.
- **`data.db` = _what happened_** — operational data features produce (actions,
  conversations, meeting transcripts/notes, runtime feature state). The
  "features + LLM" data.
- **Keyring = _secrets_** — API keys / credentials. **Never in either DB, never
  in a file.**

Two databases, not one, because they have different lifecycles: `config.db` is
small and critical (backed up / exported as a unit); `data.db` is large and
churny ("clear history" truncates it without touching config).

The DBs store **references, not secrets**: an action/conversation row records
`engine_id`, model name, and a logical `provider_ref` — never the API key or
provider base URL (those resolve from `config.db` + keyring). A *binding* lives
in `config.db`; a *usage* of that binding is a `data.db` record.

### 6.1 Configuration (`config.db`)

- Settings, presets, prompt catalog, and feature/slot bindings live in a SQLite
  database (`config.db`) via `sqlx` with versioned migrations — replacing loose
  serde files. This buys atomic writes, safe concurrent access across the main
  window / tray / overlay, and schema evolution, instead of hand-rolled file
  parsing. Owned by `core/store`, accessed through typed repositories.

### 6.2 Secrets (OS keyring)

- Provider credentials in the OS keyring (`keyring`: Keychain / Credential
  Manager / libsecret), behind a `CredentialStore` abstraction.
- **This phase keeps credentials in the keyring; DB credential storage and
  encryption are deferred.** If we later want them in `config.db`, the
  abstraction lets us add an encrypted credentials table (master key held in the
  keyring as the root of trust) with no caller changes. Plaintext keys in a DB
  file are explicitly avoided.

### 6.3 Operational database (`data.db`)

- **Engine:** SQLite via `sqlx` (async, compile-time-checked queries) with
  versioned migrations run on startup. Embedded, cross-platform, no server.
  File in the app data dir (`kea/data.db`, via Tauri `path` APIs; separate from
  `config.db`).
- **Ownership:** `core/store`. Features access it through typed repositories,
  never raw SQL. Each feature's tables are namespaced; a shared `actions` log
  spans all features.
- **Core tables:**
  - `actions` — one row per user action: feature_id, command, engine_id, model,
    provider_ref, started_at, finished_at, status, error, token/char counts.
    The audit trail powering the History view and "re-run."
  - `conversations` + `messages` — LLM interaction records (rewrite
    request/response, and any multi-turn): role, content, engine/model, tokens.
  - `meetings` + `meeting_segments` + `meeting_notes` — meeting transcripts and
    synthesized notes/titles (replaces today's meeting storage).
  - `feature_data` — generic per-feature key/value state.
- **Content & retention (default: store content):** full text of
  actions/conversations/transcripts is stored by default so History,
  conversations, and meetings are useful and re-runnable. Controls:
  per-feature "don't store content" toggle, a global retention setting
  (auto-prune older than N days), and "clear history."

### 6.4 Logging (tracing)

- `core/log` configures `tracing` + `tracing-subscriber` with a rotating file
  appender in the platform log directory (resolved via Tauri `path` APIs; the
  macOS location matches today's), plus stderr in dev builds.
- Log level configurable in Settings; structured spans around feature runs,
  engine calls, and platform operations.
- A Logs view in the UI tails the current file; an "open log folder" action is
  provided.

## 7. UI Architecture

The UI **reflects** the registered features and engines but is **composed from a
shared component library**, not auto-generated. Each top-level page and feature
surface is an explicitly authored React page assembled from standard
components. Because those components carry well-defined contracts (matching the
Tauri command layer), per-page authoring stays light and any page can be
extended with bespoke UI when the standard kit is not enough.

Rust feature plugins are compiled-in and cannot ship UI code, so the web app —
not the Rust plugin — provides the components and pages; the plugin layer is
reached only through the typed Tauri command/event contracts.

### 7.1 Component library (the "standard outputs")

Reusable components wrap the backend contracts so each works against the
registries with no per-page glue:

- `SlotBinder` / `ModelSelector` — renders a feature's capability slot as a
  picker over compatible registered engines + their models (reads `list_engines`,
  writes `set_binding`). Adding an engine appears here automatically.
- `EngineConfig` / `CredentialField` — provider base URL, API key (keyring),
  model list for an engine.
- `SettingsForm` — renders a feature/app `settings_schema()` as fields and
  validates via `set_settings`.
- `HotkeyBinder` — binds a key combo to a feature command, with conflict detection.
- `ModelManager` — local model download / progress / storage (Whisper; later Parakeet).
- Overlay / feedback primitives — `LevelMeter`, `StatusPill`, `TranscriptPanel`.

### 7.2 Pages (composed from the library)

| Page | Layer it surfaces | Contents |
|------|-------------------|----------|
| **Configuration** | Engine plugins | Provider credentials, base URLs, model management (`EngineConfig`, `CredentialField`, `ModelManager`) |
| **Features** | Feature plugins | Per feature: enable/disable, bind an engine to each slot (`SlotBinder`), feature settings (`SettingsForm`), command hotkeys (`HotkeyBinder`). Where per-feature/per-slot customization lives. |
| **Settings** | App / Platform providers | Permissions, autostart, tray, updates, appearance, global hotkey conflicts, logging level, history/retention controls |
| **History / Activity** | `core/store` | Past actions + conversations (content-aware): search, inspect, re-run, clear/prune |
| **Logs** | `core/log` | Tails the current log file; open-log-folder action |
| **Feature surfaces** | Feature plugins | Rich views a feature needs — Dictation overlay, Meetings transcript/notes panel — composed from overlay primitives and extended with feature-specific UI |

The three top-level pages map cleanly onto the three plugin layers
(Configuration ↔ engines, Features ↔ features, Settings ↔ app/platform).

### 7.3 Authoring model

A new feature gets a small page composed from the library (modest rework) and
can be **extended** with bespoke components on that page when it needs more than
the standard kit. The page is the unit of customization; the component library
keeps the cost low and the contracts consistent. A schema-only feature needs no
new component work beyond composing existing ones; a feature with a rich surface
adds its own view, keyed by feature id.

## 8. Cross-Platform Integration Matrix

| Capability           | macOS                     | Windows                          | Linux (X11)           | Linux (Wayland)                            |
|----------------------|---------------------------|----------------------------------|-----------------------|--------------------------------------------|
| Global hotkeys       | `global-hotkey` (Carbon)  | `global-hotkey` (RegisterHotKey) | `global-hotkey` (X11) | `GlobalShortcuts` portal (fallback uinput) |
| Clipboard            | `arboard`                 | `arboard`                        | `arboard`             | `arboard` (wl-clipboard backend)           |
| Synthetic paste/keys | `enigo` (CGEvent)         | `enigo` (SendInput)              | `enigo` (XTest)       | `uinput`/evdev                             |
| Mic capture          | `cpal` (CoreAudio)        | `cpal` (WASAPI)                  | `cpal` (ALSA/Pulse)   | `cpal` (Pulse/Pipewire)                    |
| System/loopback audio (Meetings) | ScreenCaptureKit / aggregate device (+ Screen Recording perm) | WASAPI loopback | Pulse/PipeWire monitor source | Pulse/PipeWire monitor source |
| Audio playback       | `cpal`/`rodio`            | `cpal`/`rodio`                   | `cpal`/`rodio`        | `cpal`/`rodio`                             |
| Local ASR accel      | Metal                     | CUDA / Vulkan / CPU              | CUDA / Vulkan / CPU   | CUDA / Vulkan / CPU                        |
| Secrets              | Keychain                  | Credential Manager               | libsecret             | libsecret                                  |
| Tray                 | Tauri                     | Tauri                            | Tauri (appindicator)  | Tauri (appindicator)                       |
| Autostart            | LaunchAgent               | Registry Run key                 | XDG autostart         | XDG autostart                              |

**Permissions to handle:** macOS Accessibility + Microphone + **Screen Recording**
(system-audio capture for Meetings); Linux `uinput` access (group/udev rule) for
Wayland synthetic input; Wayland portal consent for global shortcuts.

## 9. Phased Build Order

Each phase ends in a working, launchable app. Order is build sequence, not
feature reduction; the final phase reaches full parity.

### Phase 0 — Foundation + plugin framework
- Tauri 2.x scaffold; Cargo workspace (`core`, `engines`, `features`,
  `platform`, `infer`).
- **Rename Vox → KEA:** new repo/workspace identity, bundle ids, binary name,
  and docs land here, on the fresh scaffold (the old Swift app keeps its name
  until it is retired in Phase 4).
- Three registries (Feature / Engine / Platform), slot-binding, and keyring
  secrets (behind `CredentialStore`).
- `core/store`: two SQLite DBs via `sqlx` migrations — `config.db` (settings,
  presets, bindings) and `data.db` (`actions`, `conversations`/`messages`) — plus
  `core/log` (tracing + rotating files), so every feature persists config and
  records actions/logs from day one. Meeting tables arrive in Phase 3.
- Web UI shell (Vite + React, D10) + tray; one trivial feature exercising the
  framework end-to-end.
- CI matrix building runnable artifacts on macOS, Windows, Linux.
- **Outcome:** app launches, persists settings, and resolves a slot on all three
  OSes.

### Phase 1 — Rewrite feature
- LLM engines (OpenAI + OpenAI-compatible); presets, prompt catalog, modes.
- `platform/hotkeys` + `platform/textio`; rewrite feature wired through slot
  resolution; in-place replace (D4) on all OSes incl. Wayland.
- **Outcome:** the core value proposition on all three OSes; the full stack proven.

### Phase 2 — Dictation feature + STT engines
- `platform/audio` mic capture; push-to-talk.
- OpenAI-API STT engine first; then local Whisper (`infer` model
  registry/download + GPU feature flags).
- Insert-at-cursor; speech overlay (level meter + state).
- **Outcome:** dictation, remote and offline, on all three OSes.

### Phase 3 — Meetings feature
- System/loopback audio capture (mic + other participants, D14) added to
  `platform/audio`; macOS Screen Recording permission flow.
- Composes STT + LLM slots: capture → segmented transcription → AI synthesis
  (notes + titles) → persisted record (`data.db`) → UI panel.
- **Outcome:** Granola-like meetings on all three OSes.

### Phase 4 — Parity polish, later engines, distribution
- Parakeet engine (sherpa-onnx; `ort` fallback documented) — prototype integration early.
- TTS engines: OpenAI TTS **and** a local engine via sherpa-onnx (reusing the
  ONNX runtime, D11) + `platform/audio` playback; selection read-aloud.
- macOS extras: `NSServices` context-menu equivalent, Accessibility insertion
  enhancement, notch/overlay polish.
- Autostart, notifications, per-OS first-run permission flows.
- Packaging & signing: macOS notarized `.dmg`/`.app`; Windows MSI + NSIS;
  Linux AppImage + `.deb`. Auto-update. Migrate release workflow. Retire Swift app.
- **Outcome:** full functional parity, all three OSes.

## 10. Definition of "Full Parity" (acceptance for Phase 4)

- Rewrite via global hotkey with in-place replacement — macOS, Windows, Linux
  (X11 + Wayland).
- Dictation, both remote and offline (Whisper) — all OSes.
- Meetings: capture (mic + system audio) → transcription → AI synthesis
  (notes + titles) — all OSes.
- Text-to-speech for selected text, remote (OpenAI) and local (sherpa-onnx) — all OSes.
- Parakeet available as a selectable STT engine — all OSes.
- Presets, prompt overrides, provider config, per-feature/per-slot engine
  bindings, secure key storage — all OSes.
- Tray, settings UI, first-run permissions, autostart — all OSes.
- Signed/packaged installers per OS with auto-update.

## 11. Testing Strategy

- **Per-crate unit tests** (TDD): each engine against a mocked transport;
  settings/preset/binding stores; feature orchestration driven by fake engines.
- **Trait-conformance suites:** one shared, table-driven battery every
  `SttEngine`/`LlmEngine`/`TtsEngine` impl must pass — the safeguard that keeps
  "add an engine later" from silently breaking a slot.
- **Slot-resolution tests:** auto-bind on single compatible engine; prompt on
  multiple; error on incompatible/missing.
- **Store tests:** `config.db` and `data.db` migrations apply cleanly;
  settings/preset/binding repos (config) and action/conversation/meeting repos
  (data) round-trip; retention prune and "store content" toggle behave;
  content-off path records metadata only.
- **Integration smoke per OS in CI:** hotkey → capture → replace; mic →
  transcribe (remote + a tiny offline model).
- **Manual platform matrix** checklist for Phase 4 acceptance (X11 + Wayland on
  GNOME + KDE, Windows, macOS).

## 12. Risks & Mitigations

- **Parakeet portable runtime (D6):** sherpa-onnx Rust bindings / model export
  may have rough edges. Mitigation: validate early in Phase 4; `ort` fallback;
  Parakeet is additive (not on the parity-critical path until Phase 4).
- **Wayland synthetic input/hotkeys (D7):** highest-risk integration; varies by
  compositor. Mitigation: prefer XDG portals; fall back to `uinput` with a
  documented udev rule; prototype on GNOME + KDE early in Phase 1.
- **System/loopback audio capture (Meetings, D14):** capturing the other side of
  a call is OS-specific and fiddly — macOS needs ScreenCaptureKit or an aggregate
  device (plus Screen Recording permission), Windows WASAPI loopback, Linux a
  Pulse/PipeWire monitor source. Mitigation: prototype per-OS at the start of
  Phase 3; fall back to mic-only meeting capture where loopback is unavailable.
- **whisper.cpp / ONNX GPU build matrix:** CUDA/Vulkan/Metal complexity in CI
  across both whisper.cpp and ONNX Runtime. Mitigation: CPU build as guaranteed
  baseline; GPU via feature flags + per-OS CI.
- **Clipboard-restore races (D4):** restoring clipboard after paste can race the
  target app. Mitigation: small post-paste delay + content verification; macOS
  Accessibility path as higher-fidelity option.
- **Windows code-signing:** required to avoid SmartScreen. Mitigation: Phase 4
  procurement item; unsigned dev builds until then.
- **Plugin-layer over-abstraction:** three trait families could ossify too early.
  Mitigation: traits are internal (D1, no external ABI), so they can be revised
  freely until parity; keep them minimal.

## 13. Open Items (deferred, not blocking)

- Windows code-signing certificate procurement (Phase 4 action item).
- Future: move credentials into an encrypted `config.db` table (master key in
  keyring as root of trust) if keyring-only proves limiting — deferred this phase
  (D9).

_(Resolved since first draft: React (D10), KEA name fixed (D13), Parakeet via
sherpa-onnx (D6), local TTS via sherpa-onnx (D11), macOS Accessibility insertion
kept (D12).)_
