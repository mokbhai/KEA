pub mod download;
pub mod error;
pub mod registry;
pub mod sherpa_stt;
pub mod sherpa_tts;
pub mod storage;
pub mod whisper;

pub use download::{
    temp_file_for, DownloadProgress, DownloadTransport, ModelDownloader, StreamedFile,
};
pub use error::InferError;
pub use registry::{ModelRegistry, OnnxModelEntry, OnnxModelKind, WhisperModelEntry};
pub use sherpa_stt::SherpaSttInference;
pub use sherpa_tts::SherpaTtsInference;
pub use storage::ModelStorage;
pub use whisper::{AudioPcm, WhisperInference, WhisperOpts};

#[cfg(feature = "whisper")]
pub use whisper::WhisperRsInference;

#[cfg(feature = "sherpa")]
pub use sherpa_stt::SherpaOnnxSttInference;

#[cfg(feature = "sherpa")]
pub use sherpa_tts::SherpaOnnxTtsInference;
