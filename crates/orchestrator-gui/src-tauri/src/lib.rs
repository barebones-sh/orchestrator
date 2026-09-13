// Tauri commands need the same "no config file yet = start empty, but a real
// parse/validate error is fatal" behavior `orchestrator-cli/src/main.rs`'s
// `load_or_default_config`/`is_first_run_missing_file` already implement.
// Reusing those isn't possible without changing `orchestrator-cli` (out of
// scope here); this is a small, disclosed duplication, the same size as the
// original CLI helper.
fn load_or_default_config(path: &std::path::Path) -> Result<orchestrator_core::Config, String> {
    match orchestrator_core::Config::load(path) {
        Ok(config) => Ok(config),
        Err(orchestrator_core::ConfigError::Io(io_err))
            if io_err.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(orchestrator_core::Config {
                schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION,
                profiles: vec![],
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

struct RunnerHandle {
    state: std::sync::Mutex<RunnerState>,
}

// `Running`/`Error` are only ever *constructed* by the Linux-gated
// `do_start_runner`/`do_stop_runner`; on other targets they are merely
// pattern-matched (in `status_dto`), which doesn't count as a use for
// dead-code analysis, so `cargo clippy -D warnings` on macOS would fail on
// them. Same attribute/reasoning already used for the window-only items in
// `orchestrator-cli/src/profile_commands.rs` and
// `orchestrator-core/src/profile_ops.rs`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum RunnerState {
    Idle,
    Running {
        stop: std::sync::Arc<tokio::sync::Notify>,
    },
    Error(String),
}

#[derive(Clone, serde::Serialize)]
struct RunnerStatusDto {
    state: String,
    error: Option<String>,
}

fn status_dto(state: &RunnerState) -> RunnerStatusDto {
    match state {
        RunnerState::Idle => RunnerStatusDto {
            state: "idle".to_string(),
            error: None,
        },
        RunnerState::Running { .. } => RunnerStatusDto {
            state: "running".to_string(),
            error: None,
        },
        RunnerState::Error(e) => RunnerStatusDto {
            state: "error".to_string(),
            error: Some(e.clone()),
        },
    }
}

// Only called from the Linux-gated start/stop paths (see `RunnerState` above
// for the same reasoning), and generic over the Tauri runtime so the mock
// runtime in this file's tests can drive it too.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn emit_status<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &RunnerState) {
    use tauri::Emitter;
    let _ = app.emit("runner-status-changed", status_dto(state));
}

#[tauri::command]
fn list_profiles() -> Result<Vec<orchestrator_core::Profile>, String> {
    let path = orchestrator_core::Config::config_path();
    Ok(load_or_default_config(&path)?.profiles)
}

#[tauri::command]
fn add_profile(
    name: String,
    scope: orchestrator_core::Scope,
    action: orchestrator_core::Action,
    debounce_ms: Option<u32>,
) -> Result<(), String> {
    let path = orchestrator_core::Config::config_path();
    let mut config = load_or_default_config(&path)?;
    orchestrator_core::profile_ops::add_profile(
        &mut config,
        orchestrator_core::profile_ops::AddProfileArgs {
            name,
            scope,
            action,
            debounce_ms,
        },
    )
    .map_err(|e| e.to_string())?;
    config.validate().map_err(|e| e.to_string())?;
    config.save(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn edit_profile(
    name: String,
    scope: Option<orchestrator_core::Scope>,
    action: Option<orchestrator_core::Action>,
    debounce_ms: Option<u32>,
) -> Result<(), String> {
    let path = orchestrator_core::Config::config_path();
    let mut config = load_or_default_config(&path)?;
    orchestrator_core::profile_ops::edit_profile(
        &mut config,
        &name,
        orchestrator_core::profile_ops::EditProfileArgs {
            scope,
            action,
            debounce_ms,
        },
    )
    .map_err(|e| e.to_string())?;
    config.validate().map_err(|e| e.to_string())?;
    config.save(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_profile(name: String) -> Result<(), String> {
    let path = orchestrator_core::Config::config_path();
    let mut config = load_or_default_config(&path)?;
    // Deliberately no `config.validate()` before saving here: removing a
    // profile cannot introduce a new validation failure (same reasoning
    // `orchestrator-cli/src/main.rs`'s `Remove` handler already documents).
    orchestrator_core::profile_ops::remove_profile(&mut config, &name)
        .map_err(|e| e.to_string())?;
    config.save(&path).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
#[tauri::command]
async fn list_windows() -> Result<Vec<orchestrator_window::WindowInfo>, String> {
    use orchestrator_window::kwin_dbus::KwinWindowLocator;
    use orchestrator_window::WindowLocator;
    let locator = KwinWindowLocator::new().await.map_err(|e| e.to_string())?;
    locator.list_windows().await.map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
#[tauri::command]
async fn list_windows() -> Result<Vec<orchestrator_window::WindowInfo>, String> {
    Err("window listing is only implemented on Linux in this increment".to_string())
}

#[cfg(target_os = "linux")]
const APP_ID: &str = "io.github.barebonessh.Orchestrator";

#[cfg(target_os = "linux")]
async fn build_runner(
    profiles: Vec<orchestrator_core::Profile>,
) -> Result<
    orchestrator_core::runner::Runner<
        orchestrator_hotkey::kde_portal_shortcuts::KdePortalHotkeyBackend,
        orchestrator_input::linux_wayland::YdotoolInputInjector,
        orchestrator_window::kwin_dbus::KwinWindowLocator,
    >,
    String,
> {
    use orchestrator_hotkey::kde_portal_shortcuts::KdePortalHotkeyBackend;
    use orchestrator_input::linux_wayland::YdotoolInputInjector;
    use orchestrator_input::InputInjector;
    use orchestrator_window::kwin_dbus::KwinWindowLocator;

    let hotkey = KdePortalHotkeyBackend::new(APP_ID)
        .await
        .map_err(|e| format!("failed to construct hotkey backend: {e}"))?;
    let window = KwinWindowLocator::new()
        .await
        .map_err(|e| format!("failed to construct window locator: {e}"))?;
    let mut input = YdotoolInputInjector::new();
    input
        .connect()
        .await
        .map_err(|e| format!("failed to connect input injector: {e}"))?;

    Ok(orchestrator_core::runner::Runner::new(
        hotkey, input, window, profiles,
    ))
}

#[cfg(target_os = "linux")]
fn do_start_runner<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    use tauri::Manager;

    let handle = app.state::<RunnerHandle>();
    {
        let state = handle.state.lock().unwrap();
        if matches!(&*state, RunnerState::Running { .. }) {
            return Err("already running".to_string());
        }
    }

    let path = orchestrator_core::Config::config_path();
    let config = load_or_default_config(&path)?;

    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stop_for_task = stop.clone();
    let app_for_task = app.clone();

    // final-review Fix 2 (part 1): publish `Running { stop }` *before*
    // spawning, not after. With the write after the spawn, a task that
    // failed fast (e.g. `build_runner` erroring before the portal is even
    // reachable) could write `Error(..)` first and then have it immediately
    // clobbered by this `Running` write, leaving the UI claiming "Running"
    // for a run that never started.
    {
        let mut state = handle.state.lock().unwrap();
        *state = RunnerState::Running { stop };
        emit_status(&app, &state);
    }

    // final-review Fix 1: `tauri::async_runtime::spawn`, NOT `tokio::spawn`.
    // Synchronous `#[tauri::command]`s run inline on the caller's thread and
    // the tray's `on_menu_event` fires on the main event-loop thread; neither
    // has entered a Tokio runtime, so `tokio::spawn` panics there -- and
    // because that happens across the webview/event-loop FFI boundary the
    // panic aborts the process instead of unwinding. Tauri owns its own
    // Tokio runtime and `async_runtime::spawn` schedules onto it from any
    // thread, runtime context or not.
    tauri::async_runtime::spawn(async move {
        let result = match build_runner(config.profiles).await {
            Ok(runner) => runner
                .run(stop_for_task.notified())
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        let handle = app_for_task.state::<RunnerHandle>();
        let mut state = handle.state.lock().unwrap();
        // final-review Fix 2 (part 2): only the *current* run may publish its
        // own completion. A Stop immediately followed by a Start leaves the
        // previous task still tearing down (unregistering hotkeys, restoring
        // focus -- real D-Bus round-trips); without this identity check that
        // stale task's unconditional `Idle` write would clobber the newer
        // run's `Running { stop2 }`, dropping the only live reference to
        // `stop2` and leaving an unstoppable Runner behind a UI showing
        // "Idle". `Arc::ptr_eq` against our own stop handle makes a stale
        // completion a no-op.
        let is_current_run = matches!(
            &*state,
            RunnerState::Running { stop } if std::sync::Arc::ptr_eq(stop, &stop_for_task)
        );
        if is_current_run {
            *state = match result {
                Ok(()) => RunnerState::Idle,
                Err(e) => RunnerState::Error(e),
            };
            emit_status(&app_for_task, &state);
        }
    });

    Ok(())
}

#[cfg(target_os = "linux")]
fn do_stop_runner<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    use tauri::Manager;

    let handle = app.state::<RunnerHandle>();
    let mut state = handle.state.lock().unwrap();
    match &*state {
        RunnerState::Running { stop } => {
            stop.notify_one();
            *state = RunnerState::Idle;
            emit_status(&app, &state);
            Ok(())
        }
        _ => Err("not running".to_string()),
    }
}

#[cfg(not(target_os = "linux"))]
fn do_start_runner<R: tauri::Runtime>(_app: tauri::AppHandle<R>) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}

#[cfg(not(target_os = "linux"))]
fn do_stop_runner<R: tauri::Runtime>(_app: tauri::AppHandle<R>) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}

#[tauri::command]
fn start_runner<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    do_start_runner(app)
}

#[tauri::command]
fn stop_runner<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    do_stop_runner(app)
}

#[tauri::command]
fn runner_status(handle: tauri::State<RunnerHandle>) -> RunnerStatusDto {
    let state = handle.state.lock().unwrap();
    status_dto(&state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // final-review Fix 4: without a subscriber every `tracing::warn!` /
    // `tracing::error!` the embedded `Runner` emits (hotkey channel
    // disconnects, window-match failures, injection failures) is discarded,
    // so a silent partial failure would leave the status panel showing
    // "Running" with no diagnostic signal anywhere. `orchestrator-cli`'s
    // `linux_run::run` installs one the same way, first thing before setup.
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(RunnerHandle {
            state: std::sync::Mutex::new(RunnerState::Idle),
        })
        .setup(|app| {
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            use tauri::Manager;

            if let Some(window) = app.get_webview_window("main") {
                let window_for_close = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                });
            }

            let start_item = MenuItem::with_id(app, "start", "Start", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let show_item = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&start_item, &stop_item, &show_item, &quit_item])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "start" => {
                        let _ = do_start_runner(app.clone());
                    }
                    "stop" => {
                        let _ = do_stop_runner(app.clone());
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_profiles,
            add_profile,
            edit_profile,
            remove_profile,
            list_windows,
            start_runner,
            stop_runner,
            runner_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Regression test for the final review's Fix 1: `do_start_runner` used to
    /// call `tokio::spawn`, which panics (and, across the webview/event-loop
    /// FFI boundary, aborts the process) when there is no Tokio runtime
    /// entered on the calling thread — which is exactly the situation for
    /// both of its real callers (a synchronous `#[tauri::command]`, which runs
    /// inline on the IPC thread, and the tray's `on_menu_event`, which fires
    /// on the main event-loop thread).
    ///
    /// This is deliberately a plain `#[test]`, **not** `#[tokio::test]`: that
    /// macro would enter a Tokio runtime on the test thread and make the very
    /// bug under test disappear. The `try_current()` assertion below pins that
    /// down so the test can't silently rot into a runtime-having context.
    #[test]
    fn start_runner_spawns_from_a_thread_with_no_tokio_runtime() {
        // Keep this hermetic and, above all, keep it off the developer's real
        // desktop session: point the config lookup at a directory that does
        // not exist (so `load_or_default_config` yields zero profiles and the
        // `Runner` registers no global shortcuts / pops no KDE dialog), and
        // point the session bus at an unreachable address so `build_runner`'s
        // portal connection fails fast instead of opening a real portal
        // session. Neither is required for the spawn-context assertion itself;
        // both exist so running `cargo test` never touches a live desktop.
        let missing_config_home = std::env::temp_dir().join(format!(
            "orchestrator-gui-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("XDG_CONFIG_HOME", &missing_config_home);
        std::env::set_var(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/orchestrator-gui-test-bus",
        );

        // The whole point: production calls `do_start_runner` from a thread
        // that has *not* entered a Tokio runtime. If this ever starts failing,
        // the test has stopped testing the thing that broke.
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test thread unexpectedly has an ambient Tokio runtime; this test \
             must run in the runtime-less context production actually uses"
        );

        // Direct, deterministic proof that the spawn function production now
        // uses works from this runtime-less thread (the original `tokio::spawn`
        // would panic right here).
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        tauri::async_runtime::spawn(async move {
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("task spawned via tauri::async_runtime::spawn never ran");

        let app = tauri::test::mock_builder()
            .manage(RunnerHandle {
                state: std::sync::Mutex::new(RunnerState::Idle),
            })
            .invoke_handler(tauri::generate_handler![
                list_profiles,
                add_profile,
                edit_profile,
                remove_profile,
                list_windows,
                start_runner,
                stop_runner,
                runner_status
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("failed to build mock app");

        // The real assertion: this must return, not abort the process.
        let result = do_start_runner(app.handle().clone());
        assert!(
            result.is_ok(),
            "do_start_runner should succeed with an empty config, got {result:?}"
        );

        // Fix 2 (part 1) corollary: `Running` is published *before* the spawn,
        // so it is already visible the moment `do_start_runner` returns. It may
        // have since become `Error(..)` if the spawned task already failed to
        // reach the (deliberately unreachable) session bus — what must never be
        // observed here is `Idle`.
        {
            use tauri::Manager;
            let handle = app.handle().state::<RunnerHandle>();
            let state = handle.state.lock().unwrap();
            assert!(
                matches!(&*state, RunnerState::Running { .. } | RunnerState::Error(_)),
                "expected Running (or an already-failed Error), got Idle"
            );
        }

        // Signal any runner that did manage to start to shut down; `Notify`
        // stores the permit, so this is effective even if the task has not
        // reached its `notified()` await yet.
        let _ = do_stop_runner(app.handle().clone());
    }
}
