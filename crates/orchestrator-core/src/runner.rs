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
    prior_focus: Option<orchestrator_window::WindowHandle>,
}

impl ProfileState {
    fn new() -> Self {
        Self {
            running: None,
            task: None,
            last_toggle_at: None,
            prior_focus: None,
        }
    }
}

/// Pure window-resolution policy shared by `handle_press`'s `Scope::Window`
/// handling, kept standalone so it's independently unit-testable (design
/// spec §2 "Scope").
///
/// Policy: filter by `process_name` first. If more than one window matches
/// and `window_title_hint` is non-empty, narrow further by title
/// substring match (falling back to the unfiltered process-name matches if
/// the title hint matches nothing). Among the resulting candidates, prefer
/// one whose handle equals `backend_hint_id` if present in the list;
/// otherwise fall back to the first candidate.
pub(crate) fn resolve_window(
    windows: &[orchestrator_window::WindowInfo],
    process_name: &str,
    window_title_hint: &str,
    backend_hint_id: Option<&str>,
) -> Option<orchestrator_window::WindowHandle> {
    let by_process: Vec<&orchestrator_window::WindowInfo> = windows
        .iter()
        .filter(|w| w.process_name.as_deref() == Some(process_name))
        .collect();

    let candidates: Vec<&orchestrator_window::WindowInfo> = if by_process.len() > 1 && !window_title_hint.is_empty() {
        let filtered: Vec<&orchestrator_window::WindowInfo> = by_process
            .iter()
            .copied()
            .filter(|w| w.title.contains(window_title_hint))
            .collect();
        if filtered.is_empty() {
            by_process
        } else {
            filtered
        }
    } else {
        by_process
    };

    if candidates.is_empty() {
        return None;
    }

    if let Some(hint_id) = backend_hint_id {
        if let Some(exact) = candidates.iter().find(|w| w.handle.0 == hint_id) {
            return Some(exact.handle.clone());
        }
    }

    candidates.first().map(|w| w.handle.clone())
}

pub struct Runner<H: HotkeyBackend, I: InputInjector, W: WindowLocator> {
    hotkey: H,
    input: Arc<I>,
    window: Arc<W>,
    profiles: Vec<Profile>,
}

impl<H: HotkeyBackend, I: InputInjector + 'static, W: WindowLocator + 'static> Runner<H, I, W> {
    pub fn new(hotkey: H, input: impl Into<Arc<I>>, window: impl Into<Arc<W>>, profiles: Vec<Profile>) -> Self {
        Self {
            hotkey,
            input: input.into(),
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

        // A non-looping macro's task ends on its own once it finishes its
        // steps (see `run_macro`). Detect that here and treat it as the
        // profile having already self-toggled off, so this press starts a
        // fresh run instead of "stopping" a task that's already done
        // (design spec §5.2).
        if let Some(task) = &state.task {
            if task.is_finished() {
                state.running = None;
                state.task = None;
            }
        }

        if let Some(cancel) = state.running.take() {
            let _ = cancel.send(());
            if let Some(task) = state.task.take() {
                let _ = task.await;
            }
            if let Some(prior) = state.prior_focus.take() {
                let _ = self.window.activate_window(&prior).await;
            }
            return;
        }

        match &profile.scope {
            Scope::Desktop => {}
            Scope::Window {
                process_name,
                window_title_hint,
                backend_hint_id,
            } => {
                let Ok(windows) = self.window.list_windows().await else { return };
                let Some(handle) = resolve_window(&windows, process_name, window_title_hint, backend_hint_id.as_deref())
                else {
                    return; // no match -- toggle-on fails cleanly, profile stays off
                };
                let prior = self.window.focused_window().await.ok().flatten();
                if self.window.activate_window(&handle).await.is_err() {
                    return;
                }
                state.prior_focus = prior;
            }
        }

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let input = Arc::clone(&self.input);

        let task = match profile.action.clone() {
            Action::Repeat { input: event, interval_ms, jitter } => {
                tokio::spawn(run_repeat(input, event, interval_ms, jitter, cancel_rx))
            }
            Action::Macro { steps, loop_ } => {
                tokio::spawn(run_macro(input, steps, loop_, cancel_rx))
            }
        };

        state.running = Some(cancel_tx);
        state.task = Some(task);
    }
}

async fn run_repeat<I: InputInjector>(
    input: Arc<I>,
    event: orchestrator_input::InputEvent,
    interval_ms: u64,
    jitter: Jitter,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        let _ = input.inject(&event).await;
        let delay = compute_delay(interval_ms, &jitter);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = &mut cancel_rx => break,
        }
    }
}

async fn run_macro<I: InputInjector>(
    input: Arc<I>,
    steps: Vec<crate::action::MacroStep>,
    loop_: bool,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use crate::action::{ClickPosition, MacroStep};
    use orchestrator_input::InputEvent;

    loop {
        for step in &steps {
            let delay_ms = match step {
                MacroStep::KeyPress { delay_ms, .. }
                | MacroStep::MouseClick { delay_ms, .. }
                | MacroStep::Drag { delay_ms, .. }
                | MacroStep::Scroll { delay_ms, .. } => *delay_ms,
            };
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)) => {}
                _ = &mut cancel_rx => return,
            }

            match step {
                MacroStep::KeyPress { combo, .. } => {
                    let _ = input.inject(&InputEvent::KeyPress(combo.clone())).await;
                    let _ = input.inject(&InputEvent::KeyRelease(combo.clone())).await;
                }
                MacroStep::MouseClick { button, position, .. } => {
                    if let ClickPosition::Fixed { x, y } = position {
                        let _ = input
                            .inject(&InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    let _ = input.inject(&InputEvent::MouseButtonPress(*button)).await;
                    let _ = input.inject(&InputEvent::MouseButtonRelease(*button)).await;
                }
                MacroStep::Drag { from, to, .. } => {
                    if let ClickPosition::Fixed { x, y } = from {
                        let _ = input
                            .inject(&InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    let _ = input
                        .inject(&InputEvent::MouseButtonPress(orchestrator_input::MouseButton::Left))
                        .await;
                    if let ClickPosition::Fixed { x, y } = to {
                        let _ = input
                            .inject(&InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    let _ = input
                        .inject(&InputEvent::MouseButtonRelease(orchestrator_input::MouseButton::Left))
                        .await;
                }
                MacroStep::Scroll { dx, dy, .. } => {
                    let _ = input.inject(&InputEvent::Scroll { dx: *dx, dy: *dy }).await;
                }
            }
        }

        if !loop_ {
            break;
        }
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

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
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

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
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

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
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

#[cfg(test)]
mod macro_tests {
    use super::fakes::*;
    use super::*;
    use crate::action::{Action, ClickPosition, MacroStep};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{HotkeyEvent, KeyCombo, Modifier};
    use orchestrator_input::{InputEvent, MouseButton};
    use std::sync::Arc;

    fn desktop_macro_profile(name: &str, steps: Vec<MacroStep>, loop_: bool, debounce_ms: u32) -> Profile {
        Profile {
            name: name.to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F10".to_string(),
            },
            action: Action::Macro { steps, loop_ },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms,
        }
    }

    #[tokio::test]
    async fn non_looping_macro_runs_once_and_self_toggles_off() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let steps = vec![
            MacroStep::Scroll { delay_ms: 5, dx: 0, dy: 1 },
            MacroStep::Scroll { delay_ms: 5, dx: 0, dy: -1 },
        ];
        let profile = desktop_macro_profile("once", steps, false, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let count_after_completion = input.injected.lock().unwrap().len();
        assert_eq!(
            count_after_completion, 2,
            "non-looping macro should inject exactly once per step"
        );

        // Confirm it self-toggled off: pressing again should start a fresh
        // run (another 2 injections), not toggle "off" a task that's
        // already finished.
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(input.injected.lock().unwrap().len(), 4);

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn looping_macro_repeats_until_toggled_off() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let steps = vec![MacroStep::Scroll { delay_ms: 5, dx: 0, dy: 1 }];
        let profile = desktop_macro_profile("loop", steps, true, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let count = input.injected.lock().unwrap().len();
        assert!(count >= 3, "expected the loop to have run multiple times, got {count}");

        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let count_after_stop = input.injected.lock().unwrap().len();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(count_after_stop, input.injected.lock().unwrap().len());

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn mouse_click_at_cursor_skips_the_move_event() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = FakeWindowLocator::new(vec![], None);
        let steps = vec![MacroStep::MouseClick {
            delay_ms: 5,
            button: MouseButton::Left,
            position: ClickPosition::AtCursor,
        }];
        let profile = desktop_macro_profile("click", steps, false, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(hotkey, Arc::clone(&input), window, vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;

        let injected = input.injected.lock().unwrap();
        assert_eq!(injected.len(), 2, "expected exactly press+release, no move event");
        assert!(matches!(injected[0], InputEvent::MouseButtonPress(MouseButton::Left)));
        assert!(matches!(injected[1], InputEvent::MouseButtonRelease(MouseButton::Left)));

        drop(injected);
        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }
}

#[cfg(test)]
mod window_resolution_tests {
    use super::*;
    use orchestrator_window::{WindowHandle, WindowInfo};

    fn window(handle: &str, process_name: &str, title: &str) -> WindowInfo {
        WindowInfo {
            handle: WindowHandle(handle.to_string()),
            pid: Some(1),
            process_name: Some(process_name.to_string()),
            title: title.to_string(),
        }
    }

    #[test]
    fn resolves_single_matching_process() {
        let windows = vec![window("h1", "firefox", "Mozilla Firefox")];
        let resolved = resolve_window(&windows, "firefox", "", None);
        assert_eq!(resolved, Some(WindowHandle("h1".to_string())));
    }

    #[test]
    fn no_match_returns_none() {
        let windows = vec![window("h1", "firefox", "Mozilla Firefox")];
        let resolved = resolve_window(&windows, "chrome", "", None);
        assert_eq!(resolved, None);
    }

    #[test]
    fn multiple_matches_filtered_by_title_hint() {
        let windows = vec![
            window("h1", "code", "project-a - VS Code"),
            window("h2", "code", "project-b - VS Code"),
        ];
        let resolved = resolve_window(&windows, "code", "project-b", None);
        assert_eq!(resolved, Some(WindowHandle("h2".to_string())));
    }

    #[test]
    fn prefers_stored_backend_hint_id_among_matches() {
        let windows = vec![
            window("h1", "code", "a - VS Code"),
            window("h2", "code", "b - VS Code"),
        ];
        // Title hint alone would match both (empty hint matches everything);
        // backend_hint_id should pick h2 specifically.
        let resolved = resolve_window(&windows, "code", "", Some("h2"));
        assert_eq!(resolved, Some(WindowHandle("h2".to_string())));
    }

    #[test]
    fn falls_back_to_first_match_when_backend_hint_id_not_present() {
        let windows = vec![
            window("h1", "code", "a - VS Code"),
            window("h2", "code", "b - VS Code"),
        ];
        let resolved = resolve_window(&windows, "code", "", Some("stale-id-not-in-list"));
        assert_eq!(resolved, Some(WindowHandle("h1".to_string())));
    }
}

#[cfg(test)]
mod window_scope_tests {
    use super::fakes::*;
    use super::*;
    use crate::action::{Action, Jitter};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{HotkeyEvent, KeyCombo, Modifier};
    use orchestrator_input::InputEvent;
    use orchestrator_window::{WindowHandle, WindowInfo};
    use std::sync::Arc;

    fn window_scoped_profile(name: &str, process_name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 20,
                jitter: Jitter::None,
            },
            scope: Scope::Window {
                process_name: process_name.to_string(),
                window_title_hint: String::new(),
                backend_hint_id: None,
            },
            focus_steal: true,
            debounce_ms: 50,
        }
    }

    #[tokio::test]
    async fn toggle_on_activates_target_once_and_toggle_off_restores_prior_focus() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let target = WindowHandle("target".to_string());
        let prior = WindowHandle("prior-focus".to_string());
        let window = Arc::new(FakeWindowLocator::new(
            vec![WindowInfo {
                handle: target.clone(),
                pid: Some(1),
                process_name: Some("firefox".to_string()),
                title: "Mozilla Firefox".to_string(),
            }],
            Some(prior.clone()),
        ));
        let profile = window_scoped_profile("scoped", "firefox");

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(hotkey, Arc::clone(&input), Arc::clone(&window), vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        assert!(
            input.injected.lock().unwrap().len() >= 2,
            "expected injection to have started after activation"
        );
        assert_eq!(
            window.activated.lock().unwrap().as_slice(),
            &[target.clone()],
            "expected exactly one activate_window call (the target) at toggle-on"
        );

        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        assert_eq!(
            window.activated.lock().unwrap().as_slice(),
            &[target, prior],
            "expected a second activate_window call restoring prior focus at toggle-off"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn toggle_on_fails_cleanly_when_no_window_matches() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = Arc::new(FakeWindowLocator::new(vec![], None));
        let profile = window_scoped_profile("scoped", "nonexistent-app");

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(hotkey, Arc::clone(&input), Arc::clone(&window), vec![profile]);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        assert_eq!(
            input.injected.lock().unwrap().len(),
            0,
            "expected no injection since no window matched"
        );
        assert_eq!(window.activated.lock().unwrap().len(), 0);

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }
}
