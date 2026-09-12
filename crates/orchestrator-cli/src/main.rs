use clap::{Parser, Subcommand};

mod notation;
mod profile_commands;

use profile_commands::{ProfileActionArgs, ProfileAddArgs, ProfileEditArgs};

/// Orchestrator: global-hotkey input automation.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the config file. Defaults to the platform config directory
    /// (see `orchestrator_core::Config::config_path`).
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Profile management: add, edit, remove, or list profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),

    /// Load the config and run all profiles until Ctrl-C.
    Run,
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Add a new profile.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        scope: ScopeArg,
        #[arg(long)]
        action: ActionArg,
        /// Single input for --action repeat (key:<combo> or scroll:<dx,dy>).
        #[arg(long)]
        input: Option<String>,
        #[arg(long)]
        interval_ms: Option<u64>,
        #[arg(long)]
        jitter_ms: Option<u64>,
        /// Repeatable for --action macro.
        #[arg(long)]
        step: Vec<String>,
        #[arg(long)]
        r#loop: bool,
        #[arg(long)]
        debounce_ms: Option<u32>,
    },
    /// Edit an existing profile. Only given flags change.
    Edit {
        name: String,
        #[arg(long)]
        scope: Option<ScopeArg>,
        #[arg(long)]
        action: Option<ActionArg>,
        #[arg(long)]
        input: Option<String>,
        #[arg(long)]
        interval_ms: Option<u64>,
        #[arg(long)]
        jitter_ms: Option<u64>,
        #[arg(long)]
        step: Vec<String>,
        #[arg(long)]
        r#loop: bool,
        #[arg(long)]
        debounce_ms: Option<u32>,
    },
    /// Remove a profile by name.
    Remove { name: String },
    /// List all profiles.
    List,
}

#[derive(Clone, clap::ValueEnum)]
enum ScopeArg {
    Desktop,
    Window,
}

#[derive(Clone, clap::ValueEnum)]
enum ActionArg {
    Repeat,
    Macro,
}

// Only ever called from `linux_window_picker` (Linux-only): `tokio` is a
// Linux-only dependency per Cargo.toml's target-specific dependency tables
// (backend selection per design spec §4), so this must be cfg-gated too or
// non-Linux builds (e.g. `cargo check --target x86_64-apple-darwin`) fail to
// resolve the `tokio` crate at all.
#[cfg(target_os = "linux")]
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("failed to build tokio runtime")
}

fn config_path(override_path: &Option<std::path::PathBuf>) -> std::path::PathBuf {
    override_path.clone().unwrap_or_else(orchestrator_core::Config::config_path)
}

fn load_or_default_config(path: &std::path::Path) -> orchestrator_core::Config {
    orchestrator_core::Config::load(path).unwrap_or_else(|_| orchestrator_core::Config {
        schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION,
        profiles: vec![],
    })
}

fn main() {
    let cli = Cli::parse();
    let path = config_path(&cli.config);

    match cli.command {
        None => {}
        Some(Command::Profile(ProfileCommand::Add { name, scope, action, input, interval_ms, jitter_ms, step, r#loop, debounce_ms })) => {
            let mut config = load_or_default_config(&path);
            let resolved_scope = match scope {
                ScopeArg::Desktop => orchestrator_core::profile::Scope::Desktop,
                #[cfg(target_os = "linux")]
                ScopeArg::Window => linux_window_picker::pick_window(),
                #[cfg(not(target_os = "linux"))]
                ScopeArg::Window => {
                    eprintln!("--scope window's live picker is only implemented on Linux");
                    std::process::exit(1);
                }
            };
            let action_args = build_action_args(action, input, interval_ms, jitter_ms, step, r#loop);
            match profile_commands::add_profile(&mut config, ProfileAddArgs { name, scope: resolved_scope, action: action_args, debounce_ms }) {
                Ok(()) => match config.save(&path) {
                    Ok(()) => println!("profile added."),
                    Err(e) => { eprintln!("failed to save config: {e}"); std::process::exit(1); }
                },
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            }
        }
        Some(Command::Profile(ProfileCommand::Edit { name, scope, action, input, interval_ms, jitter_ms, step, r#loop, debounce_ms })) => {
            let mut config = load_or_default_config(&path);
            let resolved_scope = scope.map(|s| match s {
                ScopeArg::Desktop => orchestrator_core::profile::Scope::Desktop,
                #[cfg(target_os = "linux")]
                ScopeArg::Window => linux_window_picker::pick_window(),
                #[cfg(not(target_os = "linux"))]
                ScopeArg::Window => { eprintln!("--scope window's live picker is only implemented on Linux"); std::process::exit(1); }
            });
            let action_args = action.map(|a| build_action_args(a, input, interval_ms, jitter_ms, step, r#loop));
            match profile_commands::edit_profile(&mut config, &name, ProfileEditArgs { scope: resolved_scope, action: action_args, debounce_ms }) {
                Ok(()) => match config.save(&path) {
                    Ok(()) => println!("profile updated."),
                    Err(e) => { eprintln!("failed to save config: {e}"); std::process::exit(1); }
                },
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            }
        }
        Some(Command::Profile(ProfileCommand::Remove { name })) => {
            let mut config = load_or_default_config(&path);
            match profile_commands::remove_profile(&mut config, &name) {
                Ok(()) => match config.save(&path) {
                    Ok(()) => println!("profile removed."),
                    Err(e) => { eprintln!("failed to save config: {e}"); std::process::exit(1); }
                },
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            }
        }
        Some(Command::Profile(ProfileCommand::List)) => {
            let config = load_or_default_config(&path);
            print!("{}", profile_commands::format_profile_list(&config));
        }
        #[cfg(target_os = "linux")]
        Some(Command::Run) => linux_run::run(path),
        #[cfg(not(target_os = "linux"))]
        Some(Command::Run) => {
            eprintln!("`run` is only implemented on Linux in this increment (macOS backends are still stubs)");
            std::process::exit(1);
        }
    }
}

fn build_action_args(
    action: ActionArg,
    input: Option<String>,
    interval_ms: Option<u64>,
    jitter_ms: Option<u64>,
    step: Vec<String>,
    loop_: bool,
) -> ProfileActionArgs {
    match action {
        ActionArg::Repeat => ProfileActionArgs::Repeat {
            input: input.unwrap_or_else(|| { eprintln!("--input is required for --action repeat"); std::process::exit(1); }),
            interval_ms: interval_ms.unwrap_or_else(|| { eprintln!("--interval-ms is required for --action repeat"); std::process::exit(1); }),
            jitter_ms,
        },
        ActionArg::Macro => {
            if step.is_empty() {
                eprintln!("at least one --step is required for --action macro");
                std::process::exit(1);
            }
            ProfileActionArgs::Macro { steps: step, loop_ }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_window_picker {
    use orchestrator_window::kwin_dbus::KwinWindowLocator;
    use orchestrator_window::WindowLocator;

    pub fn pick_window() -> orchestrator_core::profile::Scope {
        super::runtime().block_on(async {
            let locator = match KwinWindowLocator::new().await {
                Ok(l) => l,
                Err(e) => { eprintln!("failed to construct window locator: {e}"); std::process::exit(1); }
            };
            let windows = match locator.list_windows().await {
                Ok(w) => w,
                Err(e) => { eprintln!("failed to list windows: {e}"); std::process::exit(1); }
            };
            if windows.is_empty() {
                eprintln!("no windows are currently open to choose from");
                std::process::exit(1);
            }
            println!("choose a window:");
            for (i, w) in windows.iter().enumerate() {
                println!("  [{i}] pid={:?} class={:?} title={:?}", w.pid, w.process_name, w.title);
            }
            print!("index: ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_err() {
                eprintln!("failed to read a selection from stdin");
                std::process::exit(1);
            }
            let index: usize = match line.trim().parse() {
                Ok(i) => i,
                Err(_) => { eprintln!("{line:?} is not a valid index"); std::process::exit(1); }
            };
            match super::profile_commands::resolve_window_scope(&windows, index) {
                Ok(scope) => scope,
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            }
        })
    }
}

/// Loads the config, spins up the real KDE/Wayland backends, and runs all
/// profiles until Ctrl-C (design spec §3.6).
#[cfg(target_os = "linux")]
mod linux_run {
    use orchestrator_hotkey::kde_portal_shortcuts::KdePortalHotkeyBackend;
    use orchestrator_input::linux_wayland::YdotoolInputInjector;
    use orchestrator_input::InputInjector;
    use orchestrator_window::kwin_dbus::KwinWindowLocator;

    /// Must match an installed `.desktop` file's id (see the module doc
    /// comment on `orchestrator_hotkey::kde_portal_shortcuts` and
    /// `packaging/linux/README.md`) or the portal rejects registration.
    const APP_ID: &str = "io.github.barebones-sh.Orchestrator";

    pub fn run(path: std::path::PathBuf) {
        tracing_subscriber::fmt::init();

        let config = match orchestrator_core::Config::load(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to load config at {}: {e}", path.display());
                std::process::exit(1);
            }
        };
        println!("loaded {} profile(s) from {}", config.profiles.len(), path.display());

        super::runtime().block_on(async move {
            let hotkey = match KdePortalHotkeyBackend::new(APP_ID).await {
                Ok(h) => h,
                Err(e) => { eprintln!("failed to construct hotkey backend: {e}"); std::process::exit(1); }
            };
            let window = match KwinWindowLocator::new().await {
                Ok(w) => w,
                Err(e) => { eprintln!("failed to construct window locator: {e}"); std::process::exit(1); }
            };
            let mut input = YdotoolInputInjector::new();
            if let Err(e) = input.connect().await {
                eprintln!("failed to connect input injector: {e}");
                std::process::exit(1);
            }

            let runner = orchestrator_core::runner::Runner::new(hotkey, input, window, config.profiles);
            println!("running. Ctrl-C to stop.");
            if let Err(e) = runner.run(async { let _ = tokio::signal::ctrl_c().await; }).await {
                eprintln!("runner exited with an error: {e}");
                std::process::exit(1);
            }
        });
    }
}
