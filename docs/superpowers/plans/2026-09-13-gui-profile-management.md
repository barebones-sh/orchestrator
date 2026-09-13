# GUI: Profile Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `orchestrator-gui` from a placeholder binary into a real Tauri v2 app that lists, adds, edits, and removes profiles, reading/writing the same `config.json` the CLI already uses.

**Architecture:** `crates/orchestrator-gui/` restructures into the standard Tauri v2 shape (Svelte+Vite frontend at the crate root, the actual Rust crate nested at `src-tauri/`). The CLI's `profile_commands.rs` mutation logic (notation-agnostic parts) moves into a new `orchestrator_core::profile_ops` module so both CLI and GUI call the identical, already-tested logic — the CLI still owns notation-string parsing, the GUI's Tauri commands call `profile_ops` directly with typed values from the frontend.

**Tech Stack:** Tauri 2.11.5, Svelte + Vite (exact current major/scaffolding syntax verified at implementation time — see Task 1), `orchestrator-core`/`-hotkey`/`-window` (already-existing workspace crates).

**Spec:** `docs/superpowers/specs/2026-09-13-gui-design.md`

## Global Constraints

- This plan covers profile management only. Do **not** implement live start/stop of the `Runner`, a system tray icon, an app icon design, a visible `.desktop` entry, or `.deb` packaging for the GUI — all explicitly out of scope per the spec's §2/§9, left for a future increment.
- The human partner must install Tauri's Linux build prerequisites before Task 1 can be attempted for real (spec §3): `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`. Task 1's first step verifies this.
- Task 2 (the `profile_ops` refactor) must not require any change to `orchestrator-cli/src/main.rs` — if it does, the refactor's boundary is wrong (see Task 2 for why this is achievable: `ProfileAddArgs`/`ProfileEditArgs`/`add_profile`/`edit_profile`/`remove_profile`/`resolve_window_scope`'s names and signatures in `orchestrator-cli::profile_commands` stay exactly as `main.rs` already calls them).
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` must both stay clean. Run them for real before reporting a task done — this project has hit "reported clean without running" failures twice already this session.
- Do not push to `origin` as part of executing this plan — commit locally only.
- Work directly on the `main` branch (no worktree), matching this project's standing practice all session.

---

## Task 1: Tauri + Svelte scaffold

**Files:**
- Modify: root `Cargo.toml` (add `crates/orchestrator-gui/src-tauri` to `members`)
- Delete: `crates/orchestrator-gui/Cargo.toml`, `crates/orchestrator-gui/src/main.rs` (the old placeholder)
- Create: `crates/orchestrator-gui/package.json`, `crates/orchestrator-gui/vite.config.js` (or whatever the scaffolding tool names its Vite config), `crates/orchestrator-gui/index.html`, `crates/orchestrator-gui/src/` (frontend source, exact files determined by the scaffolding tool)
- Create: `crates/orchestrator-gui/src-tauri/Cargo.toml`, `crates/orchestrator-gui/src-tauri/tauri.conf.json`, `crates/orchestrator-gui/src-tauri/build.rs`, `crates/orchestrator-gui/src-tauri/src/main.rs`, `crates/orchestrator-gui/src-tauri/icons/` (generated)

**Interfaces:** None — this is a scaffolding task with no code-level interface to other tasks beyond "a working Tauri+Svelte project exists at `crates/orchestrator-gui/` with the workspace correctly wired to it."

This task is deliberately spike-shaped: prove the toolchain actually produces a running app in this environment before Task 2 touches any shared logic. Two things here are outside this plan's ability to pin exactly, since they depend on tool versions newer than what's certain at plan-writing time — research them yourself before proceeding, the way this project's CI plan researched `dtolnay/rust-toolchain`'s current inputs and the `.deb` plan researched `cargo-deb`'s `license-file` shape:

1. The exact current non-interactive invocation of the Tauri+Svelte scaffolding tool (`npm create tauri-app@latest -- --help` or equivalent — check what flags exist for choosing the Svelte template and a project name/directory non-interactively).
2. Whether the scaffolded frontend uses Svelte 5 (runes: `$state`, `$props`, etc.) or Svelte 4 (`export let`, reactive `$:` statements) — Task 4 will need to match whichever this task actually produces.

- [ ] **Step 1: Verify the prerequisite is installed**

```bash
pkg-config --exists webkit2gtk-4.1 && echo "OK" || echo "MISSING"
```

If `MISSING`: **stop and report BLOCKED** — this requires the human partner to run the install command from this plan's Global Constraints themselves; do not attempt to install it yourself (no `sudo` access in this environment is expected, and even if you have it, this is the human's machine-level dependency to approve).

- [ ] **Step 2: Scaffold into a scratch directory, then move into place**

The target directory (`crates/orchestrator-gui/`) already contains an old placeholder `Cargo.toml`/`src/main.rs` — most scaffolding tools refuse to write into a non-empty directory. Scaffold into a fresh temp directory (e.g. `/tmp/orchestrator-gui-scaffold`), choosing the Svelte frontend template (plain JavaScript, not TypeScript — this project has no TypeScript elsewhere and YAGNI applies for a form-and-list app this size) and Rust as the backend language, non-interactively per your Step-0 research. Then:

1. Delete the old `crates/orchestrator-gui/Cargo.toml` and `crates/orchestrator-gui/src/`.
2. Move the scaffolded project's contents into `crates/orchestrator-gui/` (frontend files at the crate root, `src-tauri/` nested inside, exactly as the spec's §4 layout shows).
3. Delete the scratch directory.

- [ ] **Step 3: Merge the pre-existing OS-conditional dependencies into the new `src-tauri/Cargo.toml`**

The deleted `crates/orchestrator-gui/Cargo.toml` had these `[target.'cfg(...)'.dependencies]` blocks (needed so the real KDE/Wayland backends are reachable once GUI code uses them — same backend-selection pattern `orchestrator-cli`'s `Cargo.toml` uses):

```toml
[target.'cfg(target_os = "linux")'.dependencies]
orchestrator-core = { path = "../../orchestrator-core", features = ["kde", "wayland"] }
orchestrator-hotkey = { path = "../../orchestrator-hotkey", features = ["kde"] }
orchestrator-window = { path = "../../orchestrator-window", features = ["kde"] }
orchestrator-input = { path = "../../orchestrator-input", features = ["wayland"] }

[target.'cfg(target_os = "macos")'.dependencies]
orchestrator-core = { path = "../../orchestrator-core", features = ["macos"] }
orchestrator-hotkey = { path = "../../orchestrator-hotkey", features = ["macos"] }
orchestrator-window = { path = "../../orchestrator-window", features = ["macos"] }
orchestrator-input = { path = "../../orchestrator-input", features = ["macos"] }
```

Note the relative paths change from `../orchestrator-core` to `../../orchestrator-core` — `src-tauri/` is now one directory deeper than the old crate root was. Add these blocks to the new, scaffolded `src-tauri/Cargo.toml` (which will already have its own `[package]`/`[dependencies]`/`[lib]`/`[[bin]]` sections from the scaffolding tool — add to, don't replace, those).

Also add these fields to the scaffolded `[package]` section, matching `orchestrator-cli`'s `Cargo.toml` (consistency across the workspace's binary crates):
```toml
license = "MIT OR Apache-2.0"
description = "Global-hotkey-triggered input automation for KDE Plasma/Wayland (GUI)"
repository = "https://github.com/barebones-sh/orchestrator"
authors = ["barebones-sh"]
```

- [ ] **Step 4: Wire the workspace root**

Edit the root `Cargo.toml`'s `[workspace]` section. It currently reads:
```toml
[workspace]
resolver = "2"
members = ["crates/*"]
```
Change to:
```toml
[workspace]
resolver = "2"
members = ["crates/*", "crates/orchestrator-gui/src-tauri"]
```
(The glob `crates/*` no longer finds `orchestrator-gui`'s manifest now that it moved a level deeper.)

- [ ] **Step 5: Generate a placeholder icon set**

```bash
cd crates/orchestrator-gui/src-tauri
cargo install tauri-cli --version "^2.0.0" --locked   # if `cargo tauri` isn't already available
```
Then generate icons from any simple placeholder source image (a solid-color square PNG is fine — this is explicitly a cosmetic placeholder per the spec's §9, not a design task):
```bash
cargo tauri icon <path-to-a-placeholder-source-image.png>
```
This populates `src-tauri/icons/` with the sizes Tauri's bundler expects.

- [ ] **Step 6: Build and live-verify**

```bash
cd crates/orchestrator-gui
npm install
cargo build -p orchestrator-gui --verbose   # or whatever the scaffolded crate is named -- check src-tauri/Cargo.toml's [package] name
```

Then actually launch it and confirm it runs without erroring — this sandbox has a real KDE Plasma / Wayland session (confirmed repeatedly this session via `busctl --user list` showing `org.kde.KWin`), so a real window should be able to open:

```bash
npm run tauri dev &
TAURI_PID=$!
sleep 15
# Confirm the process is still alive (didn't crash on startup)
kill -0 $TAURI_PID && echo "still running" || echo "CRASHED"
# If a screenshot tool is available (grim, spectacle, etc.), take one to
# confirm a window actually rendered -- not required if none is available,
# but try before concluding "still running" is the best evidence you have.
kill $TAURI_PID
```

If it crashes, read the error output before concluding Step 1's prerequisite check was insufficient (e.g. a missing package the check didn't cover) — report BLOCKED with the actual error if you can't resolve it yourself.

- [ ] **Step 7: Confirm the rest of the workspace still builds**

```bash
cargo build --workspace --verbose
cargo test --workspace --verbose
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
Expected: all pass (this task doesn't touch any other crate's source).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: scaffold Tauri v2 + Svelte GUI project structure"
```

Report in your task report: the exact scaffolding command you used, the Svelte major version it produced (Task 4 needs this), and the exact live-verification evidence (process stayed alive / screenshot / error output).

---

## Task 2: Move profile mutation logic into `orchestrator_core::profile_ops`

**Files:**
- Create: `crates/orchestrator-core/src/profile_ops.rs`
- Modify: `crates/orchestrator-core/src/lib.rs` (add the module)
- Modify: `crates/orchestrator-cli/src/profile_commands.rs` (thin wrapper delegating to `profile_ops`)

**Interfaces:**
- Produces: `orchestrator_core::profile_ops::{ProfileOpError, AddProfileArgs, EditProfileArgs, add_profile, edit_profile, remove_profile, resolve_window_scope, focus_steal_for, placeholder_trigger}` — Task 3 (GUI's Tauri commands) calls these directly.
- Consumes: nothing from Task 1.
- **Must not change:** `orchestrator-cli::profile_commands`'s public names/signatures (`ProfileAddArgs`, `ProfileEditArgs`, `ProfileActionArgs`, `ProfileCommandError`, `add_profile`, `edit_profile`, `remove_profile`, `resolve_window_scope`, `format_profile_list`) — `main.rs` calls all of these today and must not need to change.

- [ ] **Step 1: Create `orchestrator-core/src/profile_ops.rs`**

```rust
//! Profile CRUD logic operating on an in-memory `Config`, shared between
//! `orchestrator-cli` and `orchestrator-gui` (design spec
//! docs/superpowers/specs/2026-09-13-gui-design.md §5). Notation-string
//! parsing (the CLI's `key:<combo>:<delay_ms>` mini-language) stays in
//! `orchestrator-cli` -- everything here operates on already-typed
//! `Scope`/`Action` values, since the GUI's forms produce those directly
//! and the CLI resolves notation into these same types before calling in.

use orchestrator_hotkey::KeyCombo;
use orchestrator_window::WindowInfo;

use crate::action::Action;
use crate::profile::{Profile, Scope};
use crate::Config;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileOpError {
    #[error("no profile named {0:?}")]
    NoSuchProfile(String),
    #[error("a profile named {0:?} already exists")]
    DuplicateProfileName(String),
    // Only ever constructed by `resolve_window_scope`, itself only called
    // from each frontend's Linux-only window picker (CLI's
    // `linux_window_picker`, GUI's `list_windows` command). The logic is
    // deliberately portable/OS-independent (kept unit-testable on any
    // platform) -- `allow(dead_code)` on non-Linux rather than cfg-gating
    // the variants away, so they (and their unit tests) keep compiling and
    // meaning something on every target. (Carried over verbatim from the
    // pre-refactor `orchestrator-cli::profile_commands::ProfileCommandError`.)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("no windows are currently open to choose from")]
    NoWindowsAvailable,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("chosen window index {chosen} is out of range (0..{available})")]
    WindowIndexOutOfRange { chosen: usize, available: usize },
}

pub fn placeholder_trigger() -> KeyCombo {
    KeyCombo {
        modifiers: vec![],
        key: String::new(),
    }
}

pub fn focus_steal_for(scope: &Scope) -> bool {
    matches!(scope, Scope::Window { .. })
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_window_scope(
    windows: &[WindowInfo],
    chosen_index: usize,
) -> Result<Scope, ProfileOpError> {
    if windows.is_empty() {
        return Err(ProfileOpError::NoWindowsAvailable);
    }
    let window = windows
        .get(chosen_index)
        .ok_or(ProfileOpError::WindowIndexOutOfRange {
            chosen: chosen_index,
            available: windows.len(),
        })?;
    Ok(Scope::Window {
        process_name: window.process_name.clone().unwrap_or_default(),
        window_title_hint: window.title.clone(),
        backend_hint_id: Some(window.handle.0.clone()),
    })
}

#[derive(Debug, Clone)]
pub struct AddProfileArgs {
    pub name: String,
    pub scope: Scope,
    pub action: Action,
    pub debounce_ms: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct EditProfileArgs {
    pub scope: Option<Scope>,
    pub action: Option<Action>,
    pub debounce_ms: Option<u32>,
}

pub fn add_profile(config: &mut Config, args: AddProfileArgs) -> Result<(), ProfileOpError> {
    if config.profiles.iter().any(|p| p.name == args.name) {
        return Err(ProfileOpError::DuplicateProfileName(args.name));
    }
    let profile = Profile {
        name: args.name,
        trigger: placeholder_trigger(),
        focus_steal: focus_steal_for(&args.scope),
        scope: args.scope,
        action: args.action,
        debounce_ms: args.debounce_ms.unwrap_or(400),
    };
    config.profiles.push(profile);
    Ok(())
}

pub fn edit_profile(
    config: &mut Config,
    name: &str,
    args: EditProfileArgs,
) -> Result<(), ProfileOpError> {
    // Unlike the pre-refactor CLI version, there is no fallible parsing step
    // here (the caller resolves notation, if any, before calling this) --
    // the only failure mode is NoSuchProfile, checked before any mutation,
    // so atomicity is automatic.
    let profile = config
        .profiles
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| ProfileOpError::NoSuchProfile(name.to_string()))?;

    if let Some(scope) = args.scope {
        profile.focus_steal = focus_steal_for(&scope);
        profile.scope = scope;
    }
    if let Some(action) = args.action {
        profile.action = action;
    }
    if let Some(debounce_ms) = args.debounce_ms {
        profile.debounce_ms = debounce_ms;
    }
    Ok(())
}

pub fn remove_profile(config: &mut Config, name: &str) -> Result<(), ProfileOpError> {
    let before = config.profiles.len();
    config.profiles.retain(|p| p.name != name);
    if config.profiles.len() == before {
        return Err(ProfileOpError::NoSuchProfile(name.to_string()));
    }
    Ok(())
}
```

Add `pub mod profile_ops;` and `pub use profile_ops::*;` to `crates/orchestrator-core/src/lib.rs`, following the exact pattern its existing `pub mod action;`/`pub use action::*;` lines already use.

- [ ] **Step 2: Move the matching unit tests**

Move these tests (adapted to the new function/type names — `ProfileAddArgs`→`AddProfileArgs`, `ProfileEditArgs`→`EditProfileArgs`, dropping the `action: ProfileActionArgs::Repeat { input: "key:A".to_string(), ... }` notation shape in favor of directly-constructed `Action` values) from `orchestrator-cli/src/profile_commands.rs`'s test module into a new `#[cfg(test)] mod tests` at the bottom of `profile_ops.rs`:

- `resolve_window_scope_picks_the_chosen_index`
- `resolve_window_scope_rejects_out_of_range_index`
- `resolve_window_scope_rejects_empty_window_list`
- `add_profile_desktop_repeat_writes_placeholder_trigger_and_derived_focus_steal`
- `add_profile_rejects_duplicate_names`
- `add_profile_window_scope_derives_focus_steal_true`
- `add_profile_macro_with_steps_and_loop`
- `edit_profile_overwrites_only_given_fields`
- `edit_profile_errors_for_unknown_name`
- `edit_profile_scope_only_change_flips_focus_steal`
- `remove_profile_removes_by_name`
- `remove_profile_errors_for_unknown_name`

Do **not** move `add_profile_rejects_click_in_repeat_mode` or `edit_profile_is_atomic_when_action_notation_fails` — these test notation-parsing failure behavior (`build_action`/`parse_repeat_input` rejecting `"click:left:cursor"` in repeat mode), which stays a CLI-only concern (Step 3). Do not move `add_profile_alone_does_not_reject_zero_interval`/`add_profile_alone_does_not_reject_jitter_exceeding_interval`/`edit_profile_alone_does_not_reject_zero_interval` either — these test `Config::validate()`'s relationship to `add_profile`/`edit_profile`, which is identical behavior at either layer; keep them in the CLI's test module calling through the thin wrapper, since that's what actually ships to users and needs the coverage.

You will need to reconstruct each moved test's `Action` values directly (e.g. `Action::Repeat { input: InputEvent::KeyPress(KeyCombo { modifiers: vec![], key: "A".to_string() }), interval_ms: 100, jitter: Jitter::None }`) instead of going through notation strings — the CLI-side tests already show this exact expected-value shape (see e.g. `add_profile_desktop_repeat_writes_placeholder_trigger_and_derived_focus_steal`'s existing assertions), just move that construction to the *input* side instead of only the assertion side.

- [ ] **Step 3: Rewrite `orchestrator-cli/src/profile_commands.rs` as a thin wrapper**

Replace the file's `ProfileCommandError` enum, and `add_profile`/`edit_profile`/`remove_profile`/`resolve_window_scope` function bodies, keeping every other name and signature (`ProfileActionArgs`, `ProfileAddArgs`, `ProfileEditArgs`, `build_action`, `format_profile_list`) unchanged:

```rust
use orchestrator_core::profile_ops::{self, ProfileOpError};
// ... keep existing imports for Action/Jitter/Scope/Config/KeyCombo/WindowInfo/notation ...

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileCommandError {
    Notation(String),
    Op(ProfileOpError),
    MissingRequiredField(&'static str),
    RepeatAndMacroBothOrNeitherSpecified,
}

impl std::fmt::Display for ProfileCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notation(msg) => write!(f, "{msg}"),
            Self::Op(e) => write!(f, "{e}"),
            Self::MissingRequiredField(field) => write!(f, "--{field} is required"),
            Self::RepeatAndMacroBothOrNeitherSpecified => write!(
                f,
                "--action repeat's fields (--input/--interval-ms) and --action macro's \
                 fields (--step/--loop) cannot both be given at once -- only supply the \
                 fields for the --action you chose"
            ),
        }
    }
}

impl std::error::Error for ProfileCommandError {}

impl From<NotationError> for ProfileCommandError {
    fn from(e: NotationError) -> Self {
        Self::Notation(e.to_string())
    }
}

impl From<ProfileOpError> for ProfileCommandError {
    fn from(e: ProfileOpError) -> Self {
        Self::Op(e)
    }
}

// build_action, ProfileActionArgs, ProfileAddArgs, ProfileEditArgs, format_profile_list: UNCHANGED, keep as-is.

pub fn resolve_window_scope(
    windows: &[WindowInfo],
    chosen_index: usize,
) -> Result<Scope, ProfileCommandError> {
    profile_ops::resolve_window_scope(windows, chosen_index).map_err(Into::into)
}

pub fn add_profile(config: &mut Config, args: ProfileAddArgs) -> Result<(), ProfileCommandError> {
    let action = build_action(&args.action)?;
    profile_ops::add_profile(
        config,
        profile_ops::AddProfileArgs {
            name: args.name,
            scope: args.scope,
            action,
            debounce_ms: args.debounce_ms,
        },
    )
    .map_err(Into::into)
}

pub fn edit_profile(
    config: &mut Config,
    name: &str,
    args: ProfileEditArgs,
) -> Result<(), ProfileCommandError> {
    // Resolve the fallible notation-parsing step BEFORE calling into
    // profile_ops -- profile_ops::edit_profile itself has no fallible step
    // left (see its doc comment), so THIS is where the original atomicity
    // guarantee ("a rejected edit is a true no-op") now lives.
    let resolved_action = match &args.action {
        Some(action_args) => Some(build_action(action_args)?),
        None => None,
    };
    profile_ops::edit_profile(
        config,
        name,
        profile_ops::EditProfileArgs {
            scope: args.scope,
            action: resolved_action,
            debounce_ms: args.debounce_ms,
        },
    )
    .map_err(Into::into)
}

pub fn remove_profile(config: &mut Config, name: &str) -> Result<(), ProfileCommandError> {
    profile_ops::remove_profile(config, name).map_err(Into::into)
}
```

Also add `#[cfg_attr(not(target_os = "linux"), allow(dead_code))]` back onto this file's `resolve_window_scope` wrapper (it carries the same "only called from Linux-only `linux_window_picker`" situation the original had).

- [ ] **Step 4: Update the remaining CLI-side tests to match the new error shape**

Every remaining test in `profile_commands.rs`'s test module that asserts against `ProfileCommandError::NoSuchProfile(...)`, `ProfileCommandError::DuplicateProfileName(...)`, `ProfileCommandError::NoWindowsAvailable`, or `ProfileCommandError::WindowIndexOutOfRange { .. }` needs updating to the wrapped shape, e.g.:
```rust
// before:
assert_eq!(err, ProfileCommandError::NoSuchProfile("nope".to_string()));
// after:
assert_eq!(err, ProfileCommandError::Op(ProfileOpError::NoSuchProfile("nope".to_string())));
```
This applies to (at least): `resolve_window_scope_rejects_out_of_range_index`, `resolve_window_scope_rejects_empty_window_list` (if kept here as a thin-wrapper-level test — see Step 2's note: these three specific tests move to `profile_ops.rs` entirely, so this is about any *other* remaining reference), `add_profile_rejects_duplicate_names`, `edit_profile_errors_for_unknown_name`, `remove_profile_errors_for_unknown_name`. `edit_profile_is_atomic_when_action_notation_fails` keeps its `ProfileCommandError::Notation(_)` assertion unchanged (notation failure, not an Op failure).

- [ ] **Step 5: Verify**

```bash
cargo build --workspace --verbose
cargo test --workspace --verbose
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
Expected: all pass. Confirm specifically that `orchestrator-cli`'s test count is roughly the same as before this task (tests moved, not lost — count them before and after) and that `main.rs` required zero changes (`git diff --stat` should show no hunk in `crates/orchestrator-cli/src/main.rs`).

- [ ] **Step 6: Commit**

```bash
git add crates/orchestrator-core/src/profile_ops.rs crates/orchestrator-core/src/lib.rs crates/orchestrator-cli/src/profile_commands.rs
git commit -m "refactor: move profile mutation logic into orchestrator_core::profile_ops"
```

---

## Task 3: Tauri backend commands

**Files:**
- Modify: `crates/orchestrator-gui/src-tauri/src/main.rs` (or `lib.rs`, if the scaffolding tool split them — check what Task 1 produced)

**Interfaces:**
- Consumes: `orchestrator_core::profile_ops::{AddProfileArgs, EditProfileArgs, add_profile, edit_profile, remove_profile}`, `orchestrator_core::{Config, Profile, Scope, Action}` (Task 2). `orchestrator_window::kwin_dbus::KwinWindowLocator`, `orchestrator_window::{WindowLocator, WindowInfo}` (pre-existing).
- Produces: five Tauri commands the frontend calls via `invoke()` (Task 4): `list_profiles`, `add_profile`, `edit_profile`, `remove_profile`, `list_windows`.

Read whatever `crates/orchestrator-gui/src-tauri/src/main.rs` (or `lib.rs`) Task 1's scaffold actually produced first — the scaffolding tool's default `greet`-style example command should be replaced by these, not left alongside them (delete the example command and its frontend call site, if the scaffold ships one).

- [ ] **Step 1: Add the config-loading helper**

Tauri commands need the same "no config file yet = start empty, but a real parse/validate error is fatal" behavior `orchestrator-cli/src/main.rs`'s `load_or_default_config`/`is_first_run_missing_file` already implement — reusing those isn't possible without changing `orchestrator-cli` (out of this task's scope; a small, disclosed duplication is an acceptable tradeoff here, same size as the original CLI helper):

```rust
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
```

(Check `orchestrator_core::ConfigError`'s exact variant shape in `crates/orchestrator-core/src/error.rs` before writing this — match its real structure rather than guessing.)

- [ ] **Step 2: Add the five commands**

```rust
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
```

Note `remove_profile` deliberately does not call `config.validate()` before saving — removing a profile cannot introduce a new validation failure (same reasoning `orchestrator-cli/src/main.rs`'s `Remove` handler already documents), so this mirrors that, not an oversight.

- [ ] **Step 3: Register the commands**

Find wherever Task 1's scaffold calls `.invoke_handler(tauri::generate_handler![...])` (in `main()` or a `run()` function `main` calls, per whatever the scaffold's exact shape is) and replace its argument list with `tauri::generate_handler![list_profiles, add_profile, edit_profile, remove_profile, list_windows]`, removing any scaffold-provided example command.

- [ ] **Step 4: Verify**

```bash
cargo build -p orchestrator-gui --verbose   # check src-tauri/Cargo.toml for the exact package name Task 1 produced
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Live-verify at least `list_profiles` and `add_profile` actually work end-to-end before Task 4 exists to call them from a UI — Tauri's dev tools let you invoke commands directly from the webview's devtools console once `npm run tauri dev` is running:
```js
window.__TAURI__.core.invoke('list_profiles').then(console.log)
window.__TAURI__.core.invoke('add_profile', {name: 'gui-test', scope: 'Desktop', action: {Repeat: {input: {KeyPress: {modifiers: [], key: 'A'}}, interval_ms: 100, jitter: 'None'}}, debounceMs: null}).then(console.log)
```
(Exact JSON shape for `scope`/`action` depends on how `Scope`/`Action`'s `serde` derive serializes their enum variants — check `crates/orchestrator-core/src/profile.rs`/`action.rs` for `#[serde(...)]` attributes, if any, before guessing the shape; adjust the example above to match what actually (de)serializes correctly.) Confirm the profile actually lands in `~/.config/orchestrator/config.json` (or wherever `Config::config_path()` resolves on this machine), then remove the test profile via `remove_profile` before finishing.

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator-gui/src-tauri/
git commit -m "feat: add Tauri profile CRUD and window-listing commands"
```

---

## Task 4: Svelte frontend

**Files:**
- Modify: `crates/orchestrator-gui/src/` (whatever Task 1's scaffold produced — likely `App.svelte` plus new component files)

**Interfaces:**
- Consumes: the five Tauri commands from Task 3, called via `@tauri-apps/api`'s `invoke()`.

Read Task 1's report for which Svelte major version was scaffolded (5 with runes, or 4 with `export let`/`$:`) and write idiomatic code for that version — do not assume without checking, the syntax differs meaningfully between them.

- [ ] **Step 1: Build the profile list view**

`App.svelte` (or a child component it renders) calls `list_profiles` on mount and renders each profile as a row: name, a scope summary (`"Desktop"` or `"Window(<process_name>)"`), an action summary (`"Repeat every <interval_ms>ms"` or `"Macro (<N> steps)"` — same information `orchestrator-cli`'s `format_profile_list` shows, just as UI instead of a tab-separated string), and Edit/Remove buttons per row, plus an "Add profile" button.

- [ ] **Step 2: Build the add/edit form**

A form component (used for both add and edit — edit pre-fills from the selected profile) with fields:
- Name (text input, add-only — profile names aren't editable after creation, matching the CLI's own design: `profile edit <name>` never changes `name`).
- Scope: a radio/select between "Desktop" and "Window". Choosing "Window" calls `list_windows`, shows the results as a picklist (process name + title), and on selection stores the chosen window's data as a `Scope::Window` value (mirroring `resolve_window_scope`'s shape: `process_name`, `window_title_hint` from the window's title, `backend_hint_id` from the window's handle) rather than calling `resolve_window_scope` itself — that function takes an index into a list the frontend already has in hand, so the frontend can construct the `Scope::Window` value directly without a second round-trip.
- Action: a select between "Repeat" and "Macro". Repeat shows: an input-kind selector (key combo text field, or scroll dx/dy number fields — mirroring the CLI's `key:`/`scroll:` notation kinds, just as separate typed fields instead of one string) built into an `Action::Repeat`'s `input: InputEvent`, plus `interval_ms` and optional `jitter_ms` number fields. Macro shows a repeatable list of steps (same input-kind choice per step, plus a `delay_ms` field) and a "loop" checkbox.
- Debounce (optional number field, defaults to 400 if left blank — matching `AddProfileArgs`'s `debounce_ms: Option<u32>`).

On submit, call `add_profile` or `edit_profile` with the constructed `Scope`/`Action` values; on error (the commands return `Result<_, String>`), show the message somewhere visible (a simple inline error banner is enough — no toast library, YAGNI).

- [ ] **Step 3: Wire Remove**

Each row's Remove button calls `remove_profile(name)` and refreshes the list on success.

- [ ] **Step 4: Live-verify the whole flow**

With `npm run tauri dev` running (same live-KDE-session environment Task 1 confirmed works), actually click through: add a Desktop-scope Repeat profile, confirm it appears in the list; edit it; add a Window-scope profile using the window picker (open some other window first, e.g. a terminal, so the picker has something to list); remove both. Confirm `~/.config/orchestrator/config.json` reflects each step by reading it directly, and confirm it's empty/clean afterward (no leftover test profiles) — same "leave the machine as you found it" bar this project's other live-verified tasks already hold to.

- [ ] **Step 5: Verify the gates**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

- [ ] **Step 6: Commit**

```bash
git add crates/orchestrator-gui/src/ crates/orchestrator-gui/package.json crates/orchestrator-gui/package-lock.json
git commit -m "feat: add profile management UI (list, add, edit, remove)"
```

---

## Plan-Level Verification

- `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` all pass after all 4 tasks.
- The full add → edit → remove flow was live-verified by actually clicking through the running app (Task 4, Step 4), not just by reading the code.
- `orchestrator-cli/src/main.rs` required zero changes across this entire plan (Task 2's constraint) — confirm with `git diff --stat main.rs` across the whole plan's commit range.
- No leftover test profiles or scratch files in `~/.config/orchestrator/config.json` or the repo.
- Design-intent check: live start/stop, tray icon, app icon design, `.desktop` entry, and `.deb` packaging for the GUI were NOT implemented (Global Constraints) — those are explicitly the next increment.
