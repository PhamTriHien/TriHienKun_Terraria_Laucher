use thiserror::Error;

/// Unified error type across the Terraria launcher workspace.
#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Archive error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Invalid binary or file format: {0}")]
    InvalidFormat(String),

    #[error("Process execution error: {0}")]
    Process(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Missing resource: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

pub type LauncherResult<T> = Result<T, LauncherError>;
