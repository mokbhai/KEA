//! Measures how long `cpal` takes to hand back live microphone audio.
//!
//! This is the verification gate for the warm-mic preroll: arming capture on
//! the first modifier of the ⌥⇧ chord only buys anything if the stream is
//! delivering samples *before* the 350ms hold threshold passes. If opening a
//! stream costs more than the modifier transit time, the preroll ring would be
//! empty at exactly the moment it is supposed to be full, and the feature
//! should not be built.
//!
//! Run with `cargo run -p kea-platform --example mic_arm_probe`. It opens and
//! closes the default input device a few times and prints, per attempt:
//!
//! * `open`  — device lookup, config query and `build_input_stream`.
//! * `play`  — `Stream::play`, which is where CoreAudio actually starts the IO.
//! * `first` — wall time from the very start until the first buffer arrives.
//!
//! `first` is the number that decides the feature: it is how much of the
//! chord's transit time is spent waiting rather than recording.
//!
//! macOS only, like the capture path it measures; on other platforms it
//! compiles to a note so `--all-targets` builds stay green.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("mic_arm_probe only has anything to say on macOS.");
}

#[cfg(target_os = "macos")]
fn main() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc;
    use std::time::Instant;

    /// Enough attempts to separate the first (cold) open from the steady state,
    /// which is the one a real chord press would pay.
    const ATTEMPTS: usize = 5;

    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        println!("no default input device; grant Microphone access and try again.");
        return;
    };
    println!(
        "device: {}",
        device.name().unwrap_or_else(|_| "<unnamed>".into())
    );

    for attempt in 1..=ATTEMPTS {
        let started = Instant::now();

        let config = match device.default_input_config() {
            Ok(config) => config,
            Err(e) => {
                println!("attempt {attempt}: could not read the input config: {e}");
                return;
            }
        };
        let stream_config: cpal::StreamConfig = config.clone().into();

        // Only the timing matters here, so the callback does the least it can:
        // report the first buffer and ignore the rest. A `SyncSender` of
        // capacity 1 never blocks the audio thread.
        let (first_tx, first_rx) = mpsc::sync_channel::<Instant>(1);
        let stream = device.build_input_stream(
            &stream_config,
            move |_data: &[f32], _| {
                let _ = first_tx.try_send(Instant::now());
            },
            |err| eprintln!("stream error: {err}"),
            None,
        );
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                println!("attempt {attempt}: build_input_stream failed: {e}");
                return;
            }
        };
        let opened = started.elapsed();

        if let Err(e) = stream.play() {
            println!("attempt {attempt}: play failed: {e}");
            return;
        }
        let played = started.elapsed();

        let first = first_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .map(|at| at.duration_since(started));
        drop(stream);

        match first {
            Ok(first) => println!(
                "attempt {attempt}: open {:>7.2}ms  play {:>7.2}ms  first {:>7.2}ms",
                opened.as_secs_f64() * 1000.0,
                (played - opened).as_secs_f64() * 1000.0,
                first.as_secs_f64() * 1000.0,
            ),
            Err(_) => println!(
                "attempt {attempt}: open {:>7.2}ms  play {:>7.2}ms  first: none within 2s \
                 (is Microphone access granted to this binary?)",
                opened.as_secs_f64() * 1000.0,
                (played - opened).as_secs_f64() * 1000.0,
            ),
        }
    }

    println!();
    println!(
        "Arming is worth building when `first` is comfortably under the 350ms hold\n\
         threshold: everything below it is audio the preroll ring captures that the\n\
         current open-on-threshold path throws away."
    );
}
