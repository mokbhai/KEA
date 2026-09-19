pub mod download;
pub mod error;
pub mod registry;
pub mod sherpa_stream;
pub mod sherpa_stt;
pub mod sherpa_tts;
pub mod storage;
pub mod types;
pub mod whisper;

pub use download::{
    temp_file_for, DownloadProgress, DownloadTransport, ModelDownloader, StreamedFile,
};
pub use error::InferError;
pub use registry::{
    ModelEntry, ModelKind, ModelRegistry, OnnxModelEntry, OnnxModelKind, OnnxVoice,
    WhisperModelEntry,
};
pub use sherpa_stream::{
    find_streaming_model_files, SherpaStreamSession, SherpaStreamingInference, StreamingModelFiles,
};
pub use sherpa_stt::SherpaSttInference;
pub use sherpa_tts::{find_tts_bundle, SherpaTtsInference, TtsBundle};
pub use storage::ModelStorage;
pub use types::{
    clamp_tts_speed, AudioPcm, StreamingCfg, TtsSynthOpts, WhisperOpts, MAX_TTS_SPEED,
    MIN_TTS_SPEED, STREAMING_SAMPLE_RATE_HZ,
};
pub use whisper::WhisperInference;

#[cfg(feature = "whisper")]
pub use whisper::WhisperRsInference;

#[cfg(feature = "sherpa")]
pub use sherpa_stream::SherpaOnnxStreamingInference;

#[cfg(feature = "sherpa")]
pub use sherpa_stt::SherpaOnnxSttInference;

#[cfg(feature = "sherpa")]
pub use sherpa_tts::SherpaOnnxTtsInference;
