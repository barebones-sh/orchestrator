use thiserror::Error;

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("input injection not permitted: {0}")]
    NotPermitted(String),

    #[error("input injection backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unsupported input operation: {0}")]
    Unsupported(String),
}
