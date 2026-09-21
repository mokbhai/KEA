//! macOS driver for the hold-to-talk chord: a listen-only `CGEventTap`.
//!
//! Carbon's `RegisterEventHotKey` — what [`super::macos`] uses — cannot
//! register a modifier-only combination, and reports presses only, never
//! releases. Both are fatal for press-and-hold, so this watches the raw event
//! stream instead and hands modifier snapshots to [`HoldToTalk`], which owns
//! every decision.
//!
//! The tap is **listen-only**: it must never swallow ⌥⇧, because those keys
//! belong to the user's own shortcuts. It subscribes to exactly two event
//! types, and of the two it reads only the modifier flag word — a `KeyDown`'s
//! keycode and character are never touched, only the fact that some key went
//! down, which is what disqualifies ⌥⇧+arrow from arming a recording.
//!
//! # Manual verification
//! 1. Grant Accessibility (without it `CGEventTapCreate` returns NULL and this
//!    refuses to start, with the same message the paste path uses).
//! 2. Hold ⌥⇧ for half a second: the dictation HUD appears and the mic opens.
//! 3. Release either key: the HUD switches to transcribing and the text lands.
//! 4. Tap ⌥⇧ quickly, and separately hold ⌥⇧ and press ← a few times: neither
//!    may start a recording.
//! 5. Tap ⌥⇧ twice quickly: the HUD shows a locked recording that survives the
//!    keys coming up. Tap once more to transcribe, or press Escape to discard.
//! 6. Headless CI cannot install a tap or deliver key events; the decision
//!    logic is covered by [`super::hold`]'s tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPortRef;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult,
};
use tokio::sync::mpsc;

use super::hold::{HoldAction, HoldModifiers, HoldToTalk};
use super::{HoldControl, HotkeyError};

// The tap's own mach port, so the callback can switch the tap back on.
//
// macOS disables a tap that misbehaves (or that the user disables with the
// secure-input escape hatch) and reports it as an out-of-band event rather than
// an error. Left unhandled, hold-to-talk would quietly stop working for the rest
// of the session — the same shape of silent failure as the paste bug. A
// thread-local rather than a captured field because `CGEventTap::new` requires a
// `Send + 'static` callback and `CFMachPort` is neither; the tap, its callback
// and this cell all live on the one thread that owns the run loop.
thread_local! {
    static TAP_PORT: std::cell::Cell<CFMachPortRef> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    // `core-graphics` keeps this private behind `CGEventTap::enable`, which
    // needs the tap object the callback cannot reach.
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

/// Installs the hold-to-talk listener.
///
/// `enabled` is read on every event so the settings toggle takes effect without
/// tearing anything down. The listener is deliberately *not* stoppable: ending
/// a `CFRunLoop` from another thread needs a source or timer installed purely to
/// carry the request, and the tap is inert while disabled (it resets the state
/// machine and returns before reading anything). It goes away when KEA quits.
pub fn spawn(
    enabled: Arc<AtomicBool>,
) -> Result<(HoldControl, mpsc::UnboundedReceiver<HoldAction>), HotkeyError> {
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    // The tap is created on the run-loop thread, so its success or failure has
    // to travel back here before `spawn` can answer.
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

    let machine = Arc::new(Mutex::new(HoldToTalk::new()));

    spawn_tap_thread(
        machine.clone(),
        enabled,
        events_tx.clone(),
        wake_tx,
        started_tx,
    );

    match started_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => return Err(HotkeyError::Other(message)),
        Err(_) => {
            return Err(HotkeyError::Other(
                "hold-to-talk listener thread died before it reported readiness".into(),
            ))
        }
    }

    spawn_deadline_thread(machine.clone(), wake_rx, events_tx);
    Ok((HoldControl::new(machine), events_rx))
}

/// Said in one place because it is said from two: the pre-flight trust check
/// and the `CGEventTapCreate` failure path mean the same thing to a user.
const ACCESSIBILITY_REQUIRED: &str =
    "KEA needs Accessibility permission to watch for the hold-to-talk chord (⌥⇧). \
Grant it in System Settings → Privacy & Security → Accessibility, then quit and \
reopen KEA. Cmd+Shift+D keeps working without it.";

fn spawn_tap_thread(
    machine: Arc<Mutex<HoldToTalk>>,
    enabled: Arc<AtomicBool>,
    events: mpsc::UnboundedSender<HoldAction>,
    wake: std::sync::mpsc::Sender<()>,
    started: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    std::thread::Builder::new()
        .name("kea-hold-to-talk".into())
        .spawn(move || {
            let tap = CGEventTap::new(
                CGEventTapLocation::HID,
                CGEventTapPlacement::HeadInsertEventTap,
                // Never Default: an active tap can drop events, and dropping
                // the user's ⌥⇧ would break every shortcut built on it.
                CGEventTapOptions::ListenOnly,
                vec![CGEventType::KeyDown, CGEventType::FlagsChanged],
                move |_proxy, event_type, event| {
                    on_event(&machine, &enabled, &events, &wake, event_type, event);
                    CallbackResult::Keep
                },
            );

            // Checked BEFORE the tap is trusted to report it, because it does
            // not: `CGEventTapCreate` SUCCEEDS for an untrusted process. What
            // fails is every subsequent keystroke — the system disables the tap
            // with `TapDisabledByUserInput` and the chord silently does
            // nothing, while `Cmd+Shift+D` keeps working because a Carbon
            // hotkey needs no Accessibility.
            //
            // That asymmetry cost a long debugging session: the only symptom
            // was a warning in a log file that read like noise.
            if !crate::textio::macos_ax::is_ax_trusted() {
                let _ = started.send(Err(ACCESSIBILITY_REQUIRED.into()));
                return;
            }

            let tap = match tap {
                Ok(tap) => tap,
                Err(()) => {
                    // The only realistic cause: this process is not trusted for
                    // Accessibility. CGEventTapCreate says so by returning NULL.
                    let _ = started.send(Err(ACCESSIBILITY_REQUIRED.into()));
                    return;
                }
            };

            let source = match tap.mach_port().create_runloop_source(0) {
                Ok(source) => source,
                Err(()) => {
                    let _ = started.send(Err(
                        "could not attach the hold-to-talk tap to a run loop".into(),
                    ));
                    return;
                }
            };
            CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            TAP_PORT.with(|slot| slot.set(tap.mach_port().as_concrete_TypeRef()));

            let _ = started.send(Ok(()));
            // Never returns; `tap` stays alive (and enabled) for the process.
            CFRunLoop::run_current();
        })
        .expect("spawning the hold-to-talk listener thread");
}

/// Why the system disabled the tap, or `None` for an ordinary event.
///
/// Extracted from the callback purely so it can be tested: the callback itself
/// needs a live `CGEvent`, which a unit test has no way to make. The mapping is
/// the part that was wrong — both reasons shared one message, so a log could
/// not say whether the process had been throttled or something had switched
/// the tap off.
fn disable_reason(event_type: CGEventType) -> Option<&'static str> {
    match event_type {
        CGEventType::TapDisabledByTimeout => Some("timeout"),
        CGEventType::TapDisabledByUserInput => Some("user input"),
        _ => None,
    }
}

fn on_event(
    machine: &Mutex<HoldToTalk>,
    enabled: &AtomicBool,
    events: &mpsc::UnboundedSender<HoldAction>,
    wake: &std::sync::mpsc::Sender<()>,
    event_type: CGEventType,
    event: &core_graphics::event::CGEvent,
) {
    if let Some(reason) = disable_reason(event_type) {
        TAP_PORT.with(|slot| {
            let port = slot.get();
            if !port.is_null() {
                unsafe { CGEventTapEnable(port, true) };
            }
        });

        // Name the reason. They are different faults with different fixes and
        // the old message covered both, which cost a debugging session:
        //
        // * Timeout   — our callback missed its deadline. Almost always the
        //   process being throttled rather than the callback being slow; see
        //   `kea_platform::appnap`.
        // * UserInput — something called `CGEventTapEnable(false)`. Nothing in
        //   KEA ever does, so this means another process or the system did.
        if reason == "user input" && !crate::textio::macos_ax::is_ax_trusted() {
            // The signature of a revoked grant: the tap survives, every real
            // keystroke kills it. ERROR rather than WARN because this one is
            // actionable and the user is otherwise told nothing at all.
            tracing::error!("{ACCESSIBILITY_REQUIRED}");
        } else {
            tracing::warn!(
                reason,
                "hold-to-talk: the system disabled the event tap; re-enabled it"
            );
        }

        // Drop any half-finished chord. The tap was deaf for an unknown
        // interval, so a press whose release happened while it was disabled
        // would otherwise sit in the machine waiting for a release that has
        // already been and gone — and the deadline thread would eventually
        // fire a recording the user did not ask for.
        //
        // Safe to do unconditionally: `reset` on an idle machine is a no-op,
        // and the next FlagsChanged carries the absolute modifier state, so
        // there is nothing to rebuild by hand.
        let mut machine = machine.lock().unwrap_or_else(|p| p.into_inner());
        let armed = machine.is_armed();
        send(events, machine.reset());
        if armed {
            let _ = events.send(HoldAction::Disarm);
        }
        return;
    }

    let mut machine = machine.lock().unwrap_or_else(|p| p.into_inner());

    if !enabled.load(Ordering::Relaxed) {
        // Turning the mode off mid-hold must end that hold, not strand the
        // dictation run it started — nor an armed stream, which `reset` cannot
        // report alongside the abandoned recording.
        let armed = machine.is_armed();
        send(events, machine.reset());
        if armed {
            let _ = events.send(HoldAction::Disarm);
        }
        return;
    }

    let action = match event_type {
        CGEventType::FlagsChanged => {
            let flags = event.get_flags();
            let mods = HoldModifiers {
                option: flags.contains(CGEventFlags::CGEventFlagAlternate),
                shift: flags.contains(CGEventFlags::CGEventFlagShift),
            };
            // Debug rather than warn: one line per modifier press is far too
            // loud for normal running, but it is the only thing that answers
            // "did the tap see it?" — which is the first question every time
            // this feature is reported broken. `make dev` shows it.
            tracing::debug!(
                option = mods.option,
                shift = mods.shift,
                "hold-to-talk: flags"
            );
            machine.on_modifiers(Instant::now(), mods)
        }
        // Nothing is read off the event: only that a key went down at all.
        CGEventType::KeyDown => machine.on_other_key(),
        _ => HoldAction::Nothing,
    };
    drop(machine);

    send(events, action);
    // A deadline may have moved either way; let the timer recompute.
    let _ = wake.send(());
}

/// Forward a decision, dropping the one that means "no decision".
fn send(events: &mpsc::UnboundedSender<HoldAction>, action: HoldAction) -> bool {
    if action == HoldAction::Nothing {
        return true;
    }
    events.send(action).is_ok()
}

/// Waits out the machine's clock-driven decisions.
///
/// A hold that is never interrupted produces no further events, so the start
/// has to come from a clock — and so do the preroll arming and the hard cap on
/// a locked recording. It sleeps exactly as long as the machine says is left
/// rather than polling, and parks on the wake channel whenever nothing is
/// pending — which is all of the time the user is not holding the chord.
fn spawn_deadline_thread(
    machine: Arc<Mutex<HoldToTalk>>,
    wake: std::sync::mpsc::Receiver<()>,
    events: mpsc::UnboundedSender<HoldAction>,
) {
    std::thread::Builder::new()
        .name("kea-hold-to-talk-deadline".into())
        .spawn(move || loop {
            let remaining = machine
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .time_until_deadline(Instant::now());

            let keep_going = match remaining {
                Some(remaining) => !matches!(
                    wake.recv_timeout(remaining),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                ),
                None => wake.recv().is_ok(),
            };
            if !keep_going {
                return;
            }

            let action = machine
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .poll(Instant::now());
            if !send(&events, action) {
                return;
            }
        })
        .expect("spawning the hold-to-talk deadline thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two disable reasons are different faults with different fixes, and
    /// the log has to say which. A timeout means this process was too slow to
    /// answer — in practice, throttled; see `kea_platform::appnap`. "User
    /// input" means something called `CGEventTapEnable(false)`, and since
    /// nothing in KEA ever passes `false`, that something is not us.
    #[test]
    fn each_disable_reason_is_named_separately() {
        assert_eq!(
            disable_reason(CGEventType::TapDisabledByTimeout),
            Some("timeout")
        );
        assert_eq!(
            disable_reason(CGEventType::TapDisabledByUserInput),
            Some("user input")
        );
        assert_ne!(
            disable_reason(CGEventType::TapDisabledByTimeout),
            disable_reason(CGEventType::TapDisabledByUserInput),
            "one message for both is what made the real failure unreadable"
        );
    }

    /// The events the tap is actually subscribed to must not be mistaken for a
    /// disable, or every chord would reset the machine instead of driving it.
    #[test]
    fn an_ordinary_event_is_not_a_disable() {
        assert_eq!(disable_reason(CGEventType::FlagsChanged), None);
        assert_eq!(disable_reason(CGEventType::KeyDown), None);
    }
}
