pub mod audio;
pub mod openai;
pub mod parakeet;
pub mod whisper;

pub use audio::{pcm_to_wav_bytes, resample_to_rate, STT_SAMPLE_RATE_HZ};
pub use openai::OpenAiSttEngine;
pub use parakeet::ParakeetSttEngine;
pub use whisper::WhisperSttEngine;

#[cfg(feature = "whisper")]
pub use whisper::register_whisper_stt_engine;

#[cfg(feature = "parakeet")]
pub use parakeet::register_parakeet_stt_engine;
