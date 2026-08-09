use thiserror::Error;

#[derive(Debug, Error)]
pub enum WindowError {
    #[error("window backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("window not found")]
    NotFound,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
