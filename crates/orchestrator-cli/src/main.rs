use clap::{Parser, Subcommand};

mod notation;

/// Orchestrator: global-hotkey input automation (profile commands not yet implemented).
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Hidden diagnostic subcommands added for Task 9's human-in-the-loop
/// verification pass against a live KDE/Wayland session. These are NOT
/// user-facing product surface -- `profile add/edit/remove/list` remains
/// future work per the first increment's non-goals (see
/// `.superpowers/sdd/meant-for-you-to-modular-spring/task-9-brief.md`). Each
/// variant is hidden from `--help` accordingly and only exists to let a
/// human exercise the real Linux backends end-to-end once, by hand.
// The shared `Internal*` prefix is deliberate, not an oversight: it signals
// at every call site (and in `--help` source, even though these are hidden)
// that these are Task 9 diagnostic-only variants, not real product
// subcommands -- see the doc comment above.
#[allow(clippy::enum_variant_names)]
#[derive(Subcommand)]
enum Command {
    /// Construct the real KDE portal hotkey backend (`KdePortalHotkeyBackend`),
    /// register one action, print the resolved key combo, then print every
    /// press/release event received on `subscribe()` until Ctrl-C.
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    InternalHotkeySmokeTest {
        /// App id passed to `KdePortalHotkeyBackend::new`. Must match an
        /// installed `.desktop` file's basename, or the portal rejects
        /// registration -- see `orchestrator-hotkey`'s
        /// `kde_portal_shortcuts` module doc comment.
        app_id: String,
    },

    /// Construct the real KWin D-Bus window locator (`KwinWindowLocator`),
    /// list all windows, print the currently focused window, and (if a
    /// handle is given) call `activate_window` on it. Run once with no
    /// handle to see the list of available handles, then run again with one
    /// copied from that output to actually exercise activation -- see
    /// `activate_window`'s role in the "activate target -> inject -> restore
    /// prior focus" sequence documented in
    /// `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`.
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    InternalWindowSmokeTest {
        /// A `WindowHandle` string (e.g. copied from a prior no-argument
        /// run's window list) to pass to `activate_window`. Omit to only
        /// list windows and print the focused one.
        handle: Option<String>,
    },

    /// Construct the real ydotool input injector (`YdotoolInputInjector`),
    /// connect to `ydotoold`, and inject one keypress (press then release)
    /// of the given portable key name (default "A").
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    InternalInputSmokeTest {
        #[arg(default_value = "A")]
        key: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None => {}
        #[cfg(target_os = "linux")]
        Some(Command::InternalHotkeySmokeTest { app_id }) => linux_smoke::hotkey_smoke_test(app_id),
        #[cfg(target_os = "linux")]
        Some(Command::InternalWindowSmokeTest { handle }) => linux_smoke::window_smoke_test(handle),
        #[cfg(target_os = "linux")]
        Some(Command::InternalInputSmokeTest { key }) => linux_smoke::input_smoke_test(key),
    }
}

/// Diagnostic-only smoke tests for Task 9's human-in-the-loop verification
/// pass. Not user-facing product surface -- see [`Command`]'s doc comment.
/// Each function builds its own single-threaded-friendly Tokio runtime
/// on-demand rather than wrapping all of `main` in `#[tokio::main]`, since
/// the ordinary bare-invocation path (no subcommand) has no async work at
/// all.
#[cfg(target_os = "linux")]
mod linux_smoke {
    use orchestrator_hotkey::kde_portal_shortcuts::KdePortalHotkeyBackend;
    use orchestrator_hotkey::{HotkeyBackend, KeyCombo};
    use orchestrator_input::linux_wayland::YdotoolInputInjector;
    use orchestrator_input::{InputEvent, InputInjector};
    use orchestrator_window::kwin_dbus::KwinWindowLocator;
    use orchestrator_window::{WindowHandle, WindowLocator};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    }

    pub fn hotkey_smoke_test(app_id: String) {
        runtime().block_on(async move {
            eprintln!(
                "internal-hotkey-smoke-test: constructing KdePortalHotkeyBackend(app_id={app_id:?})"
            );
            let backend = match KdePortalHotkeyBackend::new(app_id).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("failed to construct backend: {e}");
                    std::process::exit(1);
                }
            };

            let (id, combo) = match backend
                .register("orchestrator-smoke-test-action", None)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("register failed: {e}");
                    std::process::exit(1);
                }
            };
            println!("registered: id={id:?} resolved combo={combo:?}");
            println!("press the assigned combo to see events below; Ctrl-C to exit.");

            let rx = backend.subscribe();
            // subscribe() hands back a blocking std::sync::mpsc::Receiver, so
            // it's drained on a blocking-pool thread rather than the async
            // runtime, and simply left to be torn down with the process on
            // Ctrl-C (a bare JoinHandle::abort() on a spawn_blocking task
            // does not interrupt an in-progress blocking recv(), but that's
            // fine here since the whole process exits right after).
            let _events = tokio::task::spawn_blocking(move || {
                while let Ok((id, event)) = rx.recv() {
                    println!("event: id={id:?} event={event:?}");
                }
            });

            let _ = tokio::signal::ctrl_c().await;
            println!("Ctrl-C received; unregistering and exiting.");
            let _ = backend.unregister(id).await;
        });
    }

    pub fn window_smoke_test(handle: Option<String>) {
        runtime().block_on(async {
            eprintln!("internal-window-smoke-test: constructing KwinWindowLocator");
            let locator = match KwinWindowLocator::new().await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("failed to construct locator: {e}");
                    std::process::exit(1);
                }
            };

            match locator.list_windows().await {
                Ok(windows) => {
                    println!("windows ({}):", windows.len());
                    for w in &windows {
                        println!("  {w:?}");
                    }
                }
                Err(e) => eprintln!("list_windows failed: {e}"),
            }

            match locator.focused_window().await {
                Ok(handle) => println!("focused window: {handle:?}"),
                Err(e) => eprintln!("focused_window failed: {e}"),
            }

            // Only exercised when a handle is given (e.g. copied from the
            // window list printed above on a prior run) -- see this
            // variant's doc comment. `activate_window` is otherwise
            // untouched by this smoke test, even though it's the single
            // most load-bearing mechanic the whole Scope::Window feature
            // rests on (activate target -> inject -> restore prior focus).
            if let Some(handle) = handle {
                println!("activating window with handle {handle:?}...");
                match locator.activate_window(&WindowHandle(handle)).await {
                    Ok(()) => println!("activate_window: ok"),
                    Err(e) => eprintln!("activate_window failed: {e}"),
                }
            } else {
                println!(
                    "no handle given; skipping activate_window. Re-run with a handle copied \
                     from the window list above to exercise it."
                );
            }
        });
    }

    pub fn input_smoke_test(key: String) {
        runtime().block_on(async move {
            eprintln!("internal-input-smoke-test: constructing YdotoolInputInjector (key={key:?})");
            let mut injector = YdotoolInputInjector::new();
            if let Err(e) = injector.connect().await {
                eprintln!("connect failed: {e}");
                std::process::exit(1);
            }

            let combo = KeyCombo {
                modifiers: Vec::new(),
                key: key.clone(),
            };
            if let Err(e) = injector.inject(&InputEvent::KeyPress(combo.clone())).await {
                eprintln!("inject KeyPress failed: {e}");
                std::process::exit(1);
            }
            if let Err(e) = injector.inject(&InputEvent::KeyRelease(combo)).await {
                eprintln!("inject KeyRelease failed: {e}");
                std::process::exit(1);
            }
            println!("injected key press+release for {key:?}");
        });
    }
}
