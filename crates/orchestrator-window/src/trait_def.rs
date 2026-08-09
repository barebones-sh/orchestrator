use crate::error::WindowError;
use crate::vocab::{WindowHandle, WindowInfo};

// See orchestrator-hotkey's HotkeyBackend for the rationale on this allow.
#[allow(async_fn_in_trait)]
pub trait WindowLocator: Send + Sync {
    /// List all visible windows on the desktop.
    async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError>;

    /// Get the currently keyboard-focused window.
    async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError>;

    /// Activate (raise and focus) the given window.
    async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError>;
}
