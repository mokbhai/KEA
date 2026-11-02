# KEA

Cross-platform AI rewrite, dictation, meeting-notes, and text-to-speech utility built with Tauri, Rust, and React.

[Documentation](docs/README.md) | [Changelog](CHANGELOG.md) | [License](LICENSE)

<table>
  <tr>
    <td align="center"><a href="docs/media/kea-talk.mp4"><img src="docs/media/kea-talk.jpg" width="180" alt="Dictate: stop typing, start talking"></a><br><b>Dictate</b></td>
    <td align="center"><a href="docs/media/kea-rewrite.mp4"><img src="docs/media/kea-rewrite.jpg" width="180" alt="Rewrite selected text in place"></a><br><b>Rewrite</b></td>
    <td align="center"><a href="docs/media/kea-meetings.mp4"><img src="docs/media/kea-meetings.jpg" width="180" alt="Meeting notes"></a><br><b>Meeting notes</b></td>
    <td align="center"><a href="docs/media/kea-all-in-one.mp4"><img src="docs/media/kea-all-in-one.jpg" width="180" alt="One Mac app, four jobs"></a><br><b>All in one</b></td>
  </tr>
</table>

Click a card to watch the 20-second video. The app windows in the videos are drawn mock-ups.

## Platforms

**macOS** is the supported platform. Windows and Linux builds compile and are CI-tested, but runtime support (hotkeys, text I/O, audio capture, permissions) is not yet functional. Cross-platform support is planned.

## Features

- Global rewrite hotkeys with configurable provider/model bindings
- In-place text replacement through platform text I/O
- Dictation with hosted and local speech engines
- Meeting capture, transcription, diarization, and synthesis workflows
- Text-to-speech and activity/history surfaces
- Cross-platform Tauri 2.x shell (macOS supported; Windows/Linux build-only until runtime stubs are replaced)

## Install

On an Apple Silicon Mac, with [Homebrew](https://brew.sh):

```sh
brew install --cask mokbhai/tap/kea
```

Update with `brew upgrade --cask kea`. The cask lives in [mokbhai/homebrew-tap](https://github.com/mokbhai/homebrew-tap) and moves to each new release on its own. Installers for every platform are also on the [Releases](https://github.com/mokbhai/KEA/releases) page.

To build from source instead, read on.

## Prerequisites

Install Rust, Node.js 20+, platform build dependencies, and the Tauri CLI:

```bash
cargo install tauri-cli --version "^2" --locked
npm --prefix ui install
```

On Linux, install the WebKit/AppIndicator dependencies listed in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Run

```bash
make dev
```

This starts the Vite UI and opens the Tauri development app.

## Install On macOS

```bash
make install
open /Applications/KEA.app
```

`make install` builds the Tauri app and atomically replaces `/Applications/KEA.app`. See [Development documentation](docs/DEVELOPMENT.md) for how it handles macOS local code signing and Accessibility permissions.

## Development

```bash
make build          # Build the Tauri app bundle
make test           # Build the UI and run Rust workspace tests
make lint           # Run TypeScript, Rust compile, and active-doc hygiene checks
make reset-perms    # Reset macOS Accessibility, Screen Capture, and Microphone prompts
make release-check  # Run lint, tests, and a Tauri build
```

The old Swift/Xcode Vox runtime is retired for active development. Current code lives in the Rust workspace, `src-tauri/`, and `ui/`.

## Release

```bash
make release-check
make release-package
./scripts/release.sh 0.1.0
```

Release artifacts are generated in `dist/` from Tauri bundle outputs.
