pub mod download;
pub mod error;
pub mod registry;
pub mod sherpa_diarize;
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
    ModelEntry, ModelKind, ModelRegistry, OnnxBundleShape, OnnxModelEntry, OnnxModelKind,
    OnnxVoice, WhisperModelEntry, DIARIZATION_EMBEDDING_ID, DIARIZATION_SEGMENTATION_ID,
};
pub use sherpa_diarize::{DiarizationModels, DiarizationOpts, SpeakerDiarization, SpeakerSpan};
pub use sherpa_stream::{
    find_streaming_model_files, SherpaStreamSession, SherpaStreamingInference, StreamingModelFiles,
};
pub use sherpa_stt::SherpaSttInference;
pub use sherpa_tts::{find_tts_bundle, SherpaTtsInference, TtsBundle};
pub use storage::ModelStorage;
pub use types::{
    clamp_tts_speed, group_tokens_into_segments, join_segment_text, AudioPcm, StreamingCfg,
    SttResult, TimedSegment, TtsSynthOpts, WhisperOpts, MAX_TTS_SPEED, MIN_TTS_SPEED,
    STREAMING_SAMPLE_RATE_HZ, TOKEN_GROUP_GAP_MS, TOKEN_GROUP_MAX_CUE_MS,
};
pub use whisper::{centiseconds_to_ms, WhisperInference};

#[cfg(feature = "whisper")]
pub use whisper::WhisperRsInference;

#[cfg(feature = "sherpa")]
pub use sherpa_stream::SherpaOnnxStreamingInference;

#[cfg(feature = "sherpa")]
pub use sherpa_diarize::SherpaOnnxDiarization;

#[cfg(feature = "sherpa")]
pub use sherpa_stt::SherpaOnnxSttInference;

#[cfg(feature = "sherpa")]
pub use sherpa_tts::SherpaOnnxTtsInference;
