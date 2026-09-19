pub mod audio;
pub mod openai;
pub mod sherpa;
#[cfg(feature = "tts-system")]
pub mod system;

pub use audio::bytes_to_pcm_wav;
pub use openai::OpenAiTtsEngine;
pub use sherpa::LocalTtsEngine;

#[cfg(feature = "tts-local")]
pub use sherpa::register_sherpa_tts_engine;

#[cfg(feature = "tts-system")]
pub use system::{register_system_tts_engine, SystemTtsEngine, SYSTEM_TTS_ENGINE_ID};
