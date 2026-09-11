//! The runner: turns a loaded `Profile` list plus the three trait-crate
//! backends into actual running toggle/repeat/macro behavior. See
//! `docs/superpowers/specs/2026-08-14-runner-design.md` for the full design.

use crate::action::Jitter;

/// `Jitter::None` -> `interval_ms` fixed. `Jitter::Uniform { range_ms }` ->
/// `interval_ms +/- uniform(0, range_ms)`, symmetric around the interval.
/// `orchestrator-core`'s `Config::validate()` already requires
/// `range_ms <= interval_ms`, which is exactly what keeps this delay from
/// ever going negative (design spec §5.1).
pub(crate) fn compute_delay(interval_ms: u64, jitter: &Jitter) -> std::time::Duration {
    let millis = match jitter {
        Jitter::None => interval_ms,
        Jitter::Uniform { range_ms } if *range_ms == 0 => interval_ms,
        Jitter::Uniform { range_ms } => {
            let offset = fastrand::i64(-(*range_ms as i64)..=*range_ms as i64);
            (interval_ms as i64 + offset).max(0) as u64
        }
    };
    std::time::Duration::from_millis(millis)
}

use orchestrator_hotkey::{HotkeyBackend, HotkeyEvent, HotkeyId};
use orchestrator_input::InputInjector;
use orchestrator_window::WindowLocator;
use std::collections::HashMap;
use std::future::Future;

use crate::profile::Profile;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("failed to register hotkey for profile {profile_name:?}: {source}")]
    HotkeyRegistration {
        profile_name: String,
        #[source]
        source: orchestrator_hotkey::HotkeyError,
    },
}

use std::sync::Arc;
use std::time::Instant;

use crate::action::Action;
use crate::profile::Scope;

struct ProfileState {
    running: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    last_toggle_at: Option<Instant>,
}

impl ProfileState {
    fn new() -> Self {
        Self {
            running: None,
            task: None,
            last_toggle_at: None,
        }
    }
}

pub struct Runner<H: HotkeyBackend, I: InputInjector, W: WindowLocator> {
    hotkey: H,
    input: Arc<I>,
    window: Arc<W>,
    profiles: Vec<Profile>,
}

impl<H: HotkeyBackend, I: InputInjector + 'static, W: WindowLocator + 'static> Runner<H, I, W> {
    pub fn new(hotkey: H, input: I, window: impl Into<Arc<W>>, profiles: Vec<Profile>) -> Self {
        Self {
            hotkey,
            input: Arc::new(input),
            window: window.into(),
            profiles,
        }
    }

    /// Test-only constructor variant accepting an already-shared `Arc<I>`,
    /// so tests can keep their own clone to inspect after handing the other
    /// clone into the `Runner` (see design spec's §3 "wrap `input`/`window`
    /// in `Arc` internally"). Not exposed to real callers: `Runner::new`'s
    /// `impl Into<Arc<I>>` variant for this parameter was tried first but
    /// rejected — it collides with `std`'s blanket `impl<T> From<T> for T`
    /// and `impl<T> From<T> for Arc<T>` impls whenever a caller already
    /// holds an `Arc<I>` (exactly what these tests need), producing an
    /// unresolvable `E0283` ambiguity at the call site.
    #[cfg(test)]
    fn new_with_shared_input(hotkey: H, input: Arc<I>, window: impl Into<Arc<W>>, profiles: Vec<Profile>) -> Self {
        Self {
            hotkey,
            input,
            window: window.into(),
            profiles,
        }
    }

    /// `input` must already be connected (see `InputInjector::connect`)
    /// before constructing a `Runner` — `run()` does not call `connect()`
    /// itself. (`connect()` takes `&mut self`, which is incompatible with
    /// the `Arc<I>` sharing `run()` needs for concurrent per-profile
    /// tasks — see the runner design spec §3 vs. this plan's Task 3 note.)
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) -> Result<(), RunnerError> {
        let mut id_to_index: HashMap<HotkeyId, usize> = HashMap::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            let (id, _combo) = self
                .hotkey
                .register(&profile.name, None)
                .await
                .map_err(|source| RunnerError::HotkeyRegistration {
                    profile_name: profile.name.clone(),
                    source,
                })?;
            id_to_index.insert(id, index);
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let hotkey_rx = self.hotkey.subscribe();
        tokio::task::spawn_blocking(move || {
            while let Ok(item) = hotkey_rx.recv() {
                if tx.send(item).is_err() {
                    break;
                }
            }
        });

        let mut states: Vec<ProfileState> = self.profiles.iter().map(|_| ProfileState::new()).collect();

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                Some((id, event)) = rx.recv() => {
                    if !matches!(event, HotkeyEvent::Pressed) {
                        continue;
                    }
                    let Some(&index) = id_to_index.get(&id) else { continue };
                    self.handle_press(index, &mut states[index]).await;
                }
                else => break,
            }
        }

        for state in &mut states {
            if let Some(cancel) = state.running.take() {
                let _ = cancel.send(());
            }
            if let Some(task) = state.task.take() {
                let _ = task.await;
            }
        }

        Ok(())
    }

    async fn handle_press(&self, index: usize, state: &mut ProfileState) {
        let profile = &self.profiles[index];
        let now = Instant::now();
        if let Some(last) = state.last_toggle_at {
            if now.duration_since(last) < std::time::Duration::from_millis(profile.debounce_ms as u64) {
                return;
            }
        }
        state.last_toggle_at = Some(now);

        if let Some(cancel) = state.running.take() {
            let _ = cancel.send(());
            if let Some(task) = state.task.take() {
                let _ = task.await;
            }
            return;
        }

        let Action::Repeat { input: event, interval_ms, jitter } = &profile.action else {
            // Action::Macro handling lands in Task 5.
            return;
        };
        if !matches!(profile.scope, Scope::Desktop) {
            // Scope::Window handling lands in Task 6.
            return;
        }

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let input = Arc::clone(&self.input);
        let event = event.clone();
        let interval_ms = *interval_ms;
        let jitter = jitter.clone();
        let task = tokio::spawn(async move {
            loop {
                let _ = input.inject(&event).await;
                let delay = compute_delay(interval_ms, &jitter);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = &mut cancel_rx => break,
                }
            }
        });

        state.running = Some(cancel_tx);
        state.task = Some(task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_delay_none_jitter_is_exact_interval() {
        let d = compute_delay(100, &Jitter::None);
        assert_eq!(d, std::time::Duration::from_millis(100));
    }

    #[test]
    fn compute_delay_uniform_jitter_stays_within_symmetric_bounds() {
        let interval_ms = 100u64;
        let range_ms = 30u64;
        for _ in 0..500 {
            let d = compute_delay(interval_ms, &Jitter::Uniform { range_ms });
            let millis = d.as_millis() as i64;
            assert!(
                (interval_ms as i64 - range_ms as i64..=interval_ms as i64 + range_ms as i64)
                    .contains(&millis),
                "delay {millis}ms out of [{}, {}] bounds",
                interval_ms as i64 - range_ms as i64,
                interval_ms as i64 + range_ms as i64
            );
        }
    }

    #[test]
    fn compute_delay_zero_range_jitter_is_exact_interval() {
        let d = compute_delay(100, &Jitter::Uniform { range_ms: 0 });
        assert_eq!(d, std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod fakes {
    use orchestrator_hotkey::{HotkeyBackend, HotkeyError, HotkeyEvent, HotkeyId, KeyCombo};
    use orchestrator_input::{InjectError, InputEvent, InputInjector};
    use orchestrator_window::{WindowError, WindowHandle, WindowInfo, WindowLocator};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Records every `register()` call and hands back a `Sender` the test
    /// keeps to push `(HotkeyId, HotkeyEvent)` pairs through the same
    /// channel `subscribe()` returns the `Receiver` half of.
    pub struct FakeHotkeyBackend {
        pub registered: Mutex<Vec<(String, Option<KeyCombo>)>>,
        next_id: AtomicU64,
        event_tx: std::sync::mpsc::Sender<(HotkeyId, HotkeyEvent)>,
        event_rx: Mutex<Option<std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)>>>,
    }

    impl FakeHotkeyBackend {
        /// Returns the fake plus a sender the test uses to push events as
        /// if a real hotkey had fired.
        pub fn new() -> (Self, std::sync::mpsc::Sender<(HotkeyId, HotkeyEvent)>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let fake = Self {
                registered: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                event_tx: tx.clone(),
                event_rx: Mutex::new(Some(rx)),
            };
            (fake, tx)
        }
    }

    impl HotkeyBackend for FakeHotkeyBackend {
        async fn register(
            &self,
            action_name: &str,
            preferred: Option<&KeyCombo>,
        ) -> Result<(HotkeyId, KeyCombo), HotkeyError> {
            let id = HotkeyId(self.next_id.fetch_add(1, Ordering::SeqCst));
            self.registered
                .lock()
                .unwrap()
                .push((action_name.to_string(), preferred.cloned()));
            Ok((
                id,
                preferred.cloned().unwrap_or(KeyCombo {
                    modifiers: vec![],
                    key: String::new(),
                }),
            ))
        }

        async fn unregister(&self, _id: HotkeyId) -> Result<(), HotkeyError> {
            Ok(())
        }

        fn subscribe(&self) -> std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)> {
            self.event_rx
                .lock()
                .unwrap()
                .take()
                .expect("FakeHotkeyBackend::subscribe() called more than once")
        }

        fn backend_name(&self) -> &'static str {
            "fake"
        }
    }

    /// Records every injected event into `injected` for the test to assert
    /// against.
    pub struct FakeInputInjector {
        pub injected: Mutex<Vec<InputEvent>>,
    }

    impl FakeInputInjector {
        pub fn new() -> Self {
            Self {
                injected: Mutex::new(Vec::new()),
            }
        }
    }

    impl InputInjector for FakeInputInjector {
        async fn connect(&mut self) -> Result<(), InjectError> {
            Ok(())
        }

        async fn inject(&self, event: &InputEvent) -> Result<(), InjectError> {
            self.injected.lock().unwrap().push(event.clone());
            Ok(())
        }

        fn requires_focus_steal_for_window_targeting(&self) -> bool {
            true
        }

        fn backend_name(&self) -> &'static str {
            "fake"
        }
    }

    /// Returns a fixed window list and tracks every `activate_window()`
    /// call (in order) for the test to assert against.
    pub struct FakeWindowLocator {
        pub windows: Vec<WindowInfo>,
        pub focused: Option<WindowHandle>,
        pub activated: Mutex<Vec<WindowHandle>>,
    }

    impl FakeWindowLocator {
        pub fn new(windows: Vec<WindowInfo>, focused: Option<WindowHandle>) -> Self {
            Self {
                windows,
                focused,
                activated: Mutex::new(Vec::new()),
            }
        }
    }

    impl WindowLocator for FakeWindowLocator {
        async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError> {
            Ok(self.windows.clone())
        }

        async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError> {
            Ok(self.focused.clone())
        }

        async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError> {
            self.activated.lock().unwrap().push(handle.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod fakes_smoke_tests {
    use super::fakes::*;
    use orchestrator_hotkey::{HotkeyBackend, HotkeyEvent, HotkeyId};
    use orchestrator_input::{InputEvent, InputInjector};
    use orchestrator_window::{WindowHandle, WindowInfo, WindowLocator};

    #[tokio::test]
    async fn fake_hotkey_backend_records_registration_and_delivers_pushed_events() {
        let (backend, tx) = FakeHotkeyBackend::new();
        let (id, _combo) = backend.register("my-action", None).await.unwrap();
        assert_eq!(backend.registered.lock().unwrap().len(), 1);
        assert_eq!(backend.registered.lock().unwrap()[0].0, "my-action");

        let rx = backend.subscribe();
        tx.send((id.clone(), HotkeyEvent::Pressed)).unwrap();
        let (received_id, event) = rx.recv().unwrap();
        assert_eq!(received_id, id);
        assert!(matches!(event, HotkeyEvent::Pressed));
    }

    #[tokio::test]
    async fn fake_input_injector_records_injected_events() {
        let mut injector = FakeInputInjector::new();
        injector.connect().await.unwrap();
        injector
            .inject(&InputEvent::Scroll { dx: 0, dy: 1 })
            .await
            .unwrap();
        assert_eq!(injector.injected.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fake_window_locator_lists_windows_and_records_activation() {
        let handle = WindowHandle("h1".to_string());
        let locator = FakeWindowLocator::new(
            vec![WindowInfo {
                handle: handle.clone(),
                pid: Some(123),
                process_name: Some("app".to_string()),
                title: "Title".to_string(),
            }],
            None,
        );
        let windows = locator.list_windows().await.unwrap();
        assert_eq!(windows.len(), 1);

        locator.activate_window(&handle).await.unwrap();
        assert_eq!(locator.activated.lock().unwrap().as_slice(), &[handle]);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::fakes::*;
    use super::*;
    use crate::action::{Action, Jitter};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{KeyCombo, Modifier};
    use orchestrator_input::InputEvent;

    fn desktop_repeat_profile(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 100,
                jitter: Jitter::None,
            },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms: 400,
        }
    }

    #[tokio::test]
    async fn run_registers_one_hotkey_per_profile() {
        let (hotkey, _tx) = FakeHotkeyBackend::new();
        let input = FakeInputInjector::new();
        let window = FakeWindowLocator::new(vec![], None);
        let profiles = vec![
            desktop_repeat_profile("a"),
            desktop_repeat_profile("b"),
        ];

        let runner = Runner::new(hotkey, input, window, profiles);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        shutdown_tx.send(()).unwrap();

        // Registration must have happened even though we shut down
        // immediately after — but we can't inspect `hotkey.registered`
        // after moving it into `runner`. Instead, this test asserts via
        // the returned Ok(()) that startup (including registration)
        // completed without error for two profiles; Task 4 adds a variant
        // of this test that keeps a handle to assert call counts directly
        // by restructuring fakes to be Arc-shared (see Task 4's own test
        // for that pattern).
        let result = runner
            .run(async {
                let _ = shutdown_rx.await;
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_returns_promptly_on_immediate_shutdown() {
        let (hotkey, _tx) = FakeHotkeyBackend::new();
        let input = FakeInputInjector::new();
        let window = FakeWindowLocator::new(vec![], None);
        let profiles = vec![desktop_repeat_profile("only")];

        let runner = Runner::new(hotkey, input, window, profiles);
        let start = std::time::Instant::now();
        let result = runner.run(async {}).await;
        assert!(result.is_ok());
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "run() took too long to return after an already-resolved shutdown future"
        );
    }
}

#[cfg(test)]
mod toggle_tests {
    use super::fakes::*;
    use super::*;
    use crate::action::{Action, Jitter};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{HotkeyEvent, KeyCombo, Modifier};
    use orchestrator_input::InputEvent;
    use std::sync::Arc;

    fn desktop_repeat_profile(name: &str, interval_ms: u64, debounce_ms: u32) -> Profile {
        Profile {
            name: name.to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms,
                jitter: Jitter::None,
            },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms,
        }
    }

    #[tokio::test]
    async fn pressing_hotkey_starts_repeating_injection() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let profile = desktop_repeat_profile("clicker", 20, 400);

        let runner = Runner::new_with_shared_input(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        // Give startup (registration) time to complete, then simulate a
        // press. The fake hands back HotkeyId(1) for the first registered
        // action (see FakeHotkeyBackend::register's sequential id
        // assignment).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let count_while_running = input.injected.lock().unwrap().len();
        assert!(
            count_while_running >= 2,
            "expected multiple injections in 100ms at a 20ms interval, got {count_while_running}"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn pressing_hotkey_twice_toggles_off_and_stops_injecting() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let profile = desktop_repeat_profile("clicker", 20, 50);

        let runner = Runner::new_with_shared_input(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await; // > debounce_ms
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        let count_after_stop = input.injected.lock().unwrap().len();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let count_later = input.injected.lock().unwrap().len();
        assert_eq!(
            count_after_stop, count_later,
            "injection count changed after toggle-off; repeat task wasn't cancelled"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn second_press_within_debounce_window_is_ignored() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let profile = desktop_repeat_profile("clicker", 20, 400); // long debounce

        let runner = Runner::new_with_shared_input(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        // Second press arrives well within the 400ms debounce window --
        // must be ignored, so the profile stays "on" and keeps injecting.
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let count = input.injected.lock().unwrap().len();
        assert!(
            count >= 3,
            "expected the profile to still be running (ignored debounced press), got {count} injections"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }
}
