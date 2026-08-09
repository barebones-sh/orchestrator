use crate::error::HotkeyError;
use crate::vocab::{HotkeyEvent, HotkeyId, KeyCombo};

// Native `async fn` in a public trait is a deliberate design choice (design
// doc §5 "Rationale for Native Async Traits"): backend selection is
// compile-time only via Cargo features, so no `dyn` trait objects are ever
// formed and the usual "callers can't name/relax the Future's Send bound"
// concern behind this lint does not apply here.
#[allow(async_fn_in_trait)]
pub trait HotkeyBackend: Send + Sync {
    /// Register a hotkey combination for a named action.
    /// Returns a unique ID for later unregistration.
    async fn register(&self, combo: &KeyCombo, action_name: &str) -> Result<HotkeyId, HotkeyError>;

    /// Unregister a previously registered hotkey.
    async fn unregister(&self, id: HotkeyId) -> Result<(), HotkeyError>;

    /// Single event stream for all registrations on this backend instance.
    /// Returns a synchronous channel receiver emitting (HotkeyId, HotkeyEvent) tuples.
    /// Async backend implementations (e.g., via zbus/tokio) bridge their async event source
    /// into this synchronous channel internally — that bridging is an implementation detail.
    fn subscribe(&self) -> std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)>;

    /// Human-readable backend name for diagnostics.
    fn backend_name(&self) -> &'static str;
}
