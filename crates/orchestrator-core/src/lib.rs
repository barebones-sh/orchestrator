//! Platform-agnostic profile/action/config data model for Orchestrator.
//!
//! This crate depends only on the vocabulary types of the three trait
//! crates (`orchestrator-hotkey`, `orchestrator-input`, `orchestrator-window`,
//! all pulled in with `default-features = false`), plus `serde`,
//! `serde_json`, `thiserror`, and `dirs`. It has zero platform-specific
//! dependencies (no `zbus`, `tokio`, `ashpd`), making it fully
//! unit-testable in isolation (design spec §4 "Dependency Philosophy").

pub mod action;
pub mod config;
pub mod error;
pub mod profile;

pub use action::*;
pub use config::*;
pub use error::*;
pub use profile::*;
