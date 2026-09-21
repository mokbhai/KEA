use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use kea_platform::{HoldAction, spawn_hold_to_talk};

#[tokio::test(flavor = "multi_thread")]
async fn test_hold_to_talk_background() {
    // macOS only
    if !cfg!(target_os = "macos") {
        return;
    }
    
    // Accessibility permissions are required for CGEventTap
    if !kea_platform::textio::macos_ax::is_ax_trusted() {
        println!("Skipping test: the test runner is not trusted for macOS Accessibility");
        return;
    }

    // Spawn the hold to talk listener
    let enabled = Arc::new(AtomicBool::new(true));
    let (_control, mut rx) = spawn_hold_to_talk(enabled).expect("Failed to spawn hold to talk");

    // Launch the child process (chord-probe) to post the exact synthetic event sequence.
    // Notice that running chord-probe does not make the test executable frontmost,
    // which validates our requirement that gestures work globally.
    let probe_bin = env!("CARGO_BIN_EXE_chord-probe");
    let hold_ms = 1200; // sufficiently long to pass min-hold
    
    let mut child = Command::new(probe_bin)
        .arg(hold_ms.to_string())
        .spawn()
        .expect("Failed to launch chord-probe");

    let mut actions = Vec::new();
    
    // We expect: Arm, Start, Stop, and maybe Disarm
    let poll_timeout = timeout(Duration::from_millis(hold_ms + 1000), async {
        while let Some(action) = rx.recv().await {
            actions.push(action);
            if action == HoldAction::Stop || action == HoldAction::Disarm {
                break;
            }
        }
    });

    poll_timeout.await.unwrap_or_else(|_| {
        child.kill().ok();
        panic!("Timed out waiting for hold-to-talk actions; received so far: {:?}", actions);
    });

    child.wait().unwrap();

    assert!(actions.contains(&HoldAction::Arm), "Missing Arm action. Got: {:?}", actions);
    assert!(actions.contains(&HoldAction::Start), "Missing Start action. Got: {:?}", actions);
    assert!(actions.contains(&HoldAction::Stop), "Missing Stop action. Got: {:?}", actions);
}
