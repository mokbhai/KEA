//! Prints everything the paste path checks before posting a keystroke.
//!
//! Run with `cargo run -p kea-platform --example paste_probe` when a user
//! reports that dictation types nothing. It answers the four questions the OS
//! will not answer during a real run, without needing a dictation to be in
//! flight, and it posts no keystrokes of its own.
//! macOS only: every signal it prints is a macOS API. On other platforms it
//! compiles to a note rather than breaking `--all-targets` builds.
#[cfg(target_os = "macos")]
use kea_platform::textio::{macos_ax, macos_keys, macos_pasteboard};

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("paste_probe only has anything to say on macOS.");
}

#[cfg(target_os = "macos")]
fn main() {
    println!("ax_trusted:     {}", macos_ax::is_ax_trusted());
    println!("secure_input:   {}", macos_keys::secure_input_enabled());
    println!(
        "held_modifiers: {}",
        macos_keys::describe_modifiers(macos_keys::current_modifier_flags())
    );
    println!("focus:          {}", macos_ax::focus_summary());
    println!("change_count:   {:?}", macos_pasteboard::change_count());
}
