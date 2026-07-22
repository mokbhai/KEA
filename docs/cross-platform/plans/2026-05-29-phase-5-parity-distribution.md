# Cross-Platform Rewrite — Phase 5 (Parity Polish + Distribution) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the Tauri app to full functional parity with today's shipping Swift product and ship it. Add the `platform::system` module (autostart, desktop notifications, first-run permission flows per OS), macOS-only enhancements (an `NSServices`-equivalent context-menu entry point, notch/overlay polish, an Accessibility-based insertion path over the clipboard+paste baseline), packaged + signed installers per OS with auto-update, a migrated GitHub Actions release pipeline producing per-OS installers and a GitHub Release, and a consolidated Parity Acceptance Checklist that verifies every item in spec §8 across macOS, Windows, and Linux (X11 + Wayland). The Swift app is retired once parity is verified.

**Architecture:** This phase fills in `app/crates/platform/src/system.rs` (autostart / notifications / permissions traits with pure, unit-tested helpers and OS-specific effecting impls behind `#[cfg(target_os = ...)]`), adds macOS-only enhancements behind `#[cfg(target_os = "macos")]` (context-menu/Services registration, overlay polish, Accessibility insertion implementing the existing `platform::textio::TextIo` trait as an alternative to `ClipboardTextIo`, per Decision D3), wires Tauri's bundler + updater for distribution, and replaces the Swift release machinery (`scripts/*`, `.github/workflows/release.yml`) with a Tauri-based pipeline. Inherently-manual steps (signing, notarization, OS permission prompts, installer smoke tests, the parity matrix) ship exact commands/config plus explicit manual verification with expected results; pure helpers and version/notes scripts are TDD'd.

**Tech Stack:** Rust (edition 2021), Tauri 2.x + `tauri-plugin-updater` + `tauri-plugin-notification` + `tauri-plugin-autostart`, the Tauri bundler (`tauri build`), `tauri-action` GitHub Action, `serde`/`serde_json`, `notify-rust` (Linux notification helper where the plugin is insufficient), `objc2`/`objc2-app-kit` + `core-foundation` (macOS-only Accessibility + Services), GitHub Actions (macos-latest, windows-latest, ubuntu-latest), `bash` release scripts, `node`/`npm` for the UI.

**Reference spec:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md` (Phase 5, §8 parity definition, §9 risks)

---

## File Structure

This phase ADDS the `system` module to the existing `vox-platform` crate (created in
Phase 1), adds macOS-only enhancement modules, wires Tauri distribution config, and
replaces the legacy Swift release pipeline with a Tauri one. Each file has one
responsibility.

- `app/crates/platform/src/system.rs` — autostart / notification / permission traits + pure helpers + per-OS effecting impls (NEW).
- `app/crates/platform/src/lib.rs` — re-export `system` alongside `hotkeys`, `textio`, `audio` (MODIFY).
- `app/crates/platform/src/macos_services.rs` — macOS-only `NSServices`-equivalent context-menu registration + invocation handler, behind `#[cfg(target_os = "macos")]` (NEW).
- `app/crates/platform/src/macos_axtextio.rs` — macOS-only `AxTextIo` implementing `platform::textio::TextIo` via the Accessibility API (enhancement over D3 baseline), behind `#[cfg(target_os = "macos")]` (NEW).
- `app/crates/platform/Cargo.toml` — add macOS-only `objc2`/`core-foundation` deps and `notify-rust`/dev-deps (MODIFY).
- `app/src-tauri/src/commands.rs` — add `set_autostart`, `get_autostart`, `notify`, `permission_status`, `request_permission`, `check_update` commands wrapping `system` + updater (MODIFY).
- `app/src-tauri/src/main.rs` — register the new commands, plugins (autostart/notification/updater), macOS Services handler, first-run permission flow (MODIFY).
- `app/src-tauri/Cargo.toml` — add `tauri-plugin-updater`, `tauri-plugin-notification`, `tauri-plugin-autostart`, bundler features (MODIFY).
- `app/src-tauri/tauri.conf.json` — bundler targets per OS, updater config, signing identifiers (MODIFY).
- `app/src-tauri/capabilities/default.json` — grant the updater/notification/autostart plugin permissions to the windows (MODIFY/Create).
- `app/scripts/current_version.sh` — read version from `app/src-tauri/tauri.conf.json` (NEW; replaces reading `project.pbxproj`).
- `app/scripts/set_version.sh` — write version into `tauri.conf.json` + `Cargo.toml`s (NEW).
- `app/scripts/extract_release_notes.sh` — extract a version section from `CHANGELOG.md` (NEW; ported logic).
- `app/scripts/release.sh` — orchestrate version bump + checks + tag for the Tauri app (NEW).
- `.github/workflows/app-release.yml` — tag-triggered Tauri build/release matrix producing per-OS installers + a GitHub Release (NEW).
- `app/README.md` — develop/build/release/permissions/signing docs for the cross-platform app (MODIFY).
- `README.md` — point the root README at the cross-platform app + retirement note (MODIFY).
- `docs/cross-platform/PARITY-CHECKLIST.md` — the consolidated, runnable §8 parity acceptance checklist (NEW).

---

## Prerequisites (one-time, not committed)

- [ ] **Step 0: Verify the toolchain + signing inputs are available**

Run:
```bash
rustc --version && cargo --version && node --version && npm --version
cargo tauri --version 2>/dev/null || cargo install tauri-cli --version "^2" --locked
```
Expected: `rustc`/`cargo`/`node`/`npm` print versions; `cargo tauri` prints a 2.x version.
On Linux also install bundler deps (AppImage/.deb need these):
```bash
sudo apt-get update && sudo apt-get install -y \
  libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev rpm fakeroot
```
Signing/notarization inputs to procure and store as GitHub Actions secrets (tracked
per spec §9; builds run unsigned until present):
- macOS: `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID`.
- Windows: a code-signing certificate (`WINDOWS_CERTIFICATE` base64 + `WINDOWS_CERTIFICATE_PASSWORD`) — **procurement is a tracked Phase 5 item (spec §9)**; ship unsigned dev builds until purchased.
- Updater: a Tauri updater key pair (next step).

- [ ] **Step 1: Generate the Tauri updater signing key pair (one-time)**

Run:
```bash
cd app
cargo tauri signer generate -w ~/.tauri/vox-updater.key
```
Expected: prints a public key and writes the private key to `~/.tauri/vox-updater.key`
(with a password you choose). Store the **private key** as the GitHub secret
`TAURI_SIGNING_PRIVATE_KEY` and its password as `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
The printed **public key** is pasted into `tauri.conf.json` (Task 6, Step 1). Never
commit the private key.

---

## Task 1: `platform::system` — traits + pure helpers (autostart/notifications/permissions)

**Files:**
- Create: `app/crates/platform/src/system.rs`
- Modify: `app/crates/platform/src/lib.rs`
- Modify: `app/crates/platform/Cargo.toml`
- Test: in-file `#[cfg(test)]` module in `system.rs`

- [ ] **Step 1: Write the failing test for the pure helpers**

Create `app/crates/platform/src/system.rs`:
```rust
//! OS integration: autostart, desktop notifications, and first-run permission
//! flows. Pure helpers (label/identifier formatting, plist/desktop-entry/Run-key
//! payload generation, permission-state classification) are unit-tested here.
//! Effecting impls live behind `#[cfg(target_os = ...)]` and are smoke-checked
//! manually (see the Phase Acceptance section).

use std::path::PathBuf;

/// Bundle identifier shared with secrets (`com.voxapp.rewrite`) and Tauri.
pub const APP_IDENTIFIER: &str = "com.voxapp.rewrite";
/// Human-facing product name.
pub const APP_NAME: &str = "Vox";

/// A desktop notification to surface to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// OS-level permissions Vox may require. Linux/Windows report `Granted` for
/// capabilities the OS does not gate (callers branch on `Required`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Permission {
    /// macOS Accessibility (synthetic input + AX insertion).
    Accessibility,
    /// Microphone capture.
    Microphone,
    /// Linux: write access to /dev/uinput (Wayland synthetic input).
    Uinput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PermissionStatus {
    Granted,
    Denied,
    /// Not yet requested / undetermined (macOS first-run).
    NotDetermined,
    /// This OS does not gate this capability.
    NotApplicable,
}

#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("autostart: {0}")]
    Autostart(String),
    #[error("notification: {0}")]
    Notification(String),
    #[error("permission: {0}")]
    Permission(String),
}

/// Enables/disables launching Vox at login (LaunchAgent / Run key / XDG autostart).
pub trait Autostart: Send + Sync {
    fn set_enabled(&self, enabled: bool) -> Result<(), SystemError>;
    fn is_enabled(&self) -> Result<bool, SystemError>;
}

/// Posts desktop notifications.
pub trait Notifier: Send + Sync {
    fn notify(&self, notification: &Notification) -> Result<(), SystemError>;
}

/// Queries and requests OS permissions, and provides per-OS guidance text for
/// the first-run flow.
pub trait Permissions: Send + Sync {
    fn status(&self, permission: Permission) -> Result<PermissionStatus, SystemError>;
    /// Best-effort request/prompt; returns the resulting status (or
    /// `NotDetermined` if the OS shows an async prompt).
    fn request(&self, permission: Permission) -> Result<PermissionStatus, SystemError>;
}

// ---------- Pure helpers (unit-tested) ----------

/// macOS LaunchAgent plist contents for autostart, launching `exe_path` at login.
pub fn launch_agent_plist(exe_path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n  <string>{id}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n    <string>{exe}</string>\n  </array>\n\
  <key>RunAtLoad</key>\n  <true/>\n\
</dict>\n\
</plist>\n",
        id = APP_IDENTIFIER,
        exe = exe_path,
    )
}

/// Path of the per-user LaunchAgent plist (relative to $HOME).
pub fn launch_agent_relpath() -> PathBuf {
    PathBuf::from("Library/LaunchAgents").join(format!("{APP_IDENTIFIER}.plist"))
}

/// Linux XDG autostart `.desktop` entry contents launching `exec`.
pub fn xdg_autostart_desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
Type=Application\n\
Name={name}\n\
Exec={exec}\n\
X-GNOME-Autostart-enabled=true\n\
Terminal=false\n",
        name = APP_NAME,
        exec = exec,
    )
}

/// Path of the per-user XDG autostart entry (relative to $HOME, honoring
/// XDG_CONFIG_HOME being unset).
pub fn xdg_autostart_relpath() -> PathBuf {
    PathBuf::from(".config/autostart").join(format!("{APP_IDENTIFIER}.desktop"))
}

/// Windows `HKCU\...\Run` value name + data for autostart.
pub fn windows_run_entry(exe_path: &str) -> (String, String) {
    (APP_NAME.to_string(), format!("\"{exe_path}\""))
}

/// First-run permissions required on a given OS, in the order they should be
/// requested. `os` is the value of `std::env::consts::OS`.
pub fn required_permissions(os: &str) -> Vec<Permission> {
    match os {
        "macos" => vec![Permission::Accessibility, Permission::Microphone],
        // Linux gates uinput (Wayland synthetic input) + mic via Pulse/portal.
        "linux" => vec![Permission::Uinput, Permission::Microphone],
        // Windows gates the microphone via the privacy settings.
        "windows" => vec![Permission::Microphone],
        _ => vec![],
    }
}

/// Guidance shown in the first-run UI when a permission is missing.
pub fn permission_guidance(permission: Permission, os: &str) -> &'static str {
    match (permission, os) {
        (Permission::Accessibility, "macos") => {
            "Open System Settings > Privacy & Security > Accessibility and enable Vox \
so it can read selected text and paste rewrites."
        }
        (Permission::Microphone, "macos") => {
            "Open System Settings > Privacy & Security > Microphone and enable Vox to \
dictate speech."
        }
        (Permission::Microphone, _) => {
            "Allow microphone access for Vox in your system privacy settings to dictate \
speech."
        }
        (Permission::Uinput, "linux") => {
            "Wayland synthetic paste needs /dev/uinput access. Add your user to the \
'input' group and install this udev rule as /etc/udev/rules.d/99-vox-uinput.rules:\n\
  KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"\n\
then run: sudo udevadm control --reload-rules && sudo udevadm trigger\n\
On Wayland, also approve the global-shortcuts portal prompt when it appears."
        }
        _ => "No additional permission is required on this system.",
    }
}

/// Classify whether a queried status still blocks the capability.
pub fn blocks_capability(status: PermissionStatus) -> bool {
    matches!(status, PermissionStatus::Denied | PermissionStatus::NotDetermined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_plist_embeds_label_and_exe() {
        let p = launch_agent_plist("/Applications/Vox.app/Contents/MacOS/vox");
        assert!(p.contains("<string>com.voxapp.rewrite</string>"));
        assert!(p.contains("<string>/Applications/Vox.app/Contents/MacOS/vox</string>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(p.trim_start().starts_with("<?xml"));
    }

    #[test]
    fn launch_agent_relpath_is_per_user() {
        assert_eq!(
            launch_agent_relpath(),
            PathBuf::from("Library/LaunchAgents/com.voxapp.rewrite.plist")
        );
    }

    #[test]
    fn xdg_entry_has_required_keys() {
        let e = xdg_autostart_desktop_entry("/usr/bin/vox");
        assert!(e.starts_with("[Desktop Entry]\n"));
        assert!(e.contains("Exec=/usr/bin/vox\n"));
        assert!(e.contains("X-GNOME-Autostart-enabled=true\n"));
        assert_eq!(
            xdg_autostart_relpath(),
            PathBuf::from(".config/autostart/com.voxapp.rewrite.desktop")
        );
    }

    #[test]
    fn windows_run_entry_quotes_path() {
        let (name, data) = windows_run_entry(r"C:\Program Files\Vox\vox.exe");
        assert_eq!(name, "Vox");
        assert_eq!(data, "\"C:\\Program Files\\Vox\\vox.exe\"");
    }

    #[test]
    fn required_permissions_per_os() {
        assert_eq!(
            required_permissions("macos"),
            vec![Permission::Accessibility, Permission::Microphone]
        );
        assert_eq!(
            required_permissions("linux"),
            vec![Permission::Uinput, Permission::Microphone]
        );
        assert_eq!(required_permissions("windows"), vec![Permission::Microphone]);
        assert!(required_permissions("freebsd").is_empty());
    }

    #[test]
    fn guidance_mentions_udev_rule_on_linux() {
        let g = permission_guidance(Permission::Uinput, "linux");
        assert!(g.contains("/dev/uinput"));
        assert!(g.contains("99-vox-uinput.rules"));
        assert!(g.contains("portal"));
    }

    #[test]
    fn only_granted_and_n_a_unblock() {
        assert!(!blocks_capability(PermissionStatus::Granted));
        assert!(!blocks_capability(PermissionStatus::NotApplicable));
        assert!(blocks_capability(PermissionStatus::Denied));
        assert!(blocks_capability(PermissionStatus::NotDetermined));
    }
}
```

- [ ] **Step 2: Wire the module into the crate**

Edit `app/crates/platform/src/lib.rs` to add the re-export (keep existing ones):
```rust
pub mod hotkeys;
pub mod textio;
pub mod audio;
pub mod system;

#[cfg(target_os = "macos")]
pub mod macos_services;
#[cfg(target_os = "macos")]
pub mod macos_axtextio;
```

Edit `app/crates/platform/Cargo.toml` to add the deps this phase needs (merge under
the existing sections):
```toml
[dependencies]
serde = { workspace = true }
thiserror = "1"

[target.'cfg(target_os = "linux")'.dependencies]
notify-rust = "4"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = "0.5"
objc2-app-kit = "0.2"
objc2-foundation = "0.2"
core-foundation = "0.10"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform system`
Expected: all eight `system` unit tests PASS (this task is pure helpers + trait
definitions; effecting impls land in Task 2).

- [ ] **Step 4: Commit**

```bash
git add app/crates/platform/src/system.rs app/crates/platform/src/lib.rs app/crates/platform/Cargo.toml
git commit -m "feat(platform): add system module traits + pure OS-integration helpers"
```

---

## Task 2: `platform::system` — per-OS effecting implementations

**Files:**
- Modify: `app/crates/platform/src/system.rs`

- [ ] **Step 1: Add the autostart implementations behind cfg**

Append to `app/crates/platform/src/system.rs` (above the `tests` module). These
reuse the pure helpers from Task 1, so the logic stays testable:
```rust
use std::fs;

/// Per-OS autostart, selected at runtime via the factory below.
pub struct OsAutostart;

#[cfg(target_os = "macos")]
impl Autostart for OsAutostart {
    fn set_enabled(&self, enabled: bool) -> Result<(), SystemError> {
        let home = dirs_home()?;
        let path = home.join(launch_agent_relpath());
        if enabled {
            let exe = std::env::current_exe()
                .map_err(|e| SystemError::Autostart(e.to_string()))?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| SystemError::Autostart(e.to_string()))?;
            }
            fs::write(&path, launch_agent_plist(&exe.to_string_lossy()))
                .map_err(|e| SystemError::Autostart(e.to_string()))
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(SystemError::Autostart(e.to_string())),
            }
        }
    }
    fn is_enabled(&self) -> Result<bool, SystemError> {
        Ok(dirs_home()?.join(launch_agent_relpath()).exists())
    }
}

#[cfg(target_os = "linux")]
impl Autostart for OsAutostart {
    fn set_enabled(&self, enabled: bool) -> Result<(), SystemError> {
        let home = dirs_home()?;
        let path = home.join(xdg_autostart_relpath());
        if enabled {
            let exe = std::env::current_exe()
                .map_err(|e| SystemError::Autostart(e.to_string()))?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| SystemError::Autostart(e.to_string()))?;
            }
            fs::write(&path, xdg_autostart_desktop_entry(&exe.to_string_lossy()))
                .map_err(|e| SystemError::Autostart(e.to_string()))
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(SystemError::Autostart(e.to_string())),
            }
        }
    }
    fn is_enabled(&self) -> Result<bool, SystemError> {
        Ok(dirs_home()?.join(xdg_autostart_relpath()).exists())
    }
}

#[cfg(target_os = "windows")]
impl Autostart for OsAutostart {
    fn set_enabled(&self, enabled: bool) -> Result<(), SystemError> {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (run, _) = hkcu
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| SystemError::Autostart(e.to_string()))?;
        let exe = std::env::current_exe()
            .map_err(|e| SystemError::Autostart(e.to_string()))?;
        let (name, data) = windows_run_entry(&exe.to_string_lossy());
        if enabled {
            run.set_value(&name, &data).map_err(|e| SystemError::Autostart(e.to_string()))
        } else {
            match run.delete_value(&name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(SystemError::Autostart(e.to_string())),
            }
        }
    }
    fn is_enabled(&self) -> Result<bool, SystemError> {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let run = hkcu
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| SystemError::Autostart(e.to_string()))?;
        let (name, _) = windows_run_entry("");
        Ok(run.get_value::<String, _>(&name).is_ok())
    }
}

fn dirs_home() -> Result<std::path::PathBuf, SystemError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| SystemError::Autostart("no home directory".into()))
}

pub fn new_autostart() -> Box<dyn Autostart> {
    Box::new(OsAutostart)
}
```
Add the Windows-only registry dep to `app/crates/platform/Cargo.toml`:
```toml
[target.'cfg(target_os = "windows")'.dependencies]
winreg = "0.52"
```

- [ ] **Step 2: Add the notifier + permissions implementations**

Append to `system.rs` (still above `tests`):
```rust
pub struct OsNotifier;

impl Notifier for OsNotifier {
    fn notify(&self, notification: &Notification) -> Result<(), SystemError> {
        #[cfg(target_os = "linux")]
        {
            notify_rust::Notification::new()
                .summary(&notification.title)
                .body(&notification.body)
                .appname(APP_NAME)
                .show()
                .map(|_| ())
                .map_err(|e| SystemError::Notification(e.to_string()))
        }
        // macOS/Windows route through the Tauri notification plugin from
        // src-tauri (see Task 4). This impl is the Linux/no-plugin fallback;
        // on other targets it is a no-op the shell does not call.
        #[cfg(not(target_os = "linux"))]
        {
            let _ = notification;
            Ok(())
        }
    }
}

pub fn new_notifier() -> Box<dyn Notifier> {
    Box::new(OsNotifier)
}

pub struct OsPermissions;

impl Permissions for OsPermissions {
    fn status(&self, permission: Permission) -> Result<PermissionStatus, SystemError> {
        match (permission, std::env::consts::OS) {
            #[cfg(target_os = "macos")]
            (Permission::Accessibility, _) => Ok(macos_accessibility_status()),
            (Permission::Uinput, "linux") => Ok(uinput_status()),
            // Microphone status is reported by the OS prompt at capture time
            // (cpal). We report NotDetermined until a capture has occurred so
            // the first-run flow surfaces guidance; NotApplicable elsewhere.
            (Permission::Microphone, "macos") | (Permission::Microphone, "windows")
            | (Permission::Microphone, "linux") => Ok(PermissionStatus::NotDetermined),
            (Permission::Accessibility, _) => Ok(PermissionStatus::NotApplicable),
            (Permission::Uinput, _) => Ok(PermissionStatus::NotApplicable),
            (Permission::Microphone, _) => Ok(PermissionStatus::NotApplicable),
        }
    }
    fn request(&self, permission: Permission) -> Result<PermissionStatus, SystemError> {
        match (permission, std::env::consts::OS) {
            #[cfg(target_os = "macos")]
            (Permission::Accessibility, _) => {
                macos_prompt_accessibility();
                Ok(macos_accessibility_status())
            }
            // The OS shows its own async prompt for mic/portal; re-query after.
            _ => self.status(permission),
        }
    }
}

#[cfg(target_os = "linux")]
fn uinput_status() -> PermissionStatus {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/uinput")
    {
        Ok(_) => PermissionStatus::Granted,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => PermissionStatus::Denied,
        Err(_) => PermissionStatus::NotDetermined,
    }
}

#[cfg(target_os = "macos")]
fn macos_accessibility_status() -> PermissionStatus {
    // AXIsProcessTrusted() returns whether this process is trusted for AX.
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    if unsafe { AXIsProcessTrusted() } {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    }
}

#[cfg(target_os = "macos")]
fn macos_prompt_accessibility() {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef) -> bool;
        static kAXTrustedCheckOptionPrompt: core_foundation::string::CFStringRef;
    }
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = core_foundation::boolean::CFBoolean::true_value();
        let opts = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), value.as_CFType())]);
        let _ = AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef());
    }
}

pub fn new_permissions() -> Box<dyn Permissions> {
    Box::new(OsPermissions)
}
```
Add the Linux-only `libc` dep to `app/crates/platform/Cargo.toml`:
```toml
[target.'cfg(target_os = "linux")'.dependencies]
notify-rust = "4"
libc = "0.2"
```

- [ ] **Step 3: Verify it compiles + tests still pass on the current OS**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform system`
Expected: the eight pure-helper tests still PASS and the crate compiles on the host
OS (the cfg-gated effecting code type-checks for that OS).

- [ ] **Step 4: Manual smoke check (effects per OS — not a unit test)**

These exercise real OS side effects, so verify by hand on each OS:
- macOS: `OsAutostart.set_enabled(true)` then confirm `ls ~/Library/LaunchAgents/com.voxapp.rewrite.plist` exists; `set_enabled(false)` removes it.
- Linux: `set_enabled(true)` then confirm `~/.config/autostart/com.voxapp.rewrite.desktop` exists; toggling off removes it. `OsNotifier.notify(...)` shows a desktop notification.
- Windows: `set_enabled(true)` then `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v Vox` shows the quoted exe path; `set_enabled(false)` removes it.
Expected: each assertion holds. (Driven via the Tauri commands in Task 4, Step 3.)

- [ ] **Step 5: Commit**

```bash
git add app/crates/platform/src/system.rs app/crates/platform/Cargo.toml
git commit -m "feat(platform): implement per-OS autostart, notifications, permissions"
```

---

## Task 3: macOS-only enhancements — Services entry point, overlay polish, Accessibility insertion

**Files:**
- Create: `app/crates/platform/src/macos_services.rs`
- Create: `app/crates/platform/src/macos_axtextio.rs`
- Modify: `app/src-tauri/tauri.conf.json` (Services array under the macOS bundle)

These are macOS-only enhancements (spec Phase 5). Decision **D3 keeps clipboard+paste
as the cross-platform baseline**; `AxTextIo` is an *alternative* `TextIo` impl selected
only on macOS when Accessibility is granted.

- [ ] **Step 1: Write the failing test for the Services selector mapping (pure)**

Create `app/crates/platform/src/macos_services.rs`:
```rust
//! macOS-only `NSServices`-equivalent context-menu entry point. Registers a
//! "Rewrite with Vox" Services item; the responder forwards the selected text
//! to the rewrite pipeline. The Info.plist `NSServices` array (the actual menu
//! registration) is declared in `tauri.conf.json` (Step 3); this module maps
//! the inbound service message to an app action and is unit-tested for that
//! mapping.

/// Actions the macOS Services menu can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Rewrite,
    Speak,
}

/// Map a Services message name (the `NSMessage` value) to an action.
pub fn action_for_message(message: &str) -> Option<ServiceAction> {
    match message {
        "rewriteSelection" => Some(ServiceAction::Rewrite),
        "speakSelection" => Some(ServiceAction::Speak),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_service_messages() {
        assert_eq!(action_for_message("rewriteSelection"), Some(ServiceAction::Rewrite));
        assert_eq!(action_for_message("speakSelection"), Some(ServiceAction::Speak));
        assert_eq!(action_for_message("nope"), None);
    }
}
```

- [ ] **Step 2: Run the test (expect PASS) and add the responder glue**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform macos_services`
Expected: `maps_known_service_messages` PASSES.

Append the responder registration to `macos_services.rs` (the effecting part,
smoke-checked manually):
```rust
use std::sync::mpsc::Sender;

/// Install an NSServices provider object whose handler sends the inbound
/// selected text + action onto `tx` for the Tauri shell to process. Called once
/// at startup on macOS. Uses `NSApp().setServicesProvider(_:)`; the menu items
/// themselves come from the Info.plist `NSServices` array (Step 3).
pub fn register_services_provider(tx: Sender<(ServiceAction, String)>) {
    // Bridged via objc2: a small provider class with a method per NSMessage
    // (`rewriteSelection:userData:error:`) reads the pasteboard string and
    // forwards `(action, text)` on `tx`. Full objc2 class definition lives in
    // this module's `provider` submodule.
    provider::install(tx);
}

mod provider {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSPasteboard};
    use objc2_foundation::{MainThreadMarker, NSString};

    // A single shared sender, set at install time; read by the service method.
    static mut SENDER: Option<Sender<(ServiceAction, String)>> = None;

    pub fn install(tx: Sender<(ServiceAction, String)>) {
        // SAFETY: install runs once on the main thread at startup before any
        // service message can arrive.
        unsafe { SENDER = Some(tx); }
        let mtm = MainThreadMarker::new().expect("Services install must run on main thread");
        let app = NSApplication::sharedApplication(mtm);
        let provider: Retained<AnyObject> = make_provider_object();
        unsafe { app.setServicesProvider(Some(&provider)); }
    }

    // Reads the current pasteboard selection and forwards the action+text.
    pub(super) fn handle(action: ServiceAction) {
        let mtm = MainThreadMarker::new().expect("service handler on main thread");
        let pb = unsafe { NSPasteboard::generalPasteboard() };
        let text = unsafe { pb.stringForType(objc2_app_kit::NSPasteboardTypeString) }
            .map(|s: Retained<NSString>| s.to_string())
            .unwrap_or_default();
        let _ = mtm; // marker held for thread-confinement documentation
        // SAFETY: SENDER set in install(); only mutated once at startup.
        if let Some(tx) = unsafe { SENDER.as_ref() } {
            let _ = tx.send((action, text));
        }
    }

    // The objc2 class declaration with `rewriteSelection:userData:error:` and
    // `speakSelection:userData:error:` selectors that call `handle(...)`.
    fn make_provider_object() -> Retained<AnyObject> {
        objc_provider::new()
    }

    mod objc_provider {
        use super::*;
        use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};

        declare_class!(
            struct VoxServiceProvider;
            unsafe impl ClassType for VoxServiceProvider {
                type Super = objc2::runtime::NSObject;
                type Mutability = mutability::InteriorMutable;
                const NAME: &'static str = "VoxServiceProvider";
            }
            impl DeclaredClass for VoxServiceProvider {}
            unsafe impl VoxServiceProvider {
                #[method(rewriteSelection:userData:error:)]
                fn rewrite_selection(
                    &self,
                    _pb: *mut AnyObject,
                    _data: *mut AnyObject,
                    _err: *mut AnyObject,
                ) {
                    super::handle(ServiceAction::Rewrite);
                }
                #[method(speakSelection:userData:error:)]
                fn speak_selection(
                    &self,
                    _pb: *mut AnyObject,
                    _data: *mut AnyObject,
                    _err: *mut AnyObject,
                ) {
                    super::handle(ServiceAction::Speak);
                }
            }
        );

        pub fn new() -> Retained<AnyObject> {
            let obj: Retained<VoxServiceProvider> =
                unsafe { msg_send_id![VoxServiceProvider::alloc(), init] };
            // Erase to AnyObject for setServicesProvider.
            unsafe { Retained::cast(obj) }
        }
    }
}
```

- [ ] **Step 3: Declare the `NSServices` items in the macOS bundle config**

Edit `app/src-tauri/tauri.conf.json` to add the Services array under the macOS
bundle (merge with the bundle block configured in Task 6):
```json
{
  "bundle": {
    "macOS": {
      "infoPlist": {
        "NSServices": [
          {
            "NSMenuItem": { "default": "Rewrite with Vox" },
            "NSMessage": "rewriteSelection",
            "NSPortName": "Vox",
            "NSSendTypes": ["NSStringPboardType"],
            "NSReturnTypes": ["NSStringPboardType"]
          },
          {
            "NSMenuItem": { "default": "Speak with Vox" },
            "NSMessage": "speakSelection",
            "NSPortName": "Vox",
            "NSSendTypes": ["NSStringPboardType"]
          }
        ]
      }
    }
  }
}
```

- [ ] **Step 4: Add the Accessibility insertion `TextIo` enhancement**

Create `app/crates/platform/src/macos_axtextio.rs`:
```rust
//! macOS-only enhancement over the D3 clipboard+paste baseline: insert text
//! into the focused UI element via the Accessibility API (AXUIElement). Falls
//! back to the caller's clipboard path when AX is unavailable. Implements the
//! same `platform::textio::TextIo` trait so it is a drop-in alternative to
//! `ClipboardTextIo`, selected only on macOS when Accessibility is granted.

use crate::textio::{TextIo, TextIoError};

pub struct AxTextIo {
    fallback: crate::textio::ClipboardTextIo,
}

impl AxTextIo {
    pub fn new() -> Result<Self, TextIoError> {
        Ok(Self {
            fallback: crate::textio::ClipboardTextIo::new()?,
        })
    }

    /// True when this process is AX-trusted (mirrors system::Accessibility).
    fn ax_available() -> bool {
        extern "C" {
            fn AXIsProcessTrusted() -> bool;
        }
        unsafe { AXIsProcessTrusted() }
    }
}

impl TextIo for AxTextIo {
    fn capture_selection(&self) -> Result<String, TextIoError> {
        // Reading the focused element's AXSelectedText is unreliable across
        // apps; capture via the proven clipboard copy path (D3).
        self.fallback.capture_selection()
    }

    fn replace_selection(&self, text: &str) -> Result<(), TextIoError> {
        if Self::ax_available() {
            match ax_set_focused_value(text) {
                Ok(()) => return Ok(()),
                Err(_) => { /* fall through to clipboard path */ }
            }
        }
        self.fallback.replace_selection(text)
    }

    fn insert_text(&self, text: &str) -> Result<(), TextIoError> {
        if Self::ax_available() {
            if ax_set_focused_value(text).is_ok() {
                return Ok(());
            }
        }
        self.fallback.insert_text(text)
    }
}

/// Set the focused element's AXValue / replace its selected substring.
fn ax_set_focused_value(text: &str) -> Result<(), TextIoError> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    // AXUIElementCreateSystemWide -> copy kAXFocusedUIElementAttribute ->
    // set kAXSelectedTextAttribute to `text`. Errors map to Inject.
    let _ = CFString::new(text).as_concrete_TypeRef();
    ax_bridge::set_selected_text(text)
        .map_err(|e| TextIoError::Inject(format!("AX insertion failed: {e}")))
}

mod ax_bridge {
    /// Thin FFI over ApplicationServices' AXUIElement APIs. Returns Err with a
    /// human string on any AXError so the caller can fall back to clipboard.
    pub fn set_selected_text(text: &str) -> Result<(), String> {
        use core_foundation::base::{CFTypeRef, TCFType};
        use core_foundation::string::CFString;
        #[allow(non_snake_case)]
        extern "C" {
            fn AXUIElementCreateSystemWide() -> CFTypeRef;
            fn AXUIElementCopyAttributeValue(
                element: CFTypeRef,
                attribute: core_foundation::string::CFStringRef,
                value: *mut CFTypeRef,
            ) -> i32;
            fn AXUIElementSetAttributeValue(
                element: CFTypeRef,
                attribute: core_foundation::string::CFStringRef,
                value: CFTypeRef,
            ) -> i32;
        }
        const K_AX_FOCUSED: &str = "AXFocusedUIElement";
        const K_AX_SELECTED_TEXT: &str = "AXSelectedText";
        unsafe {
            let system = AXUIElementCreateSystemWide();
            if system.is_null() {
                return Err("no system-wide AX element".into());
            }
            let focused_attr = CFString::new(K_AX_FOCUSED);
            let mut focused: CFTypeRef = std::ptr::null();
            if AXUIElementCopyAttributeValue(
                system,
                focused_attr.as_concrete_TypeRef(),
                &mut focused,
            ) != 0
                || focused.is_null()
            {
                return Err("no focused element".into());
            }
            let value = CFString::new(text);
            let sel_attr = CFString::new(K_AX_SELECTED_TEXT);
            let err = AXUIElementSetAttributeValue(
                focused,
                sel_attr.as_concrete_TypeRef(),
                value.as_concrete_TypeRef() as CFTypeRef,
            );
            if err != 0 {
                return Err(format!("AXError {err}"));
            }
            Ok(())
        }
    }
}
```
(Both modules are already gated in `lib.rs` from Task 1, Step 2.)

- [ ] **Step 5: Verify it compiles (macOS) / is excluded (others)**

Run on macOS: `cargo build --manifest-path app/Cargo.toml -p vox-platform`
Expected: the macOS modules compile; `macos_services` test passes via
`cargo test --manifest-path app/Cargo.toml -p vox-platform macos_services`.
Run on Linux/Windows: `cargo build --manifest-path app/Cargo.toml -p vox-platform`
Expected: builds with the macOS modules excluded by `cfg`.

- [ ] **Step 6: Manual smoke check (macOS effects — not a unit test)**

After the bundled app is installed (Task 7): in any text field, select text, open the
app's Services submenu (right-click or app menu > Services) and choose "Rewrite with
Vox". Expected: the selection is rewritten in place. With Accessibility granted,
confirm replacement uses the AX path (no clipboard flicker) and that revoking
Accessibility makes it fall back to clipboard+paste without error.

- [ ] **Step 7: Commit**

```bash
git add app/crates/platform/src/macos_services.rs app/crates/platform/src/macos_axtextio.rs app/src-tauri/tauri.conf.json
git commit -m "feat(platform): macOS Services entry point + Accessibility insertion enhancement"
```

---

## Task 4: Tauri commands + shell wiring for system + first-run flow

**Files:**
- Modify: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/main.rs`
- Modify: `app/src-tauri/Cargo.toml`
- Modify: `app/src-tauri/capabilities/default.json`

- [ ] **Step 1: Add the commands wrapping `platform::system` + updater**

Append to `app/src-tauri/src/commands.rs`:
```rust
use vox_platform::system::{
    new_autostart, new_notifier, new_permissions, Notification, Permission, PermissionStatus,
    permission_guidance, required_permissions,
};

#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    new_autostart().set_enabled(enabled).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_autostart() -> Result<bool, String> {
    new_autostart().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn notify(title: String, body: String) -> Result<(), String> {
    new_notifier()
        .notify(&Notification { title, body })
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct PermissionRow {
    pub permission: Permission,
    pub status: PermissionStatus,
    pub guidance: String,
}

#[tauri::command]
pub fn permission_status() -> Result<Vec<PermissionRow>, String> {
    let perms = new_permissions();
    let os = std::env::consts::OS;
    required_permissions(os)
        .into_iter()
        .map(|p| {
            let status = perms.status(p).map_err(|e| e.to_string())?;
            Ok(PermissionRow {
                permission: p,
                status,
                guidance: permission_guidance(p, os).to_string(),
            })
        })
        .collect()
}

#[tauri::command]
pub fn request_permission(permission: Permission) -> Result<PermissionStatus, String> {
    new_permissions().request(permission).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Register the commands, plugins, Services handler, and first-run flow**

Edit `app/src-tauri/src/main.rs` to register everything (merge with the existing
`setup`/tray/`invoke_handler` from earlier phases):
```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // ... existing tray setup from earlier phases ...

            // macOS Services entry point: forward (action, text) to commands.
            #[cfg(target_os = "macos")]
            {
                use tauri::Manager;
                let (tx, rx) = std::sync::mpsc::channel();
                vox_platform::macos_services::register_services_provider(tx);
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    while let Ok((action, text)) = rx.recv() {
                        let event = match action {
                            vox_platform::macos_services::ServiceAction::Rewrite => "service:rewrite",
                            vox_platform::macos_services::ServiceAction::Speak => "service:speak",
                        };
                        let _ = handle.emit(event, text);
                    }
                });
            }

            // First-run permission flow: if any required permission still
            // blocks a capability, open the settings window to its Permissions
            // tab (the UI calls `permission_status` and renders guidance).
            {
                use tauri::Manager;
                let blocking = commands::permission_status()
                    .unwrap_or_default()
                    .into_iter()
                    .any(|row| vox_platform::system::blocks_capability(row.status));
                if blocking {
                    if let Some(w) = app.get_webview_window("settings") {
                        let _ = w.show();
                        let _ = w.set_focus();
                        let _ = w.emit("permissions:show", ());
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::load_settings,
            commands::save_settings,
            commands::set_secret,
            commands::has_secret,
            // ... earlier-phase commands (rewrite_selection, start_dictation,
            // stop_dictation, list_models, download_model, speak_selection) ...
            commands::set_autostart,
            commands::get_autostart,
            commands::notify,
            commands::permission_status,
            commands::request_permission,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```
Add the plugins to `app/src-tauri/Cargo.toml`:
```toml
[dependencies]
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-notification = "2"
tauri-plugin-updater = "2"
tauri-plugin-autostart = "2"
vox-core = { path = "../crates/core" }
vox-platform = { path = "../crates/platform" }
serde = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 3: Grant plugin permissions to the windows**

Edit `app/src-tauri/capabilities/default.json` (create if missing) so the windows can
call the plugins:
```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Default capabilities for Vox windows.",
  "windows": ["settings", "overlay"],
  "permissions": [
    "core:default",
    "notification:default",
    "updater:default",
    "autostart:allow-enable",
    "autostart:allow-disable",
    "autostart:allow-is-enabled"
  ]
}
```

- [ ] **Step 4: Verify it builds**

Run: `npm --prefix app/ui run build && cargo build --manifest-path app/Cargo.toml -p vox`
Expected: the UI builds and the `vox` binary compiles with the six new commands and
three plugins registered.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/main.rs app/src-tauri/Cargo.toml app/src-tauri/capabilities/default.json
git commit -m "feat(app): expose system commands, plugins, Services handler, first-run flow"
```

---

## Task 5: Settings + UI for autostart, notifications, and permissions

**Files:**
- Modify: `app/crates/core/src/settings.rs`
- Modify: `app/ui/src/App.tsx`

- [ ] **Step 1: Write the failing test for the bumped schema version**

`launch_at_login` already exists on `Settings` (Phase 0). This phase makes it
authoritative and bumps the schema. Edit the `Default for Settings` impl in
`settings.rs` to set `schema_version: 6` and add the test:
```rust
    #[test]
    fn phase5_schema_version_is_six() {
        assert_eq!(Settings::default().schema_version, 6);
    }

    #[test]
    fn launch_at_login_defaults_off_and_roundtrips() {
        let mut s = Settings::default();
        assert!(!s.launch_at_login);
        s.launch_at_login = true;
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert!(parsed.launch_at_login);
    }
```

- [ ] **Step 2: Run the test (expect FAIL), then bump the version**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: FAIL — `phase5_schema_version_is_six` fails until the default is bumped.
Change `schema_version: 5` (Phase 4's value) to `schema_version: 6` in
`Default for Settings`.

- [ ] **Step 3: Run the test (expect PASS)**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: all settings tests PASS, including the two new ones.

- [ ] **Step 4: Add the Permissions + autostart UI section**

Edit `app/ui/src/App.tsx` to add a permissions/launch section (merge with the existing
settings form; full block shown):
```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type PermissionRow = {
  permission: "Accessibility" | "Microphone" | "Uinput";
  status: "Granted" | "Denied" | "NotDetermined" | "NotApplicable";
  guidance: string;
};

export function PermissionsSection() {
  const [rows, setRows] = useState<PermissionRow[]>([]);
  const [autostart, setAutostart] = useState(false);

  const refresh = () => {
    invoke<PermissionRow[]>("permission_status").then(setRows).catch(() => {});
    invoke<boolean>("get_autostart").then(setAutostart).catch(() => {});
  };

  useEffect(() => {
    refresh();
    const un = listen("permissions:show", refresh);
    return () => {
      un.then((f) => f());
    };
  }, []);

  return (
    <section style={{ padding: 16, fontFamily: "system-ui" }}>
      <h2>Permissions</h2>
      {rows.map((r) => (
        <div key={r.permission} style={{ marginBottom: 12 }}>
          <strong>
            {r.permission}: {r.status}
          </strong>
          {r.status !== "Granted" && r.status !== "NotApplicable" && (
            <>
              <p style={{ whiteSpace: "pre-line" }}>{r.guidance}</p>
              <button
                onClick={async () => {
                  await invoke("request_permission", { permission: r.permission });
                  refresh();
                }}
              >
                Grant…
              </button>
            </>
          )}
        </div>
      ))}
      <h2>Startup</h2>
      <label>
        <input
          type="checkbox"
          checked={autostart}
          onChange={async (e) => {
            await invoke("set_autostart", { enabled: e.target.checked });
            setAutostart(e.target.checked);
          }}
        />{" "}
        Launch Vox at login
      </label>
    </section>
  );
}
```
Render `<PermissionsSection />` inside the existing `App` component below the settings
form.

- [ ] **Step 5: Verify the UI builds**

Run: `npm --prefix app/ui run build`
Expected: Vite build succeeds and `app/ui/dist` is produced.

- [ ] **Step 6: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`
Expected: the Permissions section lists the OS-appropriate rows with live statuses and
guidance; "Grant…" triggers the OS prompt (macOS Accessibility/Microphone, Linux portal
guidance); toggling "Launch Vox at login" creates/removes the autostart entry (verify
per Task 2, Step 4).

- [ ] **Step 7: Commit**

```bash
git add app/crates/core/src/settings.rs app/ui/src/App.tsx
git commit -m "feat(ui): permissions + autostart settings section; bump schema to v6"
```

---

## Task 6: Bundler + updater configuration (packaging & signing)

**Files:**
- Modify: `app/src-tauri/tauri.conf.json`

- [ ] **Step 1: Configure bundler targets, signing, and the updater**

Edit `app/src-tauri/tauri.conf.json` so it contains the full bundle + updater config
(merge with windows/tray/identifier from Phase 0 and the macOS `NSServices` from Task 3;
complete blocks shown). Replace `PASTE_UPDATER_PUBLIC_KEY_HERE` with the public key from
Prerequisites Step 1, and `https://releases.voxapp.example/...` with the real endpoint:
```json
{
  "productName": "Vox",
  "identifier": "com.voxapp.rewrite",
  "version": "2.1.0",
  "bundle": {
    "active": true,
    "targets": ["app", "dmg", "msi", "nsis", "appimage", "deb"],
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "category": "Productivity",
    "shortDescription": "AI text rewriting + dictation, everywhere.",
    "longDescription": "Vox rewrites selected text in place and dictates speech to text via global hotkeys, on macOS, Windows, and Linux.",
    "macOS": {
      "minimumSystemVersion": "12.0",
      "signingIdentity": null,
      "entitlements": "entitlements.plist",
      "infoPlist": {
        "LSUIElement": true,
        "NSMicrophoneUsageDescription": "Vox needs the microphone to transcribe your speech.",
        "NSServices": [
          {
            "NSMenuItem": { "default": "Rewrite with Vox" },
            "NSMessage": "rewriteSelection",
            "NSPortName": "Vox",
            "NSSendTypes": ["NSStringPboardType"],
            "NSReturnTypes": ["NSStringPboardType"]
          },
          {
            "NSMenuItem": { "default": "Speak with Vox" },
            "NSMessage": "speakSelection",
            "NSPortName": "Vox",
            "NSSendTypes": ["NSStringPboardType"]
          }
        ]
      }
    },
    "windows": {
      "certificateThumbprint": null,
      "digestAlgorithm": "sha256",
      "timestampUrl": "http://timestamp.digicert.com",
      "nsis": { "installMode": "perMachine" }
    },
    "linux": {
      "deb": {
        "depends": ["libwebkit2gtk-4.1-0", "libayatana-appindicator3-1"]
      }
    }
  },
  "plugins": {
    "updater": {
      "active": true,
      "pubkey": "PASTE_UPDATER_PUBLIC_KEY_HERE",
      "endpoints": [
        "https://github.com/voxapp/vox/releases/latest/download/latest.json"
      ],
      "windows": { "installMode": "passive" }
    }
  }
}
```
Notes carried from spec §9: `macOS.signingIdentity` and `windows.certificateThumbprint`
stay `null` until the certs are procured; CI injects signing via environment (Task 8).
`LSUIElement: true` keeps Vox a menu-bar/tray app (no Dock icon), matching the Swift app.

- [ ] **Step 2: Add the macOS entitlements file**

Create `app/src-tauri/entitlements.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.automation.apple-events</key>
  <true/>
  <key>com.apple.security.device.audio-input</key>
  <true/>
</dict>
</plist>
```

- [ ] **Step 3: Verify the config is valid and bundles locally (unsigned)**

Run: `cd app && cargo tauri build --no-bundle && npx tauri build 2>/dev/null || cargo tauri build`
Then on each OS run the OS-appropriate bundle:
- macOS: `cargo tauri build --bundles app,dmg`
- Windows: `cargo tauri build --bundles msi,nsis`
- Linux: `cargo tauri build --bundles appimage,deb`
Expected: `cargo tauri build` produces installers under
`app/src-tauri/target/release/bundle/` (`dmg/`, `macos/`, `msi/`, `nsis/`, `appimage/`,
`deb/`). Unsigned is acceptable here; signing is verified in Task 8.

- [ ] **Step 4: Manual signing/notarization verification (not a unit test)**

These require real certs/credentials (spec §9 tracked item). With the macOS signing
env set (Prerequisites Step 0):
```bash
codesign --verify --deep --strict --verbose=2 \
  "app/src-tauri/target/release/bundle/macos/Vox.app"
xcrun notarytool submit \
  "app/src-tauri/target/release/bundle/dmg/Vox_2.1.0_aarch64.dmg" \
  --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_PASSWORD" --wait
xcrun stapler staple "app/src-tauri/target/release/bundle/dmg/Vox_2.1.0_aarch64.dmg"
```
Expected: `codesign --verify` prints `valid on disk` / `satisfies its Designated
Requirement`; `notarytool` returns `status: Accepted`; `stapler staple` prints `The
staple and validate action worked!`. For Windows, with `WINDOWS_CERTIFICATE` configured,
`signtool verify /pa /v Vox_2.1.0_x64-setup.exe` prints `Successfully verified`.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/tauri.conf.json app/src-tauri/entitlements.plist
git commit -m "feat(app): configure Tauri bundler targets, signing, and updater"
```

---

## Task 7: Versioning scripts for the Tauri app (TDD)

**Files:**
- Create: `app/scripts/current_version.sh`
- Create: `app/scripts/set_version.sh`
- Create: `app/scripts/extract_release_notes.sh`
- Create: `app/scripts/release.sh`
- Test: a shell test harness invoked inline

These port the existing `scripts/{current_version,set_version,extract_release_notes,
release}.sh` (which read `VoxNative.xcodeproj/project.pbxproj`) to read/write the Tauri
app's version in `app/src-tauri/tauri.conf.json`. The pure parsing/writing logic is TDD'd
with a bash test harness; `release.sh` orchestration is verified by a dry run.

- [ ] **Step 1: Write the failing test harness**

Create `app/scripts/version_test.sh` (a temporary harness, removed in Step 6):
```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/app/src-tauri" "$TMP/app/scripts"
cp "$ROOT/app/scripts/current_version.sh" "$ROOT/app/scripts/set_version.sh" \
   "$ROOT/app/scripts/extract_release_notes.sh" "$TMP/app/scripts/"
cat > "$TMP/app/src-tauri/tauri.conf.json" <<'JSON'
{ "productName": "Vox", "identifier": "com.voxapp.rewrite", "version": "2.1.0" }
JSON
cat > "$TMP/CHANGELOG.md" <<'MD'
# Changelog

## [2.2.0] - 2026-05-29
- Cross-platform parity release.

## [2.1.0] - 2026-04-01
- Older release.
MD

# current_version reads the JSON version
got="$(cd "$TMP" && ./app/scripts/current_version.sh)"
[[ "$got" == "2.1.0" ]] || { echo "FAIL current_version: $got"; exit 1; }

# set_version writes a new version
(cd "$TMP" && ./app/scripts/set_version.sh 2.2.0)
got="$(cd "$TMP" && ./app/scripts/current_version.sh)"
[[ "$got" == "2.2.0" ]] || { echo "FAIL set_version: $got"; exit 1; }

# extract_release_notes pulls the right section
notes="$(cd "$TMP" && ./app/scripts/extract_release_notes.sh 2.2.0)"
echo "$notes" | grep -q "Cross-platform parity release." || { echo "FAIL notes"; exit 1; }
echo "$notes" | grep -q "Older release." && { echo "FAIL notes leaked next section"; exit 1; }

echo "ALL VERSION SCRIPT TESTS PASSED"
```
Make it executable: `chmod +x app/scripts/version_test.sh`.

- [ ] **Step 2: Run the harness (expect FAIL)**

Run: `bash app/scripts/version_test.sh`
Expected: FAIL — the three scripts do not exist yet.

- [ ] **Step 3: Write the scripts**

Create `app/scripts/current_version.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONF="$ROOT_DIR/src-tauri/tauri.conf.json"
VERSION="$(node -e "process.stdout.write(require('$CONF').version)" 2>/dev/null \
  || sed -n 's/.*\"version\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p' "$CONF" | head -n1)"
if [[ -z "${VERSION:-}" ]]; then
  echo "Could not determine version from $CONF" >&2
  exit 1
fi
echo "$VERSION"
```
Create `app/scripts/set_version.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
if [[ $# -ne 1 ]]; then
  echo "Usage: app/scripts/set_version.sh <major.minor.patch>" >&2
  exit 1
fi
VERSION="$1"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid semantic version: $VERSION" >&2
  exit 1
fi
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONF="$ROOT_DIR/src-tauri/tauri.conf.json"
perl -0pi -e 's/("version"\s*:\s*)"[^"]*"/${1}"'"$VERSION"'"/' "$CONF"
echo "Updated Tauri app version to $VERSION"
```
Create `app/scripts/extract_release_notes.sh` (ported from the root script):
```bash
#!/usr/bin/env bash
set -euo pipefail
if [[ $# -ne 1 ]]; then
  echo "Usage: app/scripts/extract_release_notes.sh <version>" >&2
  exit 1
fi
VERSION="${1#v}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHANGELOG_FILE="$ROOT_DIR/CHANGELOG.md"
NOTES="$(
  awk -v version="$VERSION" '
    $0 ~ "^## \\[" version "\\] - " { collecting = 1; next }
    collecting && /^## \[/ { exit }
    collecting { print }
  ' "$CHANGELOG_FILE"
)"
if [[ -z "${NOTES//[$'\t\r\n ']}" ]]; then
  echo "Release notes for $VERSION were not found in $CHANGELOG_FILE" >&2
  exit 1
fi
printf '%s\n' "$NOTES"
```
Make them executable: `chmod +x app/scripts/current_version.sh app/scripts/set_version.sh app/scripts/extract_release_notes.sh`.

- [ ] **Step 4: Run the harness (expect PASS)**

Run: `bash app/scripts/version_test.sh`
Expected: prints `ALL VERSION SCRIPT TESTS PASSED`.

- [ ] **Step 5: Write the release orchestration script**

Create `app/scripts/release.sh` (ported from `scripts/release.sh`, adapted to the Tauri
app — version lives in `tauri.conf.json`, checks run `cargo test` + UI build, tagging
uses an `app-v` prefix so it does not collide with the legacy Swift `v*` tags):
```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
APP_DIR="$ROOT_DIR/app"

usage() { echo "Usage: app/scripts/release.sh <major.minor.patch>"; exit 1; }
[[ $# -eq 1 ]] || usage

VERSION="$1"
TAG="app-v$VERSION"
DATE="$(date +%F)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "Version must use semantic versioning (for example: 2.2.0)." >&2; exit 1; }

cd "$ROOT_DIR"
DIRTY_FILES="$(git status --porcelain | awk '{print $2}')"
while IFS= read -r file; do
  if [[ -n "$file" && "$file" != "CHANGELOG.md" ]]; then
    echo "Working tree contains unrelated changes: $file" >&2
    echo "Commit or stash them before starting a release." >&2
    exit 1
  fi
done <<< "$DIRTY_FILES"

git rev-parse "$TAG" >/dev/null 2>&1 && { echo "Git tag $TAG already exists." >&2; exit 1; }
grep -q "^## \[$VERSION\] - $DATE$" CHANGELOG.md || {
  echo "Add a CHANGELOG entry for $VERSION dated $DATE before releasing." >&2; exit 1; }

"$APP_DIR/scripts/set_version.sh" "$VERSION"
cargo test --manifest-path "$APP_DIR/Cargo.toml" -p vox-core -p vox-platform
npm --prefix "$APP_DIR/ui" run build

git add CHANGELOG.md "$APP_DIR/src-tauri/tauri.conf.json"
git commit -m "chore(app): release $TAG"
git tag -a "$TAG" -m "Release $TAG"

echo "Release commit and tag created."
echo "Next steps:"
echo "  git push origin main"
echo "  git push origin $TAG"
```
Make it executable: `chmod +x app/scripts/release.sh`.

- [ ] **Step 6: Remove the temporary harness and verify a dry run**

Run:
```bash
rm app/scripts/version_test.sh
bash -n app/scripts/release.sh && echo "release.sh syntax OK"
./app/scripts/current_version.sh
```
Expected: `release.sh syntax OK` and `current_version.sh` prints the configured version
(e.g. `2.1.0`).

- [ ] **Step 7: Commit**

```bash
git add app/scripts/current_version.sh app/scripts/set_version.sh app/scripts/extract_release_notes.sh app/scripts/release.sh
git commit -m "build(app): add Tauri version + release scripts (ported from Swift pipeline)"
```

---

## Task 8: Migrate the release pipeline to the new app (GitHub Actions)

**Files:**
- Create: `.github/workflows/app-release.yml`

- [ ] **Step 1: Write the tag-triggered release workflow**

Create `.github/workflows/app-release.yml`. It triggers on `app-v*` tags (leaving the
legacy Swift `release.yml` on `v*` tags untouched until retirement, Task 10), builds per
OS via `tauri-action`, signs/notarizes when secrets are present, and publishes one
GitHub Release with all installers + the updater `latest.json`:
```yaml
name: App Release

on:
  push:
    tags:
      - "app-v*"

permissions:
  contents: write

jobs:
  verify-version:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - name: Verify tag matches app version
        run: |
          APP_VERSION="$(./app/scripts/current_version.sh)"
          TAG_VERSION="${GITHUB_REF_NAME#app-v}"
          if [[ "$APP_VERSION" != "$TAG_VERSION" ]]; then
            echo "Tag $TAG_VERSION != app version $APP_VERSION" >&2
            exit 1
          fi

  build-and-publish:
    needs: verify-version
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: macos-latest
            args: "--target universal-apple-darwin --bundles app,dmg,updater"
          - os: windows-latest
            args: "--bundles msi,nsis,updater"
          - os: ubuntu-latest
            args: "--bundles appimage,deb,updater"
    runs-on: ${{ matrix.os }}
    timeout-minutes: 90
    steps:
      - uses: actions/checkout@v6

      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.os == 'macos-latest' && 'aarch64-apple-darwin,x86_64-apple-darwin' || '' }}

      - uses: actions/setup-node@v4
        with:
          node-version: 20

      - name: Install Linux system deps
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
            libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev rpm fakeroot

      - name: Install UI deps
        run: npm --prefix app/ui ci || npm --prefix app/ui install

      - name: Extract release notes
        if: matrix.os == 'ubuntu-latest'
        run: ./app/scripts/extract_release_notes.sh "${GITHUB_REF_NAME#app-v}" > app-release-notes.md

      - name: Build, sign, and publish
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # Updater signing (always present once configured)
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
          # macOS code-signing + notarization (optional until procured, spec §9)
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          # Windows code-signing (optional until cert procured, spec §9)
          WINDOWS_CERTIFICATE: ${{ secrets.WINDOWS_CERTIFICATE }}
          WINDOWS_CERTIFICATE_PASSWORD: ${{ secrets.WINDOWS_CERTIFICATE_PASSWORD }}
        with:
          projectPath: app
          tagName: ${{ github.ref_name }}
          releaseName: "Vox ${{ github.ref_name }}"
          releaseBody: ${{ matrix.os == 'ubuntu-latest' && 'See app-release-notes.md' || '' }}
          releaseDraft: true
          prerelease: false
          includeUpdaterJson: true
          args: ${{ matrix.args }}
```

- [ ] **Step 2: Verify the workflow is valid YAML + steps mirror locally**

Run:
```bash
python3 -c "import sys,yaml; yaml.safe_load(open('.github/workflows/app-release.yml')); print('workflow YAML OK')"
cargo test --manifest-path app/Cargo.toml -p vox-core -p vox-platform
npm --prefix app/ui run build
```
Expected: `workflow YAML OK`; the Rust tests pass and the UI builds (mirrors the build
the workflow performs on the host OS).

- [ ] **Step 3: Manual end-to-end release verification (not a unit test)**

Push a release tag and confirm the workflow produces installers + a release:
```bash
./app/scripts/release.sh 2.2.0   # after adding the CHANGELOG entry
git push origin main && git push origin app-v2.2.0
gh run watch "$(gh run list --workflow=app-release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
gh release view app-v2.2.0 --json assets -q '.assets[].name'
```
Expected: the **App Release** workflow succeeds on all three runners; the draft release
`app-v2.2.0` lists assets including `Vox_2.2.0_*.dmg`, `Vox_2.2.0_x64-setup.exe` (NSIS),
`Vox_2.2.0_x64_en-US.msi`, `Vox_2.2.0_*.AppImage`, `vox_2.2.0_amd64.deb`, and
`latest.json` (the updater manifest). Auto-update: install `2.1.0`, publish `2.2.0`, and
confirm the running app detects + installs the update on next launch.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/app-release.yml
git commit -m "ci(app): tag-triggered Tauri release producing per-OS installers + updater"
```

---

## Task 9: Parity Acceptance Checklist (spec §8) + docs

**Files:**
- Create: `docs/cross-platform/PARITY-CHECKLIST.md`
- Modify: `app/README.md`
- Modify: `README.md`

- [ ] **Step 1: Write the consolidated parity checklist**

Create `docs/cross-platform/PARITY-CHECKLIST.md` covering every spec §8 item across the
full OS/display-server matrix. This is the gating artifact for "full parity":
```markdown
# Vox Cross-Platform Parity Acceptance Checklist

Gate for retiring the Swift app (Decision D4). Every row in spec §8 must pass on
every applicable platform. Run a build from the `App Release` workflow, install the
per-OS installer, and check each box. Re-run when any platform-touching code changes.

Legend: ✅ pass · ❌ fail · n/a not applicable.

## Platforms

| # | macOS | Windows | Linux X11 | Linux Wayland |
|---|-------|---------|-----------|----------------|

## §8 Acceptance items

### 1. Rewrite via global hotkey with in-place replacement
- [ ] macOS: select text, press rewrite hotkey, text is replaced in place.
- [ ] Windows: same.
- [ ] Linux X11: same (XTest synthetic paste).
- [ ] Linux Wayland: same (GlobalShortcuts portal + uinput paste; udev rule applied).

### 2. Speech-to-text — remote
- [ ] macOS / Windows / Linux X11 / Linux Wayland: push-to-talk dictates via the
  hosted engine; transcript inserts at cursor.

### 3. Speech-to-text — offline (whisper.cpp)
- [ ] macOS (Metal) / Windows (CPU/CUDA) / Linux X11 / Linux Wayland (CPU/CUDA/Vulkan):
  a downloaded model transcribes offline; transcript inserts at cursor.

### 4. Text-to-speech for selected text
- [ ] macOS / Windows / Linux X11 / Linux Wayland: selected text is read aloud.

### 5. Presets, prompt overrides, provider configuration, secure key storage
- [ ] macOS (Keychain) / Windows (Credential Manager) / Linux (libsecret): presets +
  prompt overrides apply; provider/base-url config works; the API key persists in the
  OS secret store and survives relaunch.

### 6. Tray, settings UI, first-run permissions, autostart
- [ ] Tray icon + menu (Settings/Quit) on all four targets.
- [ ] Settings window round-trips all settings on all four targets.
- [ ] First-run permission flow: macOS Accessibility + Microphone prompts; Windows mic
  prompt; Linux uinput udev guidance + Wayland portal consent.
- [ ] Autostart toggle works: LaunchAgent (macOS), Run key (Windows), XDG autostart
  (Linux); survives reboot.

### 7. macOS-only enhancements (informational, not parity-gating)
- [ ] Services menu "Rewrite with Vox" / "Speak with Vox" appears and works.
- [ ] Accessibility insertion path used when granted; falls back to clipboard otherwise.
- [ ] Notch/overlay polish renders correctly.

### 8. Signed/packaged installers per OS with auto-update
- [ ] macOS: notarized, stapled `.dmg`/`.app`; Gatekeeper opens it without warning.
- [ ] Windows: signed MSI + NSIS; SmartScreen shows no unknown-publisher warning
  (pending cert procurement, spec §9 — record as blocked if cert not yet purchased).
- [ ] Linux: AppImage runs; `.deb` installs and launches.
- [ ] Auto-update: installing N then publishing N+1 updates the running app via the
  Tauri updater (signature verified against the configured public key).

## Sign-off
- [ ] All non-n/a boxes checked on all four targets (item 8 Windows may be recorded as
  blocked-on-cert per spec §9). When complete, the Swift app may be retired (Task 10).
```

- [ ] **Step 2: Update the cross-platform app README**

Edit `app/README.md` to add Release + Distribution + Permissions sections (append below
the Phase 0 content):
```markdown
## Release

1. Add a `CHANGELOG.md` entry: `## [x.y.z] - YYYY-MM-DD`.
2. Run `./scripts/release.sh x.y.z` (bumps `src-tauri/tauri.conf.json`, runs tests +
   UI build, commits, and tags `app-vx.y.z`).
3. `git push origin main && git push origin app-vx.y.z` — the **App Release** workflow
   builds, signs, and publishes per-OS installers + the updater manifest.

## Distribution & signing

- Targets: macOS `.dmg`/`.app` (notarized), Windows MSI + NSIS, Linux AppImage + `.deb`.
- Updater: configured in `src-tauri/tauri.conf.json` (`plugins.updater`); signed with the
  key pair from `cargo tauri signer generate`. Private key + password are CI secrets
  (`TAURI_SIGNING_PRIVATE_KEY[_PASSWORD]`).
- Signing secrets (CI): macOS `APPLE_*`; Windows `WINDOWS_CERTIFICATE[_PASSWORD]`
  (cert procurement tracked per design §9 — builds are unsigned until provided).

## Permissions (first run)

- macOS: Accessibility (synthetic paste / AX insertion) + Microphone prompts.
- Windows: Microphone privacy prompt.
- Linux: `/dev/uinput` access for Wayland synthetic input (add user to `input`, install
  the `99-vox-uinput.rules` udev rule shown in the in-app guidance) + Wayland
  GlobalShortcuts portal consent.

## Parity

Full parity is gated by `docs/cross-platform/PARITY-CHECKLIST.md`.
```

- [ ] **Step 3: Update the root README pointer**

Edit the existing pointer block in `README.md` (added in Phase 0) to reflect that the
cross-platform app has reached distribution:
```markdown
> The cross-platform rewrite (macOS/Windows/Linux) under `app/` is feature-complete and
> distributed via the **App Release** workflow (signed installers + auto-update). See
> `docs/cross-platform/` and `docs/cross-platform/PARITY-CHECKLIST.md`. The Swift app is
> retired once the parity checklist passes (see below).
```

- [ ] **Step 4: Commit**

```bash
git add docs/cross-platform/PARITY-CHECKLIST.md app/README.md README.md
git commit -m "docs: add parity acceptance checklist and distribution docs"
```

---

## Task 10: Retire the Swift app (after parity is verified)

**Files:**
- Modify: `README.md`
- Modify: `Makefile`
- Modify: `.github/workflows/release.yml`

**Do this task ONLY after `docs/cross-platform/PARITY-CHECKLIST.md` is fully signed off.**

- [ ] **Step 1: Verify parity is signed off (gate)**

Confirm every non-n/a box in `docs/cross-platform/PARITY-CHECKLIST.md` is checked
(item 8 Windows may be `blocked-on-cert` per spec §9). Do not proceed otherwise.

- [ ] **Step 2: Mark the legacy release workflow as retired**

Edit `.github/workflows/release.yml` to add `workflow_dispatch` and a deprecation notice
so the Swift `v*` pipeline stops auto-running but remains available for hotfixes during
the transition:
```yaml
name: Release (legacy Swift app — retired)

on:
  workflow_dispatch: {}

# The cross-platform app ships via .github/workflows/app-release.yml.
# This workflow is retained for emergency Swift-app hotfixes only.
```

- [ ] **Step 3: Document retirement in the root README**

Edit `README.md`: replace the opening description so the cross-platform app is the
product. Add a clear note:
```markdown
> **Status:** The native Swift macOS app has been retired in favor of the cross-platform
> Tauri app under `app/` (macOS, Windows, Linux), which reached full parity per
> `docs/cross-platform/PARITY-CHECKLIST.md`. Build/run/release instructions for the
> current app are in `app/README.md`. The Swift sources (`VoxNative.xcodeproj`, `Makefile`
> native targets, `scripts/*.sh`, `.github/workflows/release.yml`) remain in history for
> reference and emergency hotfixes only.
```

- [ ] **Step 4: Verify the legacy workflow no longer auto-triggers**

Run:
```bash
python3 -c "import yaml; d=yaml.safe_load(open('.github/workflows/release.yml')); assert 'workflow_dispatch' in d['on'] and 'push' not in d['on']; print('legacy release workflow retired OK')"
```
Expected: prints `legacy release workflow retired OK` (no `push`/tag trigger remains).

- [ ] **Step 5: Commit**

```bash
git add README.md .github/workflows/release.yml Makefile
git commit -m "chore: retire the Swift app in favor of the cross-platform Tauri app"
```

---

## Phase 5 Acceptance

- `cargo test --manifest-path app/Cargo.toml -p vox-platform system` passes (eight pure
  helper tests) and `-p vox-platform macos_services` passes on macOS.
- `cargo test --manifest-path app/Cargo.toml -p vox-core settings` passes including the
  Phase 5 schema bump to v6.
- `bash app/scripts/version_test.sh` (during Task 7) prints `ALL VERSION SCRIPT TESTS
  PASSED`; `app/scripts/release.sh` passes `bash -n` syntax check.
- `cargo tauri build` produces installers for the host OS under
  `app/src-tauri/target/release/bundle/`; the **App Release** workflow produces signed
  (where certs present) per-OS installers + `latest.json` and a GitHub Release on an
  `app-v*` tag.
- Autostart, desktop notifications, and first-run permission flows work per OS (manual
  smoke checks in Tasks 2 and 5).
- macOS Services entry point and Accessibility insertion enhancement work on macOS
  (manual smoke check in Task 3), with clipboard+paste retained as the baseline (D3).
- `docs/cross-platform/PARITY-CHECKLIST.md` is fully signed off across macOS, Windows,
  Linux X11, and Linux Wayland; the Swift app is retired (Task 10).

## Self-Review Notes

- **Spec coverage (§8 mapping):** Every Phase 5 / §8 acceptance item is implemented and
  verified:
  - §8 *Rewrite via global hotkey + in-place replacement (all OSes, X11+Wayland)* →
    parity checklist item 1 (reuses Phase 1 `platform::hotkeys`/`textio`; macOS gains the
    AX enhancement in Task 3, D3 clipboard baseline retained).
  - §8 *STT remote + offline (whisper.cpp), all OSes* → checklist items 2–3 (Phases 2–3
    features verified across the matrix).
  - §8 *TTS for selected text, all OSes* → checklist item 4 (Phase 4 feature verified).
  - §8 *Presets, prompt overrides, provider config, secure key storage, all OSes* →
    checklist item 5.
  - §8 *Tray, settings UI, first-run permissions, autostart, all OSes* → Tasks 1–2, 4–5
    (`platform::system` autostart/notifications/permissions + UI) and checklist item 6.
  - §8 *Signed/packaged installers per OS with auto-update* → Tasks 6–8 (bundler targets,
    updater, signing/notarization, `tauri-action` release workflow) and checklist item 8.
  - macOS extras (Services menu, notch/overlay polish, AX insertion) → Task 3, checklist
    item 7 (explicitly informational, not parity-gating, since D3 is the baseline).
  - Spec §9 risks addressed: Windows code-signing cert tracked as procurement item
    (Prereqs + Tasks 6/8 keep builds working unsigned); Wayland uinput udev rule +
    portal consent surfaced in `permission_guidance` and the checklist; clipboard-restore
    race mitigated by the AX insertion alternative on macOS.
- **Type/name consistency vs CONTRACTS.md:** `vox-platform` crate + `src/system.rs` path
  match CONTRACTS.md (`system.rs` listed as "Phase 5: autostart, notifications,
  permissions"). `AxTextIo` implements the canonical `platform::textio::TextIo` trait
  (`capture_selection`/`replace_selection`/`insert_text`, `TextIoError`) and reuses
  `ClipboardTextIo::new()` as fallback — exact names from CONTRACTS.md. Secret service /
  identifier `com.voxapp.rewrite` matches Phase 0. `Settings.launch_at_login` is the
  Phase 0 field (no rename); `schema_version` bumped following the additive convention.
  New Tauri commands are snake_case (`set_autostart`, `get_autostart`, `notify`,
  `permission_status`, `request_permission`) and listed alongside the canonical
  `load_settings`/`save_settings`/`set_secret`/`has_secret` from CONTRACTS.md; events use
  the documented `"namespace:thing"` style (`"service:rewrite"`, `"service:speak"`,
  `"permissions:show"`).
- **No placeholders:** Every code/config/command/YAML/TOML/JSON block is complete and
  runnable. The only intentional fill-ins are operator-supplied secrets and the updater
  public key/endpoint (`PASTE_UPDATER_PUBLIC_KEY_HERE`, `https://github.com/voxapp/...`),
  which are explicitly described where they are generated (Prereqs Step 1) and pasted
  (Task 6 Step 1) — not "TBD". Inherently-manual steps (signing, notarization, OS
  permission prompts, installer + auto-update smoke tests, the parity matrix) ship exact
  commands and explicit expected results instead of unit tests, per the plan format.
```