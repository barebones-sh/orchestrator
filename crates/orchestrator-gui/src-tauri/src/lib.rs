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

fn emit_status(app: &tauri::AppHandle, state: &RunnerState) {
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
fn do_start_runner(app: tauri::AppHandle) -> Result<(), String> {
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
    tokio::spawn(async move {
        let result = match build_runner(config.profiles).await {
            Ok(runner) => runner
                .run(stop_for_task.notified())
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        let handle = app_for_task.state::<RunnerHandle>();
        let mut state = handle.state.lock().unwrap();
        *state = match result {
            Ok(()) => RunnerState::Idle,
            Err(e) => RunnerState::Error(e),
        };
        emit_status(&app_for_task, &state);
    });

    let mut state = handle.state.lock().unwrap();
    *state = RunnerState::Running { stop };
    emit_status(&app, &state);
    Ok(())
}

#[cfg(target_os = "linux")]
fn do_stop_runner(app: tauri::AppHandle) -> Result<(), String> {
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
fn do_start_runner(_app: tauri::AppHandle) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}

#[cfg(not(target_os = "linux"))]
fn do_stop_runner(_app: tauri::AppHandle) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}

#[tauri::command]
fn start_runner(app: tauri::AppHandle) -> Result<(), String> {
    do_start_runner(app)
}

#[tauri::command]
fn stop_runner(app: tauri::AppHandle) -> Result<(), String> {
    do_stop_runner(app)
}

#[tauri::command]
fn runner_status(handle: tauri::State<RunnerHandle>) -> RunnerStatusDto {
    let state = handle.state.lock().unwrap();
    status_dto(&state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
