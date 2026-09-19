//! `AVSpeechSynthesizer`, rendered to PCM rather than spoken.
//!
//! # Why the buffer callback and not `speakUtterance:`
//!
//! `speakUtterance:` sends audio straight to the output device. That is one
//! line of code and it bypasses [`crate::audio::playback`] completely: no cue
//! sounds, no read-aloud state machine, nothing that could later save the
//! audio to a file. `writeUtterance:toBufferCallback:` (macOS 13+) hands the
//! samples back instead, so the system voices go down the same path as every
//! other engine's.
//!
//! # Threading
//!
//! The write call returns immediately and the buffers arrive later, so they
//! are accumulated under a mutex and the caller waits on a condvar. Delivery
//! ends with a zero-length buffer; a bounded wait covers the case where that
//! never arrives, because blocking a blocking-pool thread forever is the one
//! failure mode with no way out.
//!
//! **Measured, and not what the documentation implies:** the callback is
//! delivered through the *main* run loop, not an arbitrary queue. Rendering
//! therefore only completes while something is servicing the main thread —
//! which the app always is, and which `cargo test` never is. That is why
//! there is no unit test here that actually renders: it would pass nowhere
//! and hang for the full timeout. Verified by hand with a main-thread run
//! loop pumped alongside the render (180 voices listed, 34 415 samples at
//! 22 050 Hz for a one-sentence utterance).

use std::ffi::c_float;
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{class, msg_send, AnyThread};
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioPCMBuffer, AVSpeechSynthesisVoice, AVSpeechSynthesisVoiceQuality,
    AVSpeechSynthesizer, AVSpeechUtterance, AVSpeechUtteranceDefaultSpeechRate,
    AVSpeechUtteranceMaximumSpeechRate, AVSpeechUtteranceMinimumSpeechRate,
};
use objc2_foundation::NSString;

use super::{SystemTtsError, SystemTtsInference, SystemVoice};
use crate::audio::{downmix_to_mono, PcmFrame};

/// How long to wait for the terminal (zero-length) buffer before giving up.
/// Generous: a premium voice rendering a long selection is genuinely slow.
const RENDER_TIMEOUT: Duration = Duration::from_secs(120);

pub struct MacSystemTts;

impl MacSystemTts {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacSystemTts {
    fn default() -> Self {
        Self::new()
    }
}

fn quality_name(quality: AVSpeechSynthesisVoiceQuality) -> &'static str {
    match quality {
        AVSpeechSynthesisVoiceQuality::Enhanced => "enhanced",
        AVSpeechSynthesisVoiceQuality::Premium => "premium",
        _ => "default",
    }
}

/// Maps a rate *multiplier* onto the absolute 0..1 scale AVFoundation uses,
/// where 0.5 is the natural pace. Clamped to the framework's own bounds: a
/// rate outside them is silently ignored by AVFoundation, which would look
/// like the speed setting doing nothing.
fn utterance_rate(speed: f32) -> f32 {
    let (min, max, default) = unsafe {
        (
            AVSpeechUtteranceMinimumSpeechRate,
            AVSpeechUtteranceMaximumSpeechRate,
            AVSpeechUtteranceDefaultSpeechRate,
        )
    };
    let speed = if speed.is_finite() && speed > 0.0 {
        speed
    } else {
        1.0
    };
    (default * speed).clamp(min, max)
}

/// Copies one delivered buffer out as interleaved f32.
///
/// `floatChannelData` is planar for a non-interleaved format and interleaved
/// otherwise, but in both cases sample `(channel, frame)` is at
/// `channels[channel][frame * stride]` — so one loop covers both, and the
/// result is the interleaved layout [`downmix_to_mono`] expects.
///
/// # Safety
/// `pcm` must be a live `AVAudioPCMBuffer` whose format is 32-bit float.
unsafe fn interleaved_samples(pcm: &AVAudioPCMBuffer) -> Option<(Vec<f32>, usize, u32)> {
    let format = unsafe { pcm.format() };
    let channels = unsafe { format.channelCount() } as usize;
    let sample_rate = unsafe { format.sampleRate() } as u32;
    let frames = unsafe { pcm.frameLength() } as usize;
    let stride = unsafe { pcm.stride() };

    let data: *mut NonNull<c_float> = unsafe { pcm.floatChannelData() };
    // Null for any non-float format. Nothing to convert and nothing to
    // report: the terminal buffer is recognised by its zero frame count.
    if data.is_null() || channels == 0 || frames == 0 {
        return Some((Vec::new(), channels.max(1), sample_rate));
    }

    let mut interleaved = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        for channel in 0..channels {
            let plane = unsafe { *data.add(channel) };
            interleaved.push(unsafe { *plane.as_ptr().add(frame * stride) });
        }
    }
    Some((interleaved, channels, sample_rate))
}

/// What the callback accumulates across however many buffers arrive.
#[derive(Default)]
struct Rendered {
    interleaved: Vec<f32>,
    channels: usize,
    sample_rate_hz: u32,
    done: bool,
}

fn find_voice(identifier: &str) -> Option<Retained<AVSpeechSynthesisVoice>> {
    let ns = NSString::from_str(identifier);
    unsafe { AVSpeechSynthesisVoice::voiceWithIdentifier(&ns) }
}

/// Renders one utterance, blocking until the synthesizer signals the end of
/// delivery. Runs on a blocking-pool thread; none of the ObjC objects escape.
fn render_blocking(
    text: &str,
    voice_id: Option<String>,
    speed: f32,
) -> Result<PcmFrame, SystemTtsError> {
    let voice = match voice_id {
        Some(id) => Some(find_voice(&id).ok_or(SystemTtsError::UnknownVoice(id))?),
        None => None,
    };

    let utterance = unsafe {
        AVSpeechUtterance::initWithString(AVSpeechUtterance::alloc(), &NSString::from_str(text))
    };
    if let Some(voice) = voice.as_deref() {
        unsafe { utterance.setVoice(Some(voice)) };
    }
    unsafe { utterance.setRate(utterance_rate(speed)) };

    let state = Arc::new((Mutex::new(Rendered::default()), Condvar::new()));
    let sink = state.clone();

    // Fn, not FnOnce: the framework calls this once per buffer and finishes
    // with an empty one.
    let block = RcBlock::new(move |buffer: NonNull<AVAudioBuffer>| {
        let buffer = unsafe { buffer.as_ref() };
        let is_pcm: bool = unsafe { msg_send![buffer, isKindOfClass: class!(AVAudioPCMBuffer)] };
        let (mut chunk, channels, sample_rate) = if is_pcm {
            let pcm = unsafe { &*(buffer as *const AVAudioBuffer as *const AVAudioPCMBuffer) };
            match unsafe { interleaved_samples(pcm) } {
                Some(parts) => parts,
                None => (Vec::new(), 1, 0),
            }
        } else {
            (Vec::new(), 1, 0)
        };

        let (lock, cvar) = &*sink;
        let Ok(mut rendered) = lock.lock() else {
            return;
        };
        if chunk.is_empty() {
            // An empty buffer is the end-of-delivery signal.
            rendered.done = true;
            cvar.notify_all();
            return;
        }
        if rendered.channels == 0 {
            rendered.channels = channels;
            rendered.sample_rate_hz = sample_rate;
        }
        rendered.interleaved.append(&mut chunk);
    });

    let synthesizer = unsafe { AVSpeechSynthesizer::new() };
    unsafe {
        synthesizer.writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&block));
    }

    let (lock, cvar) = &*state;
    let guard = lock
        .lock()
        .map_err(|_| SystemTtsError::Other("speech render state was poisoned".into()))?;
    let (rendered, timeout) = cvar
        .wait_timeout_while(guard, RENDER_TIMEOUT, |state| !state.done)
        .map_err(|_| SystemTtsError::Other("speech render state was poisoned".into()))?;
    if timeout.timed_out() && rendered.interleaved.is_empty() {
        // Nearly always means nothing is servicing the main run loop —
        // see the threading note at the top of this module.
        return Err(SystemTtsError::Other(format!(
            "the system synthesizer delivered nothing within {}s \
             (its buffer callback needs the main run loop to be running)",
            RENDER_TIMEOUT.as_secs()
        )));
    }

    if rendered.interleaved.is_empty() || rendered.sample_rate_hz == 0 {
        return Err(SystemTtsError::NoAudio);
    }

    Ok(PcmFrame {
        samples: downmix_to_mono(&rendered.interleaved, rendered.channels.max(1), |s| s),
        sample_rate_hz: rendered.sample_rate_hz,
    })
}

#[async_trait]
impl SystemTtsInference for MacSystemTts {
    fn voices(&self) -> Vec<SystemVoice> {
        // Personal Voice is deliberately absent: it needs an authorization
        // call and an Info.plist usage string, and is not worth either until
        // someone asks for it.
        let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
        voices
            .iter()
            .map(|voice| SystemVoice {
                id: unsafe { voice.identifier() }.to_string(),
                name: unsafe { voice.name() }.to_string(),
                language: unsafe { voice.language() }.to_string(),
                quality: quality_name(unsafe { voice.quality() }).to_string(),
            })
            .collect()
    }

    async fn synthesize(
        &self,
        text: &str,
        voice_id: Option<&str>,
        speed: f32,
    ) -> Result<PcmFrame, SystemTtsError> {
        let text = text.to_string();
        let voice_id = voice_id.map(str::to_string);
        tokio::task::spawn_blocking(move || render_blocking(&text, voice_id, speed))
            .await
            .map_err(|e| SystemTtsError::Other(format!("speech render task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rate maps onto AVFoundation's absolute scale, where 0.5 is normal
    /// — not onto our 0.5..2.0 multiplier, which the framework would clamp
    /// away to "as fast as possible" without saying so.
    #[test]
    fn the_speed_multiplier_lands_on_the_frameworks_own_scale() {
        let normal = utterance_rate(1.0);
        assert!((normal - unsafe { AVSpeechUtteranceDefaultSpeechRate }).abs() < f32::EPSILON);
        assert!(utterance_rate(0.5) < normal);
        assert!(utterance_rate(2.0) > normal);
        let (min, max) = unsafe {
            (
                AVSpeechUtteranceMinimumSpeechRate,
                AVSpeechUtteranceMaximumSpeechRate,
            )
        };
        for speed in [0.0, -1.0, f32::NAN, 100.0, 0.001] {
            let rate = utterance_rate(speed);
            assert!(rate >= min && rate <= max, "{speed} produced {rate}");
        }
    }

    /// Asking for a voice the machine does not have must name it, not fall
    /// back silently to a different voice.
    #[test]
    fn an_unknown_voice_identifier_is_reported() {
        let err = render_blocking("hello", Some("com.example.not.a.voice".into()), 1.0)
            .expect_err("an unknown identifier must not resolve");
        assert!(matches!(err, SystemTtsError::UnknownVoice(ref id) if id.contains("not.a.voice")));
    }

    /// The machine running this always has at least the built-in voices, and
    /// every row has to be complete enough to show in a picker.
    #[test]
    fn the_system_lists_usable_voices() {
        let voices = MacSystemTts::new().voices();
        assert!(!voices.is_empty(), "macOS always ships system voices");
        for voice in &voices {
            assert!(!voice.id.is_empty());
            assert!(!voice.name.is_empty());
            assert!(!voice.language.is_empty());
            assert!(["default", "enhanced", "premium"].contains(&voice.quality.as_str()));
        }
    }
}
