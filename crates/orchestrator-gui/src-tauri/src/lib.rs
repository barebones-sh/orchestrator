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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_profiles,
            add_profile,
            edit_profile,
            remove_profile,
            list_windows
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
