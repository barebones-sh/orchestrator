//! Stub macOS backend. Real implementation is a future task; this exists so
//! the `macos` feature compiles and the trait boundary is exercised end to end.

use crate::error::HotkeyError;
use crate::trait_def::HotkeyBackend;
use crate::vocab::{HotkeyEvent, HotkeyId, KeyCombo};

pub struct MacosHotkeyBackend;

impl HotkeyBackend for MacosHotkeyBackend {
    async fn register(
        &self,
        _combo: &KeyCombo,
        _action_name: &str,
    ) -> Result<HotkeyId, HotkeyError> {
        Err(HotkeyError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    async fn unregister(&self, _id: HotkeyId) -> Result<(), HotkeyError> {
        Err(HotkeyError::BackendUnavailable(
            "macOS backend not yet implemented".into(),
        ))
    }

    fn subscribe(&self) -> std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)> {
        // The sender is dropped immediately, so the receiver is valid but will
        // report the channel closed to any caller.
        let (_tx, rx) = std::sync::mpsc::channel();
        rx
    }

    fn backend_name(&self) -> &'static str {
        "macos-stub"
    }
}
