use crate::error::InjectError;
use crate::vocab::InputEvent;

// See orchestrator-hotkey's HotkeyBackend for the rationale on this allow.
#[allow(async_fn_in_trait)]
pub trait InputInjector: Send + Sync {
    /// Establish a connection to the input system (D-Bus, uinput, etc.).
    /// Must be called before inject().
    async fn connect(&mut self) -> Result<(), InjectError>;

    /// Inject a single input event.
    fn inject(
        &self,
        event: &InputEvent,
    ) -> impl std::future::Future<Output = Result<(), InjectError>> + Send;

    /// Query whether this backend requires focus steal for window targeting.
    /// Always true on Wayland backends today; false on X11/macOS with native window targeting.
    /// Exposed explicitly so core and UI never silently assume true no-focus-steal targeting.
    fn requires_focus_steal_for_window_targeting(&self) -> bool;

    /// Human-readable backend name for diagnostics.
    fn backend_name(&self) -> &'static str;
}
