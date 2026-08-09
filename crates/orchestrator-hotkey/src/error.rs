use thiserror::Error;

use crate::vocab::KeyCombo;

#[derive(Debug, Error)]
pub enum HotkeyError {
    #[error("hotkey registration failed: {0}")]
    RegistrationFailed(String),

    #[error("hotkey already registered: {0:?}")]
    AlreadyRegistered(KeyCombo),

    #[error("hotkey backend unavailable: {0}")]
    BackendUnavailable(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
