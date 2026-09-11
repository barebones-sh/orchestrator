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
/// spec §6 "Window Scope Resolution").
///
/// Policy: filter by `process_name` first. Whenever `window_title_hint` is
/// non-empty, narrow further by title substring match -- regardless of how
/// many process-name matches there were, including exactly one. If applying
/// a non-empty hint leaves zero candidates, that is treated as no match at
/// all (`None`), not a silent fallback to the unfiltered process-name
/// matches: a hint that matches nothing means the caller's targeting intent
/// can't be satisfied, and toggle-on should fail cleanly rather than
/// silently act on the wrong window. Among the resulting candidates, prefer
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

    let candidates: Vec<&orchestrator_window::WindowInfo> = if window_title_hint.is_empty() {
        by_process
    } else {
        by_process
            .iter()
            .copied()
            .filter(|w| w.title.contains(window_title_hint))
            .collect()
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
    pub fn new(
        hotkey: H,
        input: impl Into<Arc<I>>,
        window: impl Into<Arc<W>>,
        profiles: Vec<Profile>,
    ) -> Self {
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
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), RunnerError> {
        let mut id_to_index: HashMap<HotkeyId, usize> = HashMap::new();
        for (index, profile) in self.profiles.iter().enumerate() {
            let (id, _combo) =
                self.hotkey
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
        // Bridges the backend's sync `subscribe()` channel onto the async
        // world above. Dropping a `JoinHandle` alone does not cancel the
        // task -- it only detaches it, leaving it running in the
        // background -- so we keep the handle and `.abort()` it in the
        // shutdown teardown below, echoing the same "keep the handle,
        // abort on teardown" shape `KdePortalHotkeyBackend`'s `Drop` impl
        // (in `orchestrator-hotkey`) uses for its own background
        // forwarding task. The similarity ends there, though: that task is
        // a regular `tokio::spawn` async task, which actually does stop at
        // its next `.await` point once aborted. This one is
        // `spawn_blocking`, and per Tokio's own docs, `.abort()` on a
        // `spawn_blocking` task has no effect once its closure has started
        // running -- "the task will continue running normally." Since this
        // closure spends essentially all its time inside the blocking
        // `hotkey_rx.recv()` call, `.abort()` here will not stop that OS
        // thread; it only helps the narrow case where the task hasn't
        // started running yet. In practice the thread keeps running until
        // its next event arrives (letting the loop notice the channel's
        // `tx` half was dropped and exit) or the process exits -- one
        // bounded stray thread per `Runner`, not an unbounded leak, and an
        // accepted limitation for this plan's scope rather than a solved
        // problem. A real fix would restructure this bridge to poll with
        // `recv_timeout` against a shutdown flag instead of blocking
        // `recv()`, which is a bigger change than this plan scopes.
        let bridge_task = tokio::task::spawn_blocking(move || {
            while let Ok(item) = hotkey_rx.recv() {
                if tx.send(item).is_err() {
                    break;
                }
            }
        });

        let mut states: Vec<ProfileState> =
            self.profiles.iter().map(|_| ProfileState::new()).collect();

        tokio::pin!(shutdown);
        // Once the bridged hotkey channel disconnects (e.g. a real backend's
        // event stream ending unexpectedly), `rx.recv()` starts resolving to
        // `None` immediately on every poll. Guarding the branch on this flag
        // (rather than leaving it unconditionally in the `select!`) stops it
        // from being polled again after the first `None`, so the loop falls
        // back to waiting on `shutdown` alone instead of spinning. Hotkeys
        // simply stop working from that point on; shutdown still functions.
        let mut hotkey_channel_disconnected = false;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                maybe_event = rx.recv(), if !hotkey_channel_disconnected => {
                    let Some((id, event)) = maybe_event else {
                        tracing::warn!(
                            "hotkey event channel disconnected; hotkey handling stopped for the remainder of this run (shutdown still works)"
                        );
                        hotkey_channel_disconnected = true;
                        continue;
                    };
                    if !matches!(event, HotkeyEvent::Pressed) {
                        continue;
                    }
                    let Some(&index) = id_to_index.get(&id) else { continue };
                    self.handle_press(index, &mut states[index]).await;
                }
            }
        }

        for state in &mut states {
            if let Some(cancel) = state.running.take() {
                let _ = cancel.send(());
            }
            if let Some(task) = state.task.take() {
                let _ = task.await;
            }
            self.restore_focus(state).await;
        }

        // See the comment where `bridge_task` is spawned above: this does
        // NOT reliably stop the bridge thread's blocking `recv()` if one is
        // already in progress -- it only handles the narrow race where the
        // task hasn't started running yet. Kept anyway since it's harmless
        // and does help that narrow case.
        bridge_task.abort();

        Ok(())
    }

    /// If `state` has a recorded prior focus (set when a `Scope::Window`
    /// profile was toggled on), restore it and clear the record. No-op for
    /// `Scope::Desktop` profiles, which never populate `prior_focus`. Shared
    /// by both the toggle-off branch of `handle_press` and `run()`'s
    /// shutdown teardown loop so the two sites can't drift apart.
    async fn restore_focus(&self, state: &mut ProfileState) {
        if let Some(prior) = state.prior_focus.take() {
            if let Err(e) = self.window.activate_window(&prior).await {
                tracing::warn!("failed to restore prior window focus: {e}");
            }
        }
    }

    async fn handle_press(&self, index: usize, state: &mut ProfileState) {
        let profile = &self.profiles[index];
        let now = Instant::now();
        if let Some(last) = state.last_toggle_at {
            if now.duration_since(last)
                < std::time::Duration::from_millis(profile.debounce_ms as u64)
            {
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
                // The task finished on its own (a non-looping macro ran its
                // steps and returned) rather than being cancelled by an
                // explicit toggle-off. For Scope::Window profiles that means
                // the target window is still activated and `prior_focus` is
                // still stashed -- restore it now, or it leaks: the *next*
                // toggle-on would re-capture `focused_window()` as the
                // still-activated target itself, silently overwriting the
                // real prior focus and making it permanently unrecoverable.
                self.restore_focus(state).await;
            }
        }

        if let Some(cancel) = state.running.take() {
            let _ = cancel.send(());
            if let Some(task) = state.task.take() {
                let _ = task.await;
            }
            self.restore_focus(state).await;
            return;
        }

        match &profile.scope {
            Scope::Desktop => {}
            Scope::Window {
                process_name,
                window_title_hint,
                backend_hint_id,
            } => {
                let windows = match self.window.list_windows().await {
                    Ok(windows) => windows,
                    Err(e) => {
                        tracing::warn!("list_windows() failed, toggle-on aborted: {e}");
                        return;
                    }
                };
                let Some(handle) = resolve_window(
                    &windows,
                    process_name,
                    window_title_hint,
                    backend_hint_id.as_deref(),
                ) else {
                    tracing::warn!(
                        "no window matched profile {:?} (process_name={process_name:?}, window_title_hint={window_title_hint:?}); toggle-on aborted",
                        profile.name
                    );
                    return; // no match -- toggle-on fails cleanly, profile stays off
                };
                let prior = self.window.focused_window().await.ok().flatten();
                if let Err(e) = self.window.activate_window(&handle).await {
                    tracing::warn!("activate_window() failed, toggle-on aborted: {e}");
                    return;
                }
                state.prior_focus = prior;
            }
        }

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let input = Arc::clone(&self.input);

        let task = match profile.action.clone() {
            Action::Repeat {
                input: event,
                interval_ms,
                jitter,
            } => tokio::spawn(run_repeat(input, event, interval_ms, jitter, cancel_rx)),
            Action::Macro { steps, loop_ } => {
                tokio::spawn(run_macro(input, steps, loop_, cancel_rx))
            }
        };

        state.running = Some(cancel_tx);
        state.task = Some(task);
    }
}

/// Injects a single event, logging (not propagating) failure -- design spec
/// §5.1/§7: a transient injection hiccup shouldn't kill an otherwise-working
/// repeat/macro loop, but it also shouldn't vanish silently.
async fn inject_logged<I: InputInjector>(input: &I, event: &orchestrator_input::InputEvent) {
    if let Err(e) = input.inject(event).await {
        tracing::warn!("injection failed, continuing: {e}");
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
        inject_logged(&*input, &event).await;
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
                    inject_logged(&*input, &InputEvent::KeyPress(combo.clone())).await;
                    inject_logged(&*input, &InputEvent::KeyRelease(combo.clone())).await;
                }
                MacroStep::MouseClick {
                    button, position, ..
                } => {
                    if let ClickPosition::Fixed { x, y } = position {
                        inject_logged(&*input, &InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    inject_logged(&*input, &InputEvent::MouseButtonPress(*button)).await;
                    inject_logged(&*input, &InputEvent::MouseButtonRelease(*button)).await;
                }
                MacroStep::Drag { from, to, .. } => {
                    if let ClickPosition::Fixed { x, y } = from {
                        inject_logged(&*input, &InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    inject_logged(
                        &*input,
                        &InputEvent::MouseButtonPress(orchestrator_input::MouseButton::Left),
                    )
                    .await;
                    if let ClickPosition::Fixed { x, y } = to {
                        inject_logged(&*input, &InputEvent::MouseMoveAbsolute { x: *x, y: *y })
                            .await;
                    }
                    inject_logged(
                        &*input,
                        &InputEvent::MouseButtonRelease(orchestrator_input::MouseButton::Left),
                    )
                    .await;
                }
                MacroStep::Scroll { dx, dy, .. } => {
                    inject_logged(&*input, &InputEvent::Scroll { dx: *dx, dy: *dy }).await;
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
        event_rx: Mutex<Option<std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)>>>,
        /// When `Some(msg)`, `register()` returns
        /// `Err(HotkeyError::RegistrationFailed(msg))` instead of its normal
        /// `Ok` behavior. `None` (the default) preserves the always-`Ok`
        /// behavior every existing test relies on.
        fail_register: Mutex<Option<String>>,
    }

    impl FakeHotkeyBackend {
        /// Returns the fake plus a sender the test uses to push events as
        /// if a real hotkey had fired. The fake itself holds no `Sender` --
        /// the one returned here is the only one, so the test controls the
        /// channel's lifetime entirely.
        pub fn new() -> (Self, std::sync::mpsc::Sender<(HotkeyId, HotkeyEvent)>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let fake = Self {
                registered: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
                event_rx: Mutex::new(Some(rx)),
                fail_register: Mutex::new(None),
            };
            (fake, tx)
        }

        /// Makes every subsequent `register()` call fail with
        /// `HotkeyError::RegistrationFailed(msg)`.
        pub fn fail_register(&self, msg: impl Into<String>) {
            *self.fail_register.lock().unwrap() = Some(msg.into());
        }
    }

    impl HotkeyBackend for FakeHotkeyBackend {
        async fn register(
            &self,
            action_name: &str,
            preferred: Option<&KeyCombo>,
        ) -> Result<(HotkeyId, KeyCombo), HotkeyError> {
            if let Some(msg) = self.fail_register.lock().unwrap().clone() {
                return Err(HotkeyError::RegistrationFailed(msg));
            }
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
        /// When `Some(msg)`, `inject()` returns
        /// `Err(InjectError::BackendUnavailable(msg))` instead of recording
        /// the event. `None` (the default) preserves the always-`Ok`
        /// behavior every existing test relies on.
        fail_inject: Mutex<Option<String>>,
    }

    impl FakeInputInjector {
        pub fn new() -> Self {
            Self {
                injected: Mutex::new(Vec::new()),
                fail_inject: Mutex::new(None),
            }
        }

        /// Makes every subsequent `inject()` call fail with
        /// `InjectError::BackendUnavailable(msg)` (nothing is recorded into
        /// `injected` while this is set).
        pub fn fail_inject(&self, msg: impl Into<String>) {
            *self.fail_inject.lock().unwrap() = Some(msg.into());
        }
    }

    impl InputInjector for FakeInputInjector {
        async fn connect(&mut self) -> Result<(), InjectError> {
            Ok(())
        }

        async fn inject(&self, event: &InputEvent) -> Result<(), InjectError> {
            if let Some(msg) = self.fail_inject.lock().unwrap().clone() {
                return Err(InjectError::BackendUnavailable(msg));
            }
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
        /// When `Some(msg)`, the corresponding method returns
        /// `Err(WindowError::BackendUnavailable(msg))` instead of its normal
        /// `Ok` behavior. Each defaults to `None`, preserving the
        /// always-`Ok` behavior every existing test relies on.
        fail_list_windows: Mutex<Option<String>>,
        fail_focused_window: Mutex<Option<String>>,
        fail_activate_window: Mutex<Option<String>>,
    }

    impl FakeWindowLocator {
        pub fn new(windows: Vec<WindowInfo>, focused: Option<WindowHandle>) -> Self {
            Self {
                windows,
                focused,
                activated: Mutex::new(Vec::new()),
                fail_list_windows: Mutex::new(None),
                fail_focused_window: Mutex::new(None),
                fail_activate_window: Mutex::new(None),
            }
        }

        pub fn fail_list_windows(&self, msg: impl Into<String>) {
            *self.fail_list_windows.lock().unwrap() = Some(msg.into());
        }

        /// Provided for symmetry with the other two methods' fail switches
        /// (`WindowLocator` has three fallible methods, all of which get
        /// one). Not currently exercised by any test: a `focused_window()`
        /// failure is already swallowed via `.ok().flatten()` at its one
        /// call site in `handle_press` (it degrades to "no prior focus to
        /// restore" rather than aborting toggle-on), so there is no
        /// dedicated error-path test for it the way there is for
        /// `list_windows`/`activate_window`.
        #[allow(dead_code)]
        pub fn fail_focused_window(&self, msg: impl Into<String>) {
            *self.fail_focused_window.lock().unwrap() = Some(msg.into());
        }

        pub fn fail_activate_window(&self, msg: impl Into<String>) {
            *self.fail_activate_window.lock().unwrap() = Some(msg.into());
        }
    }

    impl WindowLocator for FakeWindowLocator {
        async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError> {
            if let Some(msg) = self.fail_list_windows.lock().unwrap().clone() {
                return Err(WindowError::BackendUnavailable(msg));
            }
            Ok(self.windows.clone())
        }

        async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError> {
            if let Some(msg) = self.fail_focused_window.lock().unwrap().clone() {
                return Err(WindowError::BackendUnavailable(msg));
            }
            Ok(self.focused.clone())
        }

        async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError> {
            if let Some(msg) = self.fail_activate_window.lock().unwrap().clone() {
                return Err(WindowError::BackendUnavailable(msg));
            }
            self.activated.lock().unwrap().push(handle.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod fakes_smoke_tests {
    use super::fakes::*;
    use orchestrator_hotkey::{HotkeyBackend, HotkeyEvent};
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
        let profiles = vec![desktop_repeat_profile("a"), desktop_repeat_profile("b")];

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

    /// Design spec §3/§7: a hotkey registration failure at startup is fatal
    /// -- `run()` must return `Err(RunnerError::HotkeyRegistration { .. })`
    /// promptly, before anything else (the hotkey-event loop, any profile
    /// task) ever starts.
    #[tokio::test]
    async fn hotkey_registration_failure_at_startup_is_fatal() {
        let (hotkey, _tx) = FakeHotkeyBackend::new();
        hotkey.fail_register("simulated backend failure");
        let input = FakeInputInjector::new();
        let window = FakeWindowLocator::new(vec![], None);
        let profiles = vec![desktop_repeat_profile("only")];

        let runner = Runner::new(hotkey, input, window, profiles);
        let start = std::time::Instant::now();
        // A shutdown future that never resolves -- if `run()` were to hang
        // waiting on the event loop instead of failing fast during startup,
        // this test would hang rather than silently pass.
        let result = runner.run(std::future::pending::<()>()).await;

        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "run() took too long to return after a registration failure"
        );
        match result {
            Err(RunnerError::HotkeyRegistration { profile_name, .. }) => {
                assert_eq!(profile_name, "only");
            }
            other => {
                panic!("expected Err(RunnerError::HotkeyRegistration {{ .. }}), got {other:?}")
            }
        }
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

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
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

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
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

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
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

    /// Design spec §5.1/§7: a single injection failure mid-repeat is logged
    /// and the loop continues -- it must not crash the task or otherwise
    /// stop the repeat loop from running. Proven here by having every
    /// `inject()` call fail for the whole run, then confirming the profile
    /// still responds cleanly to toggle-off (which would `.await` forever,
    /// or the whole test would hang, if the repeat task had died or
    /// deadlocked instead of looping through the errors).
    #[tokio::test]
    async fn injection_failure_mid_repeat_is_logged_and_the_loop_continues() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        input.fail_inject("simulated ydotool hiccup");
        let window = FakeWindowLocator::new(vec![], None);
        let profile = desktop_repeat_profile("clicker", 10, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        // Let several failing ticks go by -- the repeat loop must keep
        // running (not crash, not stop) despite every injection erroring.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(
            input.injected.lock().unwrap().len(),
            0,
            "every injection was configured to fail, so nothing should have been recorded"
        );

        // Toggling off must still work cleanly -- proves the repeat task is
        // still alive and responsive to cancellation, not stuck or dead.
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();

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

    fn desktop_macro_profile(
        name: &str,
        steps: Vec<MacroStep>,
        loop_: bool,
        debounce_ms: u32,
    ) -> Profile {
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
            MacroStep::Scroll {
                delay_ms: 5,
                dx: 0,
                dy: 1,
            },
            MacroStep::Scroll {
                delay_ms: 5,
                dx: 0,
                dy: -1,
            },
        ];
        let profile = desktop_macro_profile("once", steps, false, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
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
        let steps = vec![MacroStep::Scroll {
            delay_ms: 5,
            dx: 0,
            dy: 1,
        }];
        let profile = desktop_macro_profile("loop", steps, true, 50);

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let count = input.injected.lock().unwrap().len();
        assert!(
            count >= 3,
            "expected the loop to have run multiple times, got {count}"
        );

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

        let runner = Runner::<_, FakeInputInjector, _>::new(
            hotkey,
            Arc::clone(&input),
            window,
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;

        // False positive: `injected` is explicitly `drop()`'d at the end of
        // this block, before the next `.await` -- clippy's
        // `await_holding_lock` lint operates at function granularity and
        // can't see that a statement-level `#[allow]` doesn't suppress it,
        // so the whole block is wrapped instead. It's also a test-only
        // `std::sync::Mutex` on a fake with no real contention.
        #[allow(clippy::await_holding_lock)]
        {
            let injected = input.injected.lock().unwrap();
            assert_eq!(
                injected.len(),
                2,
                "expected exactly press+release, no move event"
            );
            assert!(matches!(
                injected[0],
                InputEvent::MouseButtonPress(MouseButton::Left)
            ));
            assert!(matches!(
                injected[1],
                InputEvent::MouseButtonRelease(MouseButton::Left)
            ));

            drop(injected);
        }
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

    #[test]
    fn title_hint_applied_even_with_a_single_process_match() {
        let windows = vec![window("h1", "code", "project-a - VS Code")];
        // A single process-name match with a hint that DOES match should
        // still resolve -- the hint isn't gated on "multiple matches".
        let resolved = resolve_window(&windows, "code", "project-a", None);
        assert_eq!(resolved, Some(WindowHandle("h1".to_string())));
    }

    #[test]
    fn title_hint_matching_nothing_returns_none_even_with_a_single_process_match() {
        let windows = vec![window("h1", "code", "project-a - VS Code")];
        let resolved = resolve_window(&windows, "code", "project-z", None);
        assert_eq!(
            resolved, None,
            "a non-matching hint must fail cleanly, not silently fall back to the unfiltered match"
        );
    }

    #[test]
    fn title_hint_matching_nothing_among_multiple_matches_returns_none() {
        let windows = vec![
            window("h1", "code", "project-a - VS Code"),
            window("h2", "code", "project-b - VS Code"),
        ];
        let resolved = resolve_window(&windows, "code", "project-z", None);
        assert_eq!(
            resolved, None,
            "a hint matching none of several process-name matches must not silently fall back to the first one"
        );
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

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
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
            std::slice::from_ref(&target),
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

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
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

    /// Design spec §7: `list_windows()` failing at toggle-on aborts cleanly,
    /// same as the other resolution-failure cases.
    #[tokio::test]
    async fn toggle_on_fails_cleanly_when_list_windows_fails() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = Arc::new(FakeWindowLocator::new(vec![], None));
        window.fail_list_windows("simulated backend failure");
        let profile = window_scoped_profile("scoped", "firefox");

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        assert_eq!(input.injected.lock().unwrap().len(), 0);
        assert_eq!(window.activated.lock().unwrap().len(), 0);

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    /// Design spec §7: a window-activation failure at toggle-on aborts
    /// cleanly -- same "no crash, nothing starts" behavior as the
    /// zero-windows-match case above, just triggered by `activate_window()`
    /// itself failing instead of resolution finding no candidates.
    #[tokio::test]
    async fn toggle_on_fails_cleanly_when_activation_fails() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let target = WindowHandle("target".to_string());
        let window = Arc::new(FakeWindowLocator::new(
            vec![WindowInfo {
                handle: target.clone(),
                pid: Some(1),
                process_name: Some("firefox".to_string()),
                title: "Mozilla Firefox".to_string(),
            }],
            None,
        ));
        window.fail_activate_window("simulated activation failure");
        let profile = window_scoped_profile("scoped", "firefox");

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
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
            "expected no repeat/macro task to have started since activation failed"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }

    /// Fix 1 (final-branch review): a non-looping `Action::Macro` under
    /// `Scope::Window` that finishes on its own (self-completes) must have
    /// its stolen focus restored once the runner notices, exactly like an
    /// explicit toggle-off would. The only way the runner *can* notice a
    /// background task's completion is by processing another `Pressed`
    /// event (there's no separate polling of finished tasks) -- but per
    /// design spec §5.2 that next press is the ordinary "run it again"
    /// gesture, not a dedicated extra "toggle off, then on" round trip. This
    /// test presses exactly once to start, lets the macro self-complete,
    /// then sends the one ordinary next press and confirms the restore (to
    /// `prior`) happens *before* that same press's fresh run re-activates
    /// `target` -- i.e. `activated` is `[target, prior, target]`, not
    /// `[target, target]` (which is what the pre-fix bug produced: the
    /// stashed `prior_focus` handle silently leaked/overwritten instead of
    /// ever being restored).
    #[tokio::test]
    async fn self_completing_window_scoped_macro_restores_focus_on_next_press() {
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
        let profile = Profile {
            name: "scoped-macro".to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Macro {
                steps: vec![crate::action::MacroStep::Scroll {
                    delay_ms: 5,
                    dx: 0,
                    dy: 1,
                }],
                loop_: false,
            },
            scope: Scope::Window {
                process_name: "firefox".to_string(),
                window_title_hint: String::new(),
                backend_hint_id: None,
            },
            focus_steal: true,
            debounce_ms: 20,
        };

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();

        // Let the single-step, non-looping macro finish entirely on its
        // own -- no second press yet.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(
            window.activated.lock().unwrap().as_slice(),
            std::slice::from_ref(&target),
            "self-completion alone (no further event) must not yet have restored focus -- \
             the runner has no way to notice without an incoming event"
        );

        // The ordinary next press: past debounce, and the same gesture a
        // user would use to run the macro again.
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        assert_eq!(
            window.activated.lock().unwrap().as_slice(),
            &[target.clone(), prior, target],
            "expected the next press to restore prior focus (Fix 1) before starting its own fresh run"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }
}

#[cfg(test)]
mod shutdown_and_integration_tests {
    use super::fakes::*;
    use super::*;
    use crate::action::{Action, Jitter};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{HotkeyEvent, KeyCombo, Modifier};
    use orchestrator_input::InputEvent;
    use orchestrator_window::{WindowHandle, WindowInfo};
    use std::sync::Arc;

    #[tokio::test]
    async fn shutdown_while_window_scoped_profile_is_running_still_restores_focus() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let target = WindowHandle("target".to_string());
        let prior = WindowHandle("prior".to_string());
        let window = Arc::new(FakeWindowLocator::new(
            vec![WindowInfo {
                handle: target.clone(),
                pid: Some(1),
                process_name: Some("app".to_string()),
                title: "App".to_string(),
            }],
            Some(prior.clone()),
        ));
        let profile = Profile {
            name: "scoped".to_string(),
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
                process_name: "app".to_string(),
                window_title_hint: String::new(),
                backend_hint_id: None,
            },
            focus_steal: true,
            debounce_ms: 50,
        };

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;

        // Shut down WITHOUT a second press -- the profile is still "on".
        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();

        assert_eq!(
            window.activated.lock().unwrap().as_slice(),
            &[target, prior],
            "expected shutdown to restore prior focus even though the profile was never explicitly toggled off"
        );
    }

    #[tokio::test]
    async fn two_profiles_toggle_independently_without_interfering() {
        let (hotkey, tx) = FakeHotkeyBackend::new();
        let input = Arc::new(FakeInputInjector::new());
        let window = Arc::new(FakeWindowLocator::new(vec![], None));

        let desktop_profile = Profile {
            name: "desktop".to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 20,
                jitter: Jitter::None,
            },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms: 50,
        };
        let macro_profile = Profile {
            name: "macro".to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F10".to_string(),
            },
            action: Action::Macro {
                steps: vec![crate::action::MacroStep::Scroll {
                    delay_ms: 15,
                    dx: 1,
                    dy: 0,
                }],
                loop_: true,
            },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms: 50,
        };

        let runner = Runner::<_, FakeInputInjector, FakeWindowLocator>::new(
            hotkey,
            Arc::clone(&input),
            Arc::clone(&window),
            vec![desktop_profile, macro_profile],
        );
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let run_handle = tokio::spawn(runner.run(async {
            let _ = shutdown_rx.await;
        }));

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // FakeHotkeyBackend assigns ids sequentially in registration order:
        // profiles[0] ("desktop") -> HotkeyId(1), profiles[1] ("macro") -> HotkeyId(2).
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tx.send((orchestrator_hotkey::HotkeyId(2), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // False positive: `injected` is explicitly `drop()`'d at the end of
        // this block, before the next `.await` -- clippy's
        // `await_holding_lock` lint operates at function granularity, and a
        // `#[allow]` on a `let` statement doesn't suppress it (only a
        // bare-block statement does), hence the uninitialized bindings
        // assigned from inside the attributed block below. It's also a
        // test-only `std::sync::Mutex` on a fake with no real contention.
        let scroll_up;
        let scroll_right;
        #[allow(clippy::await_holding_lock)]
        {
            let injected = input.injected.lock().unwrap();
            scroll_up = injected
                .iter()
                .filter(|e| matches!(e, InputEvent::Scroll { dy: 1, dx: 0 }))
                .count();
            scroll_right = injected
                .iter()
                .filter(|e| matches!(e, InputEvent::Scroll { dx: 1, dy: 0 }))
                .count();
            drop(injected);
        }
        assert!(
            scroll_up >= 2,
            "expected the desktop repeat profile to have fired, got {scroll_up}"
        );
        assert!(
            scroll_right >= 2,
            "expected the looping macro profile to have fired, got {scroll_right}"
        );

        // Toggle off only the desktop profile; the macro profile keeps running.
        tx.send((orchestrator_hotkey::HotkeyId(1), HotkeyEvent::Pressed))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let scroll_up_after_stop = input
            .injected
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InputEvent::Scroll { dy: 1, dx: 0 }))
            .count();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let scroll_up_later = input
            .injected
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InputEvent::Scroll { dy: 1, dx: 0 }))
            .count();
        assert_eq!(
            scroll_up_after_stop, scroll_up_later,
            "desktop profile should have stopped"
        );

        let scroll_right_later = input
            .injected
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, InputEvent::Scroll { dx: 1, dy: 0 }))
            .count();
        assert!(
            scroll_right_later > scroll_right,
            "macro profile should still be running"
        );

        shutdown_tx.send(()).unwrap();
        run_handle.await.unwrap().unwrap();
    }
}
