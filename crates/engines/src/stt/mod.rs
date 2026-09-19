#[cfg(feature = "stt-apple")]
pub mod apple;
pub mod audio;
pub mod deepgram;
pub mod elevenlabs;
pub mod openai;
pub mod parakeet;
pub mod segments;
pub mod streaming;
pub mod whisper;

pub use audio::{pcm_to_wav_bytes, resample_to_rate, STT_SAMPLE_RATE_HZ};
pub use deepgram::{DeepgramSttEngine, DEEPGRAM_BASE_URL, DEEPGRAM_STT_ENGINE_ID};
pub use elevenlabs::{ElevenLabsSttEngine, ELEVENLABS_BASE_URL, ELEVENLABS_STT_ENGINE_ID};
pub use openai::OpenAiSttEngine;
pub use parakeet::ParakeetSttEngine;
pub use segments::{from_infer, from_openai_verbose};
pub use streaming::{StreamingZipformerEngine, STREAMING_STT_ENGINE_ID};
pub use whisper::WhisperSttEngine;

#[cfg(feature = "whisper")]
pub use whisper::register_whisper_stt_engine;

#[cfg(feature = "parakeet")]
pub use parakeet::register_parakeet_stt_engine;

#[cfg(feature = "streaming")]
pub use streaming::register_streaming_stt_engine;

#[cfg(feature = "stt-apple")]
pub use apple::{register_apple_stt_engine, AppleSttEngine, APPLE_STT_ENGINE_ID, APPLE_STT_MODEL};
