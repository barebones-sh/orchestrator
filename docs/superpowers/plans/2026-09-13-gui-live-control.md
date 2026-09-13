# GUI: Live Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the GUI actually run Orchestrator's automation live — start/stop the `Runner` from the app, with a system tray icon so closing the window doesn't kill a running session.

**Architecture:** A `RunnerHandle`/`RunnerState` pair held in Tauri managed state tracks whether the embedded `Runner` is idle, running, or errored. `start_runner`/`stop_runner`/`runner_status` commands and four static tray-menu items (Start/Stop/Show window/Quit) all call the same two plain Rust functions (`do_start_runner`/`do_stop_runner`) — no duplicated logic between the two entry points. The main window's close button hides it instead of quitting; `Quit` in the tray is the only real exit.

**Tech Stack:** No new dependencies beyond enabling `orchestrator-core`'s existing `runner` Cargo feature and `tauri`'s `tray-icon` feature — both already-published, no version research needed.

**Spec:** `docs/superpowers/specs/2026-09-13-gui-live-control-design.md`

## Global Constraints

- Linux only: `start_runner`/`stop_runner`'s real implementation is Linux-only (constructs `KdePortalHotkeyBackend`/`YdotoolInputInjector`/`KwinWindowLocator`, exactly as `orchestrator-cli`'s `linux_run` does); non-Linux gets a stub returning a clear "only implemented on Linux" error, matching the existing `list_windows` split in `crates/orchestrator-gui/src-tauri/src/lib.rs`.
- The GUI's runner must use `APP_ID = "io.github.barebonessh.Orchestrator"` — copied verbatim from `orchestrator-cli/src/main.rs`'s constant, **not** a GUI-specific id (see spec §3 for why: the `.deb` ships one `.desktop` file, and sharing the id means CLI- and GUI-registered profiles share the same KDE keybinds).
- No change to `Profile.trigger`'s meaning or any attempt to surface the real assigned key combo anywhere in the UI (spec §2, §8) — out of scope, no data source exists for it.
- No auto-start-on-launch, no persisted "was running" preference — the runner always starts `Idle` (spec §8).
- Do not touch app icon design, `.desktop` entry, or `.deb` packaging for the GUI (spec §8, same standing deferral as the profile-management increment).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` must both stay clean — run them for real before reporting a task done.
- **Hard safety constraint, carried over from the prior GUI increment: no agent may launch the app, take a screenshot, or use any input-injection tool (xdotool or similar) at any point in this plan.** This sandbox's desktop is the human partner's real, actively-used desktop; an earlier agent's synthetic click landed in the user's own Discord window. Verification is code-reading plus non-interactive commands (`cargo build`/`test`/`clippy`/`fmt`, `npm run check`/`build`) only. Each task ends with a manual-test-script addition for the human to run by hand — do not attempt to substitute agent-driven interactive testing for it under any circumstances.
- Work directly on `main` (no worktree), matching this project's standing practice all session. Do not push to `origin` as part of executing this plan.

---

## Task 1: Runner lifecycle in the Tauri backend

**Files:**
- Modify: `crates/orchestrator-gui/src-tauri/Cargo.toml`
- Modify: `crates/orchestrator-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `RunnerHandle`, `RunnerState`, `RunnerStatusDto`, `do_start_runner(app: tauri::AppHandle) -> Result<(), String>`, `do_stop_runner(app: tauri::AppHandle) -> Result<(), String>`, `status_dto(&RunnerState) -> RunnerStatusDto`, `emit_status(&tauri::AppHandle, &RunnerState)`, plus the three `start_runner`/`stop_runner`/`runner_status` Tauri commands — Task 2 (tray) calls `do_start_runner`/`do_stop_runner`/reads `RunnerHandle` directly; Task 3 (frontend) calls the three commands and listens for the `"runner-status-changed"` event these emit.
- Consumes: nothing from other tasks in this plan.

- [ ] **Step 1: Enable the `runner` and `tray-icon` Cargo features**

In `crates/orchestrator-gui/src-tauri/Cargo.toml`:
1. Add `tray-icon` to the `tauri` dependency's features: `tauri = { version = "2", features = ["tray-icon"] }`.
2. Add `"runner"` to the Linux target block's `orchestrator-core` features:
   ```toml
   orchestrator-core = { path = "../../orchestrator-core", features = ["kde", "wayland", "runner"] }
   ```
   Leave the macOS block unchanged (this whole plan is Linux-only per the Global Constraints; macOS gets the stub path, which needs no extra core features).

- [ ] **Step 2: Add the state types and status helpers**

Add to `crates/orchestrator-gui/src-tauri/src/lib.rs`, above the existing `list_profiles` command (grouped with the other shared infrastructure like `load_or_default_config`):

```rust
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
```

`RunnerStatusDto`'s `state` field is a plain string (`"idle"`/`"running"`/`"error"`) rather than a tagged enum, matching this project's existing convention of keeping Tauri command/event payloads as simple, directly-`serde`-serializable shapes (see how `list_profiles` already just returns `Vec<Profile>` directly rather than wrapping it).

- [ ] **Step 3: Add `do_start_runner`/`do_stop_runner` (Linux)**

```rust
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
```

Check `orchestrator_hotkey::kde_portal_shortcuts::KdePortalHotkeyBackend`, `orchestrator_input::linux_wayland::YdotoolInputInjector`, and `orchestrator_window::kwin_dbus::KwinWindowLocator`'s exact module paths against `orchestrator-cli/src/main.rs`'s `linux_run` module (which constructs all three already) before writing the `use` statements — copy the paths from there rather than guessing.

- [ ] **Step 4: Add `do_start_runner`/`do_stop_runner` (non-Linux stub)**

```rust
#[cfg(not(target_os = "linux"))]
fn do_start_runner(_app: tauri::AppHandle) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}

#[cfg(not(target_os = "linux"))]
fn do_stop_runner(_app: tauri::AppHandle) -> Result<(), String> {
    Err("live control is only implemented on Linux in this increment".to_string())
}
```

- [ ] **Step 5: Add the three Tauri commands**

```rust
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
```

- [ ] **Step 6: Register the managed state and the new commands**

In `run()`, add `.manage(...)` before `.invoke_handler(...)`, and add the three new commands to `generate_handler!`:

```rust
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

(Task 2 adds a `.setup(...)` closure here too — leave room for it, don't restructure this function beyond what's shown.)

- [ ] **Step 7: Verify**

```bash
cargo build -p orchestrator-gui --verbose
cargo test --workspace --verbose
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Also cross-compile-check the non-Linux path compiles (this project has done this before for CLI code — same idea here):
```bash
cargo check -p orchestrator-gui --target x86_64-apple-darwin
```
(Install the target first if needed: `rustup target add x86_64-apple-darwin`.)

Do not launch the app. Confirm correctness by reading the code and by these non-interactive commands only.

- [ ] **Step 8: Commit**

```bash
git add crates/orchestrator-gui/src-tauri/Cargo.toml crates/orchestrator-gui/src-tauri/src/lib.rs
git commit -m "feat: add Runner lifecycle (start/stop/status) to the Tauri backend"
```

---

## Task 2: System tray and window-close-to-hide

**Files:**
- Modify: `crates/orchestrator-gui/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `RunnerHandle`, `RunnerState`, `do_start_runner`, `do_stop_runner` (Task 1) — the tray's menu-event handler calls the same two functions the commands do, no duplicated start/stop logic.
- Produces: nothing further tasks depend on (Task 3 is frontend-only).

Tray menu is **four static items** (Start, Stop, Show window, Quit) rather than one dynamically-relabeled toggle — clicking Start while already running (or Stop while already idle) just gets the same `Err` the equivalent command call would get, silently ignored by the tray handler. This is a deliberate simplification over the design spec's "single toggling item" framing (§4): a dynamically-relabeled tray menu item requires storing the built `TrayIcon` in managed state and rebuilding/re-setting its menu on every status change, for a v1 UX benefit (avoiding two always-visible, sometimes-no-op menu items) that isn't worth the added state-plumbing. Four static items works correctly and is much simpler to get right.

- [ ] **Step 1: Verify Tauri v2's current tray/menu API before writing code**

Check `tauri::tray::TrayIconBuilder`, `tauri::menu::{Menu, MenuItem}`, `MenuItem::with_id`, `Menu::with_items`, `.on_menu_event(...)`, and `WebviewWindow::on_window_event` / `tauri::WindowEvent::CloseRequested`'s exact current signatures against Tauri v2's own docs (docs.rs for the `tauri` crate at the version pinned in `Cargo.toml`, or v2.tauri.app) before writing the step below — the shapes given here were confirmed against Tauri v2's docs at plan-writing time, but confirm they still match the exact pinned version rather than assuming.

- [ ] **Step 2: Add the tray and window-close setup**

Add a `.setup(...)` closure to the `tauri::Builder` chain in `run()` (between `.manage(...)` and `.invoke_handler(...)`):

```rust
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
```

`app.default_window_icon()` reuses the icon Task 1 (the profile-management plan's Task 1) already generated via `cargo tauri icon` and wired into `tauri.conf.json` — confirm it's actually set there before relying on `.unwrap()` (if `tauri.conf.json` has no `bundle.icon` configured, this panics at startup; check first, and if unset, reference one of the generated `src-tauri/icons/*.png` files directly instead via `tauri::image::Image::from_path(...)` — verify the exact current API for loading an icon from a file path if this path is needed).

- [ ] **Step 3: Verify**

```bash
cargo build -p orchestrator-gui --verbose
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Do not launch the app.

- [ ] **Step 4: Commit**

```bash
git add crates/orchestrator-gui/src-tauri/src/lib.rs
git commit -m "feat: add system tray and window-close-to-hide behavior"
```

---

## Task 3: Frontend status panel, keybind note, and manual test script

**Files:**
- Modify: `crates/orchestrator-gui/src/routes/+page.svelte` (or a new `crates/orchestrator-gui/src/lib/RunnerStatus.svelte` component if the implementer judges it substantial enough — follow `ProfileList.svelte`/`ProfileForm.svelte`'s existing precedent for that call)
- Modify: `crates/orchestrator-gui/TESTING.md`

**Interfaces:**
- Consumes: `start_runner`/`stop_runner`/`runner_status` commands and the `"runner-status-changed"` event (Task 1).

- [ ] **Step 1: Add the status panel**

Somewhere visible in the main page layout (above or alongside the profile list — implementer's call on exact placement, but it should be visible without scrolling past the profile list first, since it's the primary control for the whole app now): a status display (Idle / Running / Error, with the error message shown if present) and a Start/Stop button that calls the corresponding command and reflects the current state (disabled or hidden appropriately — e.g. show "Start" when idle/error, "Stop" when running).

On mount, call `invoke("runner_status")` for the initial state. Use `@tauri-apps/api/event`'s `listen("runner-status-changed", (event) => { ... })` to update reactively afterward — check this project's existing Svelte 5 runes state patterns (`ProfileList.svelte`'s `$state`) for how to wire an external event into reactive state; do not start a manual `setInterval` polling loop, the event is the update mechanism.

Handle `invoke("start_runner")`/`invoke("stop_runner")`'s rejections (e.g. `"already running"`/`"not running"`) the same way `ProfileForm.svelte` already handles command rejections — display the message, don't let it go unhandled.

- [ ] **Step 2: Add the keybind-assignment note**

Per the design spec §6, add this exact text near the Start button, always visible (not conditional on any state — there's no data to condition it on, see spec §2):

> The first time you click Start, KDE may ask you to press a key combination for each profile that doesn't have one assigned yet — this happens once per profile. Change an assigned shortcut anytime in KDE's System Settings → Shortcuts → Orchestrator.

- [ ] **Step 3: Verify the gates**

```bash
npm run check
npm run build
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Do not run `npm run tauri dev`, do not launch the app, do not take screenshots.

- [ ] **Step 4: Extend `crates/orchestrator-gui/TESTING.md`**

Add a new section to the existing manual test script (don't replace the existing profile-management steps) covering live control:

```markdown
## Live control

1. Launch the app (`npm run tauri dev` from `crates/orchestrator-gui`, or the built app).
2. Confirm the status panel shows "Idle" on first load.
3. If you have no profiles yet, add one first (see the steps above) — e.g. a
   Desktop-scope Repeat profile.
4. Click **Start**. Expected: status changes to "Running". If this is the
   first time this profile's name has ever been registered, KDE will show
   its own native dialog asking you to press a key combination — press one
   (e.g. Ctrl+Alt+F9, avoiding any combo already bound to something else on
   your system) to complete the assignment.
5. Press the key combination you just assigned. Expected: the configured
   action actually fires (e.g. the key you configured the profile to repeat
   gets pressed repeatedly, or the scroll happens) — confirms the whole
   chain (portal registration -> hotkey event -> input injection) is really
   live, not just that the button changed color.
6. Click **Stop**. Expected: status returns to "Idle", and the hotkey you
   pressed in step 5 no longer does anything.
7. Click **Start** again for the *same* profile. Expected: status returns
   to "Running" without KDE re-prompting for a key combo (it remembers the
   assignment from step 4) — the action fires again on the same key press.
8. Close the main window (the window-close button, not Quit). Expected:
   the window disappears but the app keeps running — check the system
   tray, the icon should still be there. Press the hotkey from step 5
   again: it should still fire, confirming closing the window didn't stop
   a running session.
9. From the tray icon's menu, click **Show window**. Expected: the window
   reappears, status panel still shows "Running".
10. From the tray icon's menu, click **Stop**, then **Start**. Expected:
    matches steps 6-7's behavior, but driven from the tray instead of the
    window.
11. From the tray icon's menu, click **Quit**. Expected: the app fully
    exits (tray icon disappears, no process left running) — check with
    `pgrep -f orchestrator-gui` or similar; nothing should remain.
12. Clean up: remove any test profile you added in step 3, the same way
    the profile-management steps above already show.
```

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator-gui/src crates/orchestrator-gui/TESTING.md
git commit -m "feat: add live-control status panel, keybind note, and manual test steps"
```

---

## Plan-Level Verification

- `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` all pass after all 3 tasks.
- `npm run check`/`npm run build` (in `crates/orchestrator-gui/`) both pass.
- No agent at any point in this plan launched the app, took a screenshot, or used an input-injection tool (Global Constraints) — verify by checking each task's report doesn't claim otherwise.
- `crates/orchestrator-gui/TESTING.md`'s new "Live control" section is genuinely runnable and complete — the human partner runs it after this plan closes, same as the profile-management increment's own script.
- Design-intent check: `APP_ID` matches the CLI's exactly (spec's load-bearing requirement); no per-profile keybind-status UI was added (spec §2's correction); no auto-start-on-launch or persisted preference exists (spec §8).
