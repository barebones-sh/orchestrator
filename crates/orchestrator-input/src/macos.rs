//! Stub macOS backend. Real implementation is a future task; this exists so
//! the `macos` feature compiles and the trait boundary is exercised end to end.

use crate::error::InjectError;
use crate::trait_def::InputInjector;
use crate::vocab::InputEvent;

pub struct MacosInputInjector;

impl InputInjector for MacosInputInjector {
    async fn connect(&mut self) -> Result<(), InjectError> {
        Err(InjectError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    async fn inject(&self, _event: &InputEvent) -> Result<(), InjectError> {
        Err(InjectError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    fn requires_focus_steal_for_window_targeting(&self) -> bool {
        // macOS's native Accessibility-API-based targeting supports direct
        // targeted injection, unlike Wayland — no focus steal required.
        false
    }

    fn backend_name(&self) -> &'static str {
        "macos-stub"
    }
}
