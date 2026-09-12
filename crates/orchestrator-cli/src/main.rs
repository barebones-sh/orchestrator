use clap::{Parser, Subcommand};

mod notation;
mod profile_commands;

use profile_commands::{ProfileActionArgs, ProfileAddArgs, ProfileCommandError, ProfileEditArgs};

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
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
}

fn config_path(override_path: &Option<std::path::PathBuf>) -> std::path::PathBuf {
    override_path
        .clone()
        .unwrap_or_else(orchestrator_core::Config::config_path)
}

/// Distinguishes the one `Config::load` failure that's expected/recoverable
/// on a first run (no config file has ever been written yet) from every
/// other failure (parse error, validation failure, unsupported schema
/// version, or any other IO error). Only the former should be silently
/// papered over with an empty default config -- silently defaulting on any
/// other failure would mean the next `save` overwrites a real, possibly
/// salvageable file on disk with an empty one (see final-review Fix 1).
fn is_first_run_missing_file(err: &orchestrator_core::ConfigError) -> bool {
    matches!(
        err,
        orchestrator_core::ConfigError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound
    )
}

fn load_or_default_config(path: &std::path::Path) -> orchestrator_core::Config {
    match orchestrator_core::Config::load(path) {
        Ok(config) => config,
        Err(e) if is_first_run_missing_file(&e) => orchestrator_core::Config {
            schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION,
            profiles: vec![],
        },
        Err(e) => {
            eprintln!("failed to load config at {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod load_or_default_config_tests {
    use super::*;

    #[test]
    fn first_run_missing_file_is_recognized() {
        let path = std::path::Path::new("/definitely/does/not/exist/config.json");
        let err = orchestrator_core::Config::load(path).unwrap_err();
        assert!(
            is_first_run_missing_file(&err),
            "a missing config file must be treated as a first run, not a hard error"
        );
    }

    #[test]
    fn unparseable_file_is_not_first_run_missing_file() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "orchestrator-cli-test-unparseable-{}-{}.json",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, "not json").unwrap();
        let err = orchestrator_core::Config::load(&path).unwrap_err();
        std::fs::remove_file(&path).ok();

        assert!(
            !is_first_run_missing_file(&err),
            "a parse failure on an existing file must NOT be treated like a first run -- \
             defaulting to an empty config here would silently destroy the broken-but- \
             salvageable file on the next save"
        );
    }

    #[test]
    fn invalid_config_on_disk_is_not_first_run_missing_file() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "orchestrator-cli-test-invalid-{}-{}.json",
            std::process::id(),
            line!()
        ));
        // Written via `Config::save` (which does NOT validate -- only
        // `load` does) so this test doesn't depend on guessing the exact
        // on-disk JSON shape. schema_version is current and the file
        // parses fine, but interval_ms: 0 fails `Config::validate`, which
        // `Config::load` runs -- this must surface as a hard error, not be
        // treated as "no file yet".
        let invalid = orchestrator_core::Config {
            schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION,
            profiles: vec![orchestrator_core::profile::Profile {
                name: "bad".to_string(),
                trigger: orchestrator_hotkey::KeyCombo {
                    modifiers: vec![],
                    key: String::new(),
                },
                action: orchestrator_core::action::Action::Repeat {
                    input: orchestrator_input::InputEvent::KeyPress(
                        orchestrator_hotkey::KeyCombo {
                            modifiers: vec![],
                            key: "A".to_string(),
                        },
                    ),
                    interval_ms: 0,
                    jitter: orchestrator_core::action::Jitter::None,
                },
                scope: orchestrator_core::profile::Scope::Desktop,
                focus_steal: false,
                debounce_ms: 400,
            }],
        };
        invalid.save(&path).unwrap();

        let result = orchestrator_core::Config::load(&path);
        std::fs::remove_file(&path).ok();

        let err = result.expect_err("interval_ms: 0 must fail Config::load's validate() step");
        assert!(!is_first_run_missing_file(&err));
    }
}

fn main() {
    let cli = Cli::parse();
    let path = config_path(&cli.config);

    match cli.command {
        None => {}
        Some(Command::Profile(ProfileCommand::Add {
            name,
            scope,
            action,
            input,
            interval_ms,
            jitter_ms,
            step,
            r#loop,
            debounce_ms,
        })) => {
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
            let action_args =
                match build_action_args(action, input, interval_ms, jitter_ms, step, r#loop) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                };
            match profile_commands::add_profile(
                &mut config,
                ProfileAddArgs {
                    name,
                    scope: resolved_scope,
                    action: action_args,
                    debounce_ms,
                },
            ) {
                Ok(()) => {
                    // `add_profile` builds an `Action`/`Scope` without
                    // checking e.g. interval_ms > 0 or jitter <= interval --
                    // that's `Config::validate`'s job, and it must run
                    // BEFORE save so an invalid profile is never written to
                    // disk (see final-review Fix 1).
                    if let Err(e) = config.validate() {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                    match config.save(&path) {
                        Ok(()) => println!("profile added."),
                        Err(e) => {
                            eprintln!("failed to save config: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Command::Profile(ProfileCommand::Edit {
            name,
            scope,
            action,
            input,
            interval_ms,
            jitter_ms,
            step,
            r#loop,
            debounce_ms,
        })) => {
            // Action-detail flags only mean anything alongside `--action`:
            // without it, `edit` has no action variant to build them into,
            // so silently doing nothing with them (design spec §3.3: "any
            // flag given overwrites that field") would be a silent no-op
            // rather than a clear error. Checked first, before touching the
            // config or triggering any live window picker.
            if edit_needs_action_flag(
                action.is_some(),
                &input,
                &interval_ms,
                &jitter_ms,
                &step,
                r#loop,
            ) {
                eprintln!(
                    "--action is required whenever --input/--interval-ms/--jitter-ms/--step/\
                     --loop are given to `profile edit` -- these flags only apply together \
                     with --action (repeat's --input/--interval-ms/--jitter-ms, or macro's \
                     --step/--loop)"
                );
                std::process::exit(1);
            }

            let mut config = load_or_default_config(&path);
            let resolved_scope = scope.map(|s| match s {
                ScopeArg::Desktop => orchestrator_core::profile::Scope::Desktop,
                #[cfg(target_os = "linux")]
                ScopeArg::Window => linux_window_picker::pick_window(),
                #[cfg(not(target_os = "linux"))]
                ScopeArg::Window => {
                    eprintln!("--scope window's live picker is only implemented on Linux");
                    std::process::exit(1);
                }
            });
            let action_args = match action {
                Some(a) => {
                    match build_action_args(a, input, interval_ms, jitter_ms, step, r#loop) {
                        Ok(args) => Some(args),
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
                None => None,
            };
            match profile_commands::edit_profile(
                &mut config,
                &name,
                ProfileEditArgs {
                    scope: resolved_scope,
                    action: action_args,
                    debounce_ms,
                },
            ) {
                Ok(()) => {
                    if let Err(e) = config.validate() {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                    match config.save(&path) {
                        Ok(()) => println!("profile updated."),
                        Err(e) => {
                            eprintln!("failed to save config: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Command::Profile(ProfileCommand::Remove { name })) => {
            let mut config = load_or_default_config(&path);
            match profile_commands::remove_profile(&mut config, &name) {
                Ok(()) => {
                    // Removing a profile can't itself introduce a new
                    // validation failure, but validating before save here
                    // too is cheap defense-in-depth and keeps all three
                    // mutating handlers consistent.
                    if let Err(e) = config.validate() {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                    match config.save(&path) {
                        Ok(()) => println!("profile removed."),
                        Err(e) => {
                            eprintln!("failed to save config: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
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

/// Builds the notation-layer `ProfileActionArgs` from the CLI's raw flags,
/// validating that exactly the fields belonging to the chosen `--action`
/// were given: `--input`/`--interval-ms`/`--jitter-ms` are repeat-only,
/// `--step`/`--loop` are macro-only, and mixing the two groups (e.g.
/// `--action repeat --step ...` or `--action macro --jitter-ms ...`) is
/// rejected via `RepeatAndMacroBothOrNeitherSpecified` rather than silently
/// ignoring the wrong-action fields. Missing required fields for the chosen
/// action are reported via `MissingRequiredField`.
fn build_action_args(
    action: ActionArg,
    input: Option<String>,
    interval_ms: Option<u64>,
    jitter_ms: Option<u64>,
    step: Vec<String>,
    loop_: bool,
) -> Result<ProfileActionArgs, ProfileCommandError> {
    let repeat_fields_given = input.is_some() || interval_ms.is_some() || jitter_ms.is_some();
    let macro_fields_given = !step.is_empty() || loop_;

    match action {
        ActionArg::Repeat => {
            if macro_fields_given {
                return Err(ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified);
            }
            Ok(ProfileActionArgs::Repeat {
                input: input.ok_or(ProfileCommandError::MissingRequiredField("input"))?,
                interval_ms: interval_ms
                    .ok_or(ProfileCommandError::MissingRequiredField("interval-ms"))?,
                jitter_ms,
            })
        }
        ActionArg::Macro => {
            if repeat_fields_given {
                return Err(ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified);
            }
            if step.is_empty() {
                return Err(ProfileCommandError::MissingRequiredField("step"));
            }
            Ok(ProfileActionArgs::Macro { steps: step, loop_ })
        }
    }
}

/// `profile edit`'s action-detail flags (`--input`/`--interval-ms`/
/// `--jitter-ms`/`--step`/`--loop`) only apply together with `--action`
/// (design spec §3.3: "any flag given overwrites that field" -- but there's
/// no field to overwrite for these without knowing which action variant is
/// meant). Returns whether the edit should be rejected for giving one of
/// these flags without `--action`.
fn edit_needs_action_flag(
    action_given: bool,
    input: &Option<String>,
    interval_ms: &Option<u64>,
    jitter_ms: &Option<u64>,
    step: &[String],
    loop_: bool,
) -> bool {
    !action_given
        && (input.is_some()
            || interval_ms.is_some()
            || jitter_ms.is_some()
            || !step.is_empty()
            || loop_)
}

#[cfg(test)]
mod edit_needs_action_flag_tests {
    use super::*;

    #[test]
    fn no_action_and_no_detail_flags_is_fine() {
        assert!(!edit_needs_action_flag(
            false,
            &None,
            &None,
            &None,
            &[],
            false
        ));
    }

    #[test]
    fn action_given_is_always_fine_regardless_of_detail_flags() {
        assert!(!edit_needs_action_flag(
            true,
            &Some("key:A".to_string()),
            &Some(100),
            &Some(10),
            &["key:A:10".to_string()],
            true
        ));
    }

    #[test]
    fn interval_ms_without_action_is_rejected() {
        assert!(edit_needs_action_flag(
            false,
            &None,
            &Some(999),
            &None,
            &[],
            false
        ));
    }

    #[test]
    fn input_without_action_is_rejected() {
        assert!(edit_needs_action_flag(
            false,
            &Some("key:A".to_string()),
            &None,
            &None,
            &[],
            false
        ));
    }

    #[test]
    fn jitter_ms_without_action_is_rejected() {
        assert!(edit_needs_action_flag(
            false,
            &None,
            &None,
            &Some(10),
            &[],
            false
        ));
    }

    #[test]
    fn step_without_action_is_rejected() {
        assert!(edit_needs_action_flag(
            false,
            &None,
            &None,
            &None,
            &["key:A:10".to_string()],
            false
        ));
    }

    #[test]
    fn loop_without_action_is_rejected() {
        assert!(edit_needs_action_flag(
            false,
            &None,
            &None,
            &None,
            &[],
            true
        ));
    }
}

#[cfg(test)]
mod build_action_args_tests {
    use super::*;

    #[test]
    fn repeat_with_required_fields_is_ok() {
        let result = build_action_args(
            ActionArg::Repeat,
            Some("key:A".to_string()),
            Some(100),
            None,
            vec![],
            false,
        );
        assert!(matches!(result, Ok(ProfileActionArgs::Repeat { .. })));
    }

    #[test]
    fn repeat_missing_input_errors() {
        let result = build_action_args(ActionArg::Repeat, None, Some(100), None, vec![], false);
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::MissingRequiredField("input")
        );
    }

    #[test]
    fn repeat_missing_interval_ms_errors() {
        let result = build_action_args(
            ActionArg::Repeat,
            Some("key:A".to_string()),
            None,
            None,
            vec![],
            false,
        );
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::MissingRequiredField("interval-ms")
        );
    }

    #[test]
    fn macro_with_steps_is_ok() {
        let result = build_action_args(
            ActionArg::Macro,
            None,
            None,
            None,
            vec!["key:A:10".to_string()],
            true,
        );
        assert!(matches!(result, Ok(ProfileActionArgs::Macro { .. })));
    }

    #[test]
    fn macro_missing_steps_errors() {
        let result = build_action_args(ActionArg::Macro, None, None, None, vec![], false);
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::MissingRequiredField("step")
        );
    }

    #[test]
    fn repeat_with_macro_step_conflicts() {
        let result = build_action_args(
            ActionArg::Repeat,
            Some("key:A".to_string()),
            Some(100),
            None,
            vec!["key:A:10".to_string()],
            false,
        );
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified
        );
    }

    #[test]
    fn repeat_with_loop_flag_conflicts() {
        let result = build_action_args(
            ActionArg::Repeat,
            Some("key:A".to_string()),
            Some(100),
            None,
            vec![],
            true,
        );
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified
        );
    }

    #[test]
    fn macro_with_repeat_fields_conflicts() {
        let result = build_action_args(
            ActionArg::Macro,
            Some("key:A".to_string()),
            None,
            None,
            vec!["key:A:10".to_string()],
            false,
        );
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified
        );
    }

    #[test]
    fn macro_with_jitter_ms_conflicts() {
        // final-review Fix 2: --jitter-ms was missing from the repeat-only
        // field check, so `--action macro --step ... --jitter-ms 10` was
        // silently accepted (jitter_ms just got dropped) instead of being
        // rejected the same way `--input`/`--interval-ms` already are.
        let result = build_action_args(
            ActionArg::Macro,
            None,
            None,
            Some(10),
            vec!["key:A:10".to_string()],
            false,
        );
        assert_eq!(
            result.unwrap_err(),
            ProfileCommandError::RepeatAndMacroBothOrNeitherSpecified
        );
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
                Err(e) => {
                    eprintln!("failed to construct window locator: {e}");
                    std::process::exit(1);
                }
            };
            let windows = match locator.list_windows().await {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("failed to list windows: {e}");
                    std::process::exit(1);
                }
            };
            if windows.is_empty() {
                eprintln!("no windows are currently open to choose from");
                std::process::exit(1);
            }
            println!("choose a window:");
            for (i, w) in windows.iter().enumerate() {
                println!(
                    "  [{i}] pid={:?} class={:?} title={:?}",
                    w.pid, w.process_name, w.title
                );
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
                Err(_) => {
                    eprintln!("{line:?} is not a valid index");
                    std::process::exit(1);
                }
            };
            match super::profile_commands::resolve_window_scope(&windows, index) {
                Ok(scope) => scope,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
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
    const APP_ID: &str = "io.github.barebonessh.Orchestrator";

    pub fn run(path: std::path::PathBuf) {
        tracing_subscriber::fmt::init();

        let config = match orchestrator_core::Config::load(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to load config at {}: {e}", path.display());
                std::process::exit(1);
            }
        };
        println!(
            "loaded {} profile(s) from {}",
            config.profiles.len(),
            path.display()
        );

        super::runtime().block_on(async move {
            let hotkey = match KdePortalHotkeyBackend::new(APP_ID).await {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("failed to construct hotkey backend: {e}");
                    std::process::exit(1);
                }
            };
            let window = match KwinWindowLocator::new().await {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("failed to construct window locator: {e}");
                    std::process::exit(1);
                }
            };
            let mut input = YdotoolInputInjector::new();
            if let Err(e) = input.connect().await {
                eprintln!("failed to connect input injector: {e}");
                std::process::exit(1);
            }

            let runner =
                orchestrator_core::runner::Runner::new(hotkey, input, window, config.profiles);
            println!("running. Ctrl-C to stop.");
            if let Err(e) = runner
                .run(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
            {
                eprintln!("runner exited with an error: {e}");
                std::process::exit(1);
            }
        });
    }
}
