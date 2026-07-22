//! macOS ScreenCaptureKit system-audio capture (feature `system-audio-sck`).
//!
//! Requires macOS 13+, Screen Recording permission, and building with
//! `--features system-audio-sck`. Default builds omit this module's native
//! implementation entirely.
//!
//! # Failure modes
//! - Permission denied → capture fails at `start()` with a descriptive error.
//! - Display disconnected → stream stops; check logs.
//! - SDK / API mismatch → feature build may stub; default mic-only path unchanged.

use super::{AudioIoError, PcmFrame};
use async_trait::async_trait;

/// Testable seam for system-audio capture (SCK or fakes in unit tests).
#[async_trait]
pub trait SystemAudioCapture: Send + Sync {
    async fn start(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;
    async fn stop(&mut self) -> Result<PcmFrame, AudioIoError>;
}

/// Returns whether ScreenCaptureKit system audio is compiled in.
pub fn sck_feature_enabled() -> bool {
    cfg!(all(target_os = "macos", feature = "system-audio-sck"))
}

/// Whether Screen Recording permission appears granted (macOS only).
pub fn screen_recording_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        core_graphics::access::ScreenCaptureAccess::default().preflight()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Stub used when the `system-audio-sck` feature is disabled.
pub struct UnavailableSystemAudioCapture;

#[async_trait]
impl SystemAudioCapture for UnavailableSystemAudioCapture {
    async fn start(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        Err(AudioIoError::Other(
            "ScreenCaptureKit system audio not compiled (enable system-audio-sck)".into(),
        ))
    }

    async fn stop(&mut self) -> Result<PcmFrame, AudioIoError> {
        Err(AudioIoError::Other(
            "ScreenCaptureKit system audio not compiled (enable system-audio-sck)".into(),
        ))
    }
}

#[cfg(all(target_os = "macos", feature = "system-audio-sck"))]
mod sck_impl {
    use super::*;
    use super::super::util::accumulate_frames;
    use screencapturekit::async_api::{AsyncSCShareableContent, AsyncSCStream};
    use screencapturekit::cm::CMSampleBufferExt;
    use screencapturekit::prelude::*;
    use screencapturekit::stream::output_type::SCStreamOutputType;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};

    const SCK_SAMPLE_RATE_HZ: u32 = 48_000;

    pub struct SckSystemAudioCapture {
        stop_tx: Option<std::sync::mpsc::Sender<()>>,
        join: Option<JoinHandle<()>>,
        buffered: Arc<Mutex<Vec<PcmFrame>>>,
        drops: Arc<AtomicU64>,
    }

    impl SckSystemAudioCapture {
        pub fn new() -> Self {
            Self {
                stop_tx: None,
                join: None,
                buffered: Arc::new(Mutex::new(Vec::new())),
                drops: Arc::new(AtomicU64::new(0)),
            }
        }

        fn audio_sample_to_pcm(sample: &screencapturekit::cm::CMSampleBuffer) -> Option<PcmFrame> {
            let list = sample.audio_buffer_list()?;
            let buffer = list.get(0)?;
            let bytes = buffer.data();
            if bytes.len() < 4 {
                return None;
            }
            let channels = buffer.number_channels.max(1) as usize;
            let sample_count = bytes.len() / (4 * channels);
            let mut mono = Vec::with_capacity(sample_count);
            for i in 0..sample_count {
                let base = i * channels * 4;
                let mut sum = 0.0f32;
                for ch in 0..channels {
                    let offset = base + ch * 4;
                    if offset + 4 > bytes.len() {
                        break;
                    }
                    let bits = [
                        bytes[offset],
                        bytes[offset + 1],
                        bytes[offset + 2],
                        bytes[offset + 3],
                    ];
                    sum += f32::from_le_bytes(bits);
                }
                mono.push(sum / channels as f32);
            }
            Some(PcmFrame {
                samples: mono,
                sample_rate_hz: SCK_SAMPLE_RATE_HZ,
            })
        }
    }

    #[async_trait]
    impl SystemAudioCapture for SckSystemAudioCapture {
        async fn start(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            if self.join.is_some() {
                return Err(AudioIoError::Other("SCK capture already active".into()));
            }
            if !screen_recording_granted() {
                return Err(AudioIoError::Other(
                    "Screen Recording permission required for system audio".into(),
                ));
            }

            self.buffered.lock().unwrap_or_else(|p| p.into_inner()).clear();
            let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(64);
            let (stop_tx, stop_rx) = std::sync::mpsc::channel();
            let buffered = Arc::clone(&self.buffered);
            let drops = Arc::clone(&self.drops);
            drops.store(0, Ordering::Relaxed);

            let join = thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime for SCK");

                if let Err(err) = rt.block_on(run_sck_loop(stop_rx, frame_tx, buffered, drops)) {
                    tracing::error!("SCK system audio capture failed: {err}");
                }
            });

            self.stop_tx = Some(stop_tx);
            self.join = Some(join);
            Ok(frame_rx)
        }

        async fn stop(&mut self) -> Result<PcmFrame, AudioIoError> {
            if let Some(tx) = self.stop_tx.take() {
                let _ = tx.send(());
            }
            if let Some(join) = self.join.take() {
                join.join()
                    .map_err(|_| AudioIoError::Other("SCK capture thread panicked".into()))?;
            }
            let frames = std::mem::take(&mut *self.buffered.lock().unwrap_or_else(|p| p.into_inner()));
            let drops = self.drops.swap(0, Ordering::Relaxed);
            if drops > 0 {
                tracing::warn!(drops, "SCK: {} system audio frames dropped (channel full) during this session", drops);
            }
            Ok(accumulate_frames(&frames))
        }
    }

    async fn run_sck_loop(
        stop_rx: std::sync::mpsc::Receiver<()>,
        frame_tx: tokio::sync::mpsc::Sender<PcmFrame>,
        buffered: Arc<Mutex<Vec<PcmFrame>>>,
        drops: Arc<AtomicU64>,
    ) -> Result<(), AudioIoError> {
        let content = AsyncSCShareableContent::get()
            .await
            .map_err(|e| AudioIoError::Other(format!("SCK shareable content: {e}")))?;

        let displays = content.displays();
        let display = displays.first().ok_or_else(|| {
            AudioIoError::Other("no display available for SCK capture".into())
        })?;

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();

        // Minimal video surface — we only consume audio samples.
        let config = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_captures_audio(true)
            .with_sample_rate(SCK_SAMPLE_RATE_HZ as i32)
            .with_channel_count(2);

        let stream = AsyncSCStream::new(&filter, &config, 32, SCStreamOutputType::Audio);

        stream
            .start_capture()
            .map_err(|e| AudioIoError::Other(format!("SCK start_capture: {e}")))?;

        loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }

            match stream.try_next() {
                Some(sample) => {
                    if let Some(frame) = SckSystemAudioCapture::audio_sample_to_pcm(&sample) {
                        buffered.lock().unwrap_or_else(|p| p.into_inner()).push(frame.clone());
                        if frame_tx.try_send(frame).is_err() {
                            let n = drops.fetch_add(1, Ordering::Relaxed) + 1;
                            if n % 100 == 0 {
                                tracing::warn!(drops = n, "SCK: system audio frame channel full; dropped {} frames so far", n);
                            }
                        }
                    }
                }
                None => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    if stream.is_closed() {
                        break;
                    }
                }
            }
        }

        stream
            .stop_capture()
            .map_err(|e| AudioIoError::Other(format!("SCK stop_capture: {e}")))?;

        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "system-audio-sck"))]
pub use sck_impl::SckSystemAudioCapture;

/// Construct the platform system-audio capture backend.
pub fn new_system_audio_capture() -> Box<dyn SystemAudioCapture> {
    #[cfg(all(target_os = "macos", feature = "system-audio-sck"))]
    {
        Box::new(SckSystemAudioCapture::new())
    }
    #[cfg(not(all(target_os = "macos", feature = "system-audio-sck")))]
    {
        Box::new(UnavailableSystemAudioCapture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSck;

    #[async_trait]
    impl SystemAudioCapture for FakeSck {
        async fn start(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tx.send(PcmFrame {
                samples: vec![0.5; 100],
                sample_rate_hz: 48_000,
            })
            .await
            .ok();
            Ok(rx)
        }

        async fn stop(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(PcmFrame {
                samples: vec![0.5; 100],
                sample_rate_hz: 48_000,
            })
        }
    }

    /// Compile-only seam test — exercises `SystemAudioCapture` without the SCK feature.
    #[tokio::test]
    async fn sck_fake_produces_frames() {
        let mut cap = FakeSck;
        let mut rx = cap.start().await.unwrap();
        let frame = rx.recv().await.unwrap();
        assert_eq!(frame.samples.len(), 100);
        let stopped = cap.stop().await.unwrap();
        assert_eq!(stopped.samples.len(), 100);
    }

    #[test]
    fn sck_feature_gate_reports_correctly() {
        assert_eq!(sck_feature_enabled(), cfg!(feature = "system-audio-sck"));
    }
}
