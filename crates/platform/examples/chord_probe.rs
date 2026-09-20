//! Posts a synthetic ⌥⇧ hold, so the hold-to-talk tap can be tested without a
//! human at the keyboard.
//!
//! Two processes: this one posts, KEA observes. Run KEA with its tap
//! instrumented, put it in the background or the foreground, then run this and
//! read KEA's log. That is the only way to answer "does the tap see the chord
//! while another app is focused?" from a script.
//!
//! `System Events`' `key down option` is NOT a substitute: it does not produce
//! the `FlagsChanged` CGEvent a tap watches, which is why an earlier attempt at
//! this test reported nothing in both directions and proved nothing.
//!
//! Usage: cargo run -p kea-platform --example chord_probe [hold_ms]

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

fn post_flags(source: &CGEventSource, flags: CGEventFlags, label: &str) {
    match CGEvent::new(source.clone()) {
        Ok(event) => {
            event.set_type(CGEventType::FlagsChanged);
            event.set_flags(flags);
            event.post(CGEventTapLocation::HID);
            println!("posted {label:<18} flags={flags:?}");
        }
        Err(()) => eprintln!("could not create a CGEvent for {label} — is this process trusted?"),
    }
}

fn main() {
    let hold_ms: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1200);

    let source = match CGEventSource::new(CGEventSourceStateID::HIDSystemState) {
        Ok(s) => s,
        Err(()) => {
            eprintln!("could not create an event source; grant Accessibility to this terminal");
            std::process::exit(1);
        }
    };

    println!("posting ⌥ down, then ⌥⇧ down, holding {hold_ms}ms, then releasing");
    post_flags(&source, CGEventFlags::CGEventFlagAlternate, "option down");
    std::thread::sleep(std::time::Duration::from_millis(120));
    post_flags(
        &source,
        CGEventFlags::CGEventFlagAlternate | CGEventFlags::CGEventFlagShift,
        "option+shift down",
    );
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));
    post_flags(&source, CGEventFlags::CGEventFlagAlternate, "shift up");
    std::thread::sleep(std::time::Duration::from_millis(60));
    post_flags(&source, CGEventFlags::empty(), "all up");
    println!("done");
}
