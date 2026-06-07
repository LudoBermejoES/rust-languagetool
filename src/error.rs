use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("engine not ready (state: {state})")]
    NotReady { state: String },

    #[error("provisioning failed: {0}")]
    Provision(String),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("no usable Java runtime found (need ≥17); provide via JAVA_HOME or PATH, or configure a JRE download URL")]
    NoJavaRuntime,

    #[error("server startup timed out after {seconds}s")]
    StartupTimeout { seconds: u64 },

    #[error("server process exited unexpectedly")]
    ServerCrashed,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

pub type Result<T> = std::result::Result<T, Error>;
