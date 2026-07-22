use thiserror::Error;

#[derive(Debug, Error)]
pub enum InferError {
    #[error("infer error: {0}")]
    Other(String),

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("hash mismatch for model '{model_id}': expected {expected}.., got {actual}..")]
    HashMismatch {
        model_id: String,
        expected: String,
        actual: String,
    },

    #[error("insufficient disk space for model '{model_id}': need {required} bytes, have {available} bytes")]
    InsufficientSpace {
        model_id: String,
        required: u64,
        available: u64,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
