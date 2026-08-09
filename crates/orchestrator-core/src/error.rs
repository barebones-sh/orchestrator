//! Error type for config loading, saving, and validation.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON (de)serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error(
        "config schema_version {0} is newer than the version supported by this build \
         (current: {current})",
        current = crate::config::CURRENT_SCHEMA_VERSION
    )]
    UnsupportedSchemaVersion(u32),

    #[error("duplicate profile name: \"{0}\"")]
    DuplicateProfileName(String),

    #[error("profile \"{profile}\" has an invalid interval_ms (must be > 0)")]
    InvalidInterval { profile: String },

    #[error("profile \"{profile}\" has a jitter range_ms that exceeds interval_ms")]
    JitterRangeExceedsInterval { profile: String },

    #[error("profile \"{profile}\" has an empty Scope::Window.process_name")]
    EmptyWindowProcessName { profile: String },

    #[error("profile \"{profile}\" has an empty Action::Macro.steps")]
    EmptyMacroSteps { profile: String },
}
