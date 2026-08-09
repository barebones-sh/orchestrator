//! Stub macOS backend. Real implementation is a future task; this exists so
//! the `macos` feature compiles and the trait boundary is exercised end to end.

use crate::error::WindowError;
use crate::trait_def::WindowLocator;
use crate::vocab::{WindowHandle, WindowInfo};

pub struct MacosWindowLocator;

impl WindowLocator for MacosWindowLocator {
    async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError> {
        Err(WindowError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError> {
        Err(WindowError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    async fn activate_window(&self, _handle: &WindowHandle) -> Result<(), WindowError> {
        Err(WindowError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }
}
