# Autostart Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Orchestrator run automatically at login via a systemd user service, closing the "autostart integration" item the original design spec named as deferred (`docs/superpowers/specs/2026-08-09-orchestrator-design.md` §7), instead of requiring `orchestrator run` to be started manually every session.

**Architecture:** A systemd user unit template (`packaging/linux/orchestrator.service`) shipped two ways: as a `.deb` asset at the vendor path `/usr/lib/systemd/user/` (auto-available to every user on a packaged install, same pattern `ydotool.service` already uses), and generated on the fly for source builds by a new `orchestrator service` CLI subcommand (`enable`/`disable`/`status`), which shells out to `systemctl --user`. The unit-content generation and path logic are pure, unit-tested functions in a new `service_commands.rs`; the actual `systemctl`/filesystem side effects live in a `cfg(target_os = "linux")` module in `main.rs`, mirroring the existing `linux_run`/`linux_window_picker` split (real I/O glue, live-verified rather than unit-tested, same as this project's established pattern for backend-touching code).

**Tech Stack:** No new external dependencies beyond `dirs` (already used elsewhere in this workspace, version `"7"`), added directly to `orchestrator-cli`'s own `Cargo.toml` (not the workspace-level table, matching how `orchestrator-core` already depends on it). `systemctl --user` (systemd) is invoked via `std::process::Command`, not a library.

**Spec:** None — this closes an item the original design spec explicitly named as deferred (`docs/superpowers/specs/2026-08-09-orchestrator-design.md` §7, "autostart integration"), with the concrete design confirmed directly with the human partner in conversation (systemd `--user` unit, `.deb`-vendor-path plus source-build fallback, no separate design doc needed since there's no remaining product ambiguity).

## Global Constraints

- Linux only, same scoping as `orchestrator run` and `--scope window`'s live picker: on non-Linux (`cfg(not(target_os = "linux"))`), `orchestrator service <subcommand>` must print a clear "only implemented on Linux" message and `std::process::exit(1)`, exactly mirroring `Command::Run`'s existing non-Linux branch in `main.rs`. Do not attempt any macOS implementation (`launchd`, etc.) in this plan.
- The unit's `ExecStart` for the **packaged** (`.deb`) case is a fixed absolute path, `/usr/bin/orchestrator run` — this must match exactly where the existing `[package.metadata.deb]` assets list already installs the binary (`crates/orchestrator-cli/Cargo.toml`'s existing `["target/release/orchestrator", "usr/bin/", "755"]` entry). For the **source-build** fallback case, the unit `orchestrator service enable` writes must instead point at the currently-running binary's own resolved path (`std::env::current_exe()`), not a hardcoded guess, so it works from `target/debug/` or `target/release/` alike.
- Do not touch `crates/orchestrator-gui` or any trait crate (`orchestrator-hotkey`/`-input`/`-window`) — this plan is `orchestrator-cli` plus packaging/docs only.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check` must both stay clean, matching this project's established non-negotiable gate.
- Do not push to `origin` as part of executing this plan — commit locally only. Pushing happens once, at the end, with the human partner's explicit go-ahead (this project's established practice all session).
- This plan's tasks work directly on the `main` branch (no worktree), matching the sustained implicit consent this project has operated under all session.

---

## Task 1: Unit file template and pure service-command logic

**Files:**
- Create: `packaging/linux/orchestrator.service`
- Create: `crates/orchestrator-cli/src/service_commands.rs`
- Modify: `crates/orchestrator-cli/Cargo.toml` (add `dirs = "7"` under `[dependencies]`)
- Modify: `crates/orchestrator-cli/src/main.rs` (add `mod service_commands;`)

**Interfaces:**
- Produces: `service_commands::render_unit(exec_start: &str) -> String`, `service_commands::user_unit_path() -> std::path::PathBuf`, `service_commands::VENDOR_UNIT_PATH: &str` (the constant `"/usr/lib/systemd/user/orchestrator.service"`), `service_commands::UNIT_FILE_NAME: &str` (the constant `"orchestrator.service"`) — Task 2 calls all four.
- Consumes: nothing from other tasks.

- [ ] **Step 1: Write the vendor unit template**

Create `packaging/linux/orchestrator.service`:

```ini
[Unit]
Description=Orchestrator hotkey automation runner
After=graphical-session.target

[Service]
ExecStart=/usr/bin/orchestrator run
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
```

This is the file a `.deb` install ships at `/usr/lib/systemd/user/orchestrator.service` (wired up in Task 3). Its `ExecStart` is fixed to the packaged binary's install path, per the Global Constraints.

- [ ] **Step 2: Write the failing tests for the pure logic**

Create `crates/orchestrator-cli/src/service_commands.rs`:

```rust
//! Pure logic for `orchestrator service` (autostart via systemd --user).
//!
//! Split from the actual `systemctl`/filesystem side effects (in `main.rs`'s
//! `linux_service` module) the same way `profile_commands.rs` separates pure
//! config-mutation logic from `main.rs`'s I/O -- this half is unit-tested,
//! the glue half is live-verified only (see design spec's established
//! pattern for backend-touching code, e.g. `linux_run`).

use std::path::PathBuf;

/// The unit file name, used both for the vendor path and the user-level
/// fallback path, and as the argument to every `systemctl --user` call.
pub const UNIT_FILE_NAME: &str = "orchestrator.service";

/// Where a `.deb` install ships the unit (systemd's vendor search path for
/// user units -- auto-available to every user on the machine with no
/// per-user copy needed, exactly like `ydotool.service` already does).
pub const VENDOR_UNIT_PATH: &str = "/usr/lib/systemd/user/orchestrator.service";

/// Renders the unit file content, given the absolute path to the
/// orchestrator binary to run. Used both to generate `packaging/linux/orchestrator.service`
/// (conceptually -- that file is committed directly, not generated at build
/// time) and, for source builds, to write a user-level unit at
/// `user_unit_path()` pointing at the currently-running binary's own path.
pub fn render_unit(exec_start: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Orchestrator hotkey automation runner\n\
         After=graphical-session.target\n\
         \n\
         [Service]\n\
         ExecStart={exec_start} run\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

/// The user-level fallback unit path for source builds (no `.deb`-installed
/// vendor unit present): `$XDG_CONFIG_HOME/systemd/user/orchestrator.service`,
/// or `~/.config/systemd/user/orchestrator.service` if unset -- systemd's
/// standard per-user unit search path. Panics only if `dirs::config_dir()`
/// returns `None`, mirroring `orchestrator_core::Config::config_path`'s own
/// documented behavior for the same rare case.
pub fn user_unit_path() -> PathBuf {
    dirs::config_dir()
        .expect("no config directory available for this platform")
        .join("systemd")
        .join("user")
        .join(UNIT_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unit_includes_the_given_exec_start_plus_run_argument() {
        let unit = render_unit("/usr/bin/orchestrator");
        assert!(unit.contains("ExecStart=/usr/bin/orchestrator run"));
    }

    #[test]
    fn render_unit_includes_restart_policy() {
        let unit = render_unit("/usr/bin/orchestrator");
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn render_unit_includes_install_section() {
        let unit = render_unit("/usr/bin/orchestrator");
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn render_unit_works_with_a_dev_build_path_too() {
        // Source builds point ExecStart at whatever `std::env::current_exe()`
        // resolves to -- e.g. a target/debug/ path, not just /usr/bin/.
        let unit = render_unit("/home/dev/src/orchestrator/target/debug/orchestrator");
        assert!(unit.contains(
            "ExecStart=/home/dev/src/orchestrator/target/debug/orchestrator run"
        ));
    }

    #[test]
    fn user_unit_path_ends_with_systemd_user_orchestrator_service() {
        let path = user_unit_path();
        assert!(path.ends_with("systemd/user/orchestrator.service"));
    }
}
```

- [ ] **Step 3: Wire the module in and run the tests**

Add `mod service_commands;` to `crates/orchestrator-cli/src/main.rs` (alongside the existing `mod notation;` / `mod profile_commands;` lines).

Add to `crates/orchestrator-cli/Cargo.toml`'s `[dependencies]` section (not the Linux/macOS target-specific tables further down -- this crate needs it on every platform, matching how `clap`/`tracing-subscriber` are listed):
```toml
dirs = "7"
```

Run: `cargo test -p orchestrator-cli --verbose`
Expected: all 5 new tests pass (plus every pre-existing test in this crate still passing).

- [ ] **Step 4: Commit**

```bash
git add packaging/linux/orchestrator.service crates/orchestrator-cli/src/service_commands.rs crates/orchestrator-cli/src/main.rs crates/orchestrator-cli/Cargo.toml
git commit -m "feat: add systemd user unit template and pure service-command logic"
```

---

## Task 2: `orchestrator service` CLI subcommand

**Files:**
- Modify: `crates/orchestrator-cli/src/main.rs`

**Interfaces:**
- Consumes: `service_commands::{render_unit, user_unit_path, VENDOR_UNIT_PATH, UNIT_FILE_NAME}` (Task 1).
- Produces: nothing further tasks depend on (Task 3 only touches packaging/docs, not code).

Read `crates/orchestrator-cli/src/main.rs` in full first -- this task adds to the existing `Command` enum and match arms using the exact same style already there for `Command::Profile`/`Command::Run` (a `#[cfg(target_os = "linux")]` real-implementation match arm paired with a `#[cfg(not(target_os = "linux"))]` "not implemented on this platform" arm; a private module gated the same way for the actual glue, following the existing `linux_run` module's shape).

- [ ] **Step 1: Add the `Service` subcommand to the `Command` enum**

In `main.rs`, extend the existing `Command` enum (currently `Profile(ProfileCommand)` and `Run`):

```rust
#[derive(Subcommand)]
enum Command {
    /// Profile management: add, edit, remove, or list profiles.
    #[command(subcommand)]
    Profile(ProfileCommand),

    /// Load the config and run all profiles until Ctrl-C.
    Run,

    /// Autostart management: run Orchestrator automatically at login via a
    /// systemd --user service.
    #[command(subcommand)]
    Service(ServiceCommand),
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Install (if needed) and enable + start the systemd user service.
    Enable,
    /// Disable + stop the systemd user service.
    Disable,
    /// Show `systemctl --user status` for the service.
    Status,
}
```

- [ ] **Step 2: Add the `linux_service` module**

Add this module to `main.rs`, alongside the existing `linux_run` module (same file, same `#[cfg(target_os = "linux")]` gating style):

```rust
/// Installs (for source builds only -- packaged installs already ship the
/// vendor unit via `.deb`), enables, disables, and reports the status of
/// the systemd --user service that runs `orchestrator run` at login.
///
/// Real `systemctl`/filesystem side effects live here, deliberately
/// unit-untested (there's no meaningful way to unit-test invoking a real
/// systemd user instance) -- verified live instead, the same way
/// `linux_run`'s real backend construction is. `service_commands.rs` holds
/// everything about this feature that *can* be pure-tested.
#[cfg(target_os = "linux")]
mod linux_service {
    use crate::service_commands::{render_unit, user_unit_path, UNIT_FILE_NAME, VENDOR_UNIT_PATH};
    use std::process::Command;

    /// Ensures a unit file is in place (vendor path from a `.deb` install,
    /// or a freshly written user-level one for source builds), then enables
    /// and starts it.
    pub fn enable() {
        let vendor_present = std::path::Path::new(VENDOR_UNIT_PATH).exists();
        if !vendor_present {
            let user_path = user_unit_path();
            if !user_path.exists() {
                let exe = match std::env::current_exe() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("failed to resolve the current executable's path: {e}");
                        std::process::exit(1);
                    }
                };
                let unit = render_unit(&exe.display().to_string());
                if let Some(parent) = user_path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!(
                            "failed to create {} for the systemd user unit: {e}",
                            parent.display()
                        );
                        std::process::exit(1);
                    }
                }
                if let Err(e) = std::fs::write(&user_path, unit) {
                    eprintln!(
                        "failed to write the systemd user unit to {}: {e}",
                        user_path.display()
                    );
                    std::process::exit(1);
                }
                println!("wrote {}", user_path.display());
            }
            run_systemctl(&["--user", "daemon-reload"]);
        }
        run_systemctl(&["--user", "enable", "--now", UNIT_FILE_NAME]);
        println!("{UNIT_FILE_NAME} enabled and started.");
    }

    pub fn disable() {
        run_systemctl(&["--user", "disable", "--now", UNIT_FILE_NAME]);
        println!("{UNIT_FILE_NAME} disabled and stopped.");
    }

    pub fn status() {
        let status = Command::new("systemctl")
            .args(["--user", "status", UNIT_FILE_NAME])
            .status();
        match status {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("failed to run systemctl: {e}");
                std::process::exit(1);
            }
        }
    }

    /// Runs `systemctl <args>`, inheriting stdio so the user sees systemctl's
    /// own output, and exits the process on failure (a non-zero exit or a
    /// failure to even launch `systemctl`, e.g. it's not installed).
    fn run_systemctl(args: &[&str]) {
        match Command::new("systemctl").args(args).status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "systemctl {} failed with {s}",
                    args.join(" ")
                );
                std::process::exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("failed to run systemctl {}: {e}", args.join(" "));
                std::process::exit(1);
            }
        }
    }
}
```

- [ ] **Step 3: Dispatch the new subcommand in `main()`**

Add these arms to the `match cli.command` block in `main()`, next to the existing `Command::Run` arms:

```rust
        #[cfg(target_os = "linux")]
        Some(Command::Service(ServiceCommand::Enable)) => linux_service::enable(),
        #[cfg(target_os = "linux")]
        Some(Command::Service(ServiceCommand::Disable)) => linux_service::disable(),
        #[cfg(target_os = "linux")]
        Some(Command::Service(ServiceCommand::Status)) => linux_service::status(),
        #[cfg(not(target_os = "linux"))]
        Some(Command::Service(_)) => {
            eprintln!("`service` is only implemented on Linux in this increment (macOS backends are still stubs)");
            std::process::exit(1);
        }
```

- [ ] **Step 4: Build and run the workspace tests**

Run:
```bash
cargo build --workspace --verbose
cargo test --workspace --verbose
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
Expected: all pass, matching this project's established baseline (no regressions -- this task only adds new code paths).

- [ ] **Step 5: Live-verify in this sandbox**

This sandbox has a real KDE Plasma / systemd --user session (confirmed earlier this session via `loginctl show-session` reporting `Type=wayland` and `busctl --user list` showing `org.kde.KWin`). Verify for real, from a dev build (`cargo build` first if needed):

```bash
# Confirm the target session's default systemd target reaches graphical-session.target
# before relying on it -- if this reports inactive/unknown, switch this plan's unit
# (Step 1 of Task 1, and this render_unit's WantedBy=/After=) to `default.target`
# instead, and note the change and why in this task's report.
systemctl --user is-active graphical-session.target

./target/debug/orchestrator service enable
systemctl --user is-enabled orchestrator.service   # expect: enabled
systemctl --user is-active orchestrator.service    # expect: active
cat ~/.config/systemd/user/orchestrator.service    # confirm ExecStart points at the dev binary's real path
./target/debug/orchestrator service status         # expect: exit 0, shows "active (running)"
./target/debug/orchestrator service disable
systemctl --user is-enabled orchestrator.service   # expect: disabled (or "No such file" -- either is fine post-disable)
```

If `graphical-session.target` is not active in this sandbox, redo Step 1 of Task 1 (both the committed `packaging/linux/orchestrator.service` and this module's `render_unit`'s hardcoded string) using `default.target` for both `After=` and `WantedBy=` instead, then re-run this verification. Note in your report which target was actually used and why.

- [ ] **Step 6: Commit**

```bash
git add crates/orchestrator-cli/src/main.rs
git commit -m "feat: add orchestrator service enable/disable/status subcommand"
```

Do **not** push -- pushing is a separate, explicit step the controller confirms with the human partner after this plan's review is complete.

---

## Task 3: Packaging and docs

**Files:**
- Modify: `crates/orchestrator-cli/Cargo.toml` (add the unit file to `[package.metadata.deb]`'s `assets` list)
- Modify: `README.md` (repo root)
- Modify: `packaging/linux/README.md`

**Interfaces:** None -- this task only touches packaging metadata and documentation, no code.

- [ ] **Step 1: Add the unit file as a `.deb` asset**

`crates/orchestrator-cli/Cargo.toml`'s `[package.metadata.deb]` section currently reads:

```toml
[package.metadata.deb]
depends = "ydotool"
license-file = ["../../packaging/linux/deb-license.txt", "0"]
assets = [
    ["target/release/orchestrator", "usr/bin/", "755"],
    ["../../packaging/linux/io.github.barebonessh.Orchestrator.desktop", "usr/share/applications/", "644"],
]
```

Add a third entry to `assets`, following the exact same `../../packaging/linux/...` relative-path convention already established and independently verified twice this session (cargo-deb resolves such paths relative to the crate's own manifest directory, `crates/orchestrator-cli/`, not the workspace root):

```toml
assets = [
    ["target/release/orchestrator", "usr/bin/", "755"],
    ["../../packaging/linux/io.github.barebonessh.Orchestrator.desktop", "usr/share/applications/", "644"],
    ["../../packaging/linux/orchestrator.service", "usr/lib/systemd/user/", "644"],
]
```

- [ ] **Step 2: Verify the built `.deb` actually contains it**

```bash
cargo build --release -p orchestrator-cli
cargo deb -p orchestrator-cli --no-build
dpkg-deb --contents target/debian/orchestrator-cli_*.deb | grep systemd
```
Expected: a line showing `./usr/lib/systemd/user/orchestrator.service` in the package contents, mode `-rw-r--r--`.

- [ ] **Step 3: Document autostart in the root README**

Add a new `## Autostart` section to `README.md` (repo root), placed after the existing "Basic usage" section and before "License":

```markdown
## Autostart

Run Orchestrator automatically at login via a systemd user service:

```
orchestrator service enable
```

A `.deb` install already ships the unit at `/usr/lib/systemd/user/`, so
`enable` just turns it on. A from-source build has no such file yet --
`enable` writes one for you, pointing at your current build (`target/debug/`
or `target/release/`, whichever you ran it from), before enabling it.

`orchestrator service status` shows `systemctl --user status` for the
service; `orchestrator service disable` turns it back off.
```

- [ ] **Step 4: Document the source-build specifics in `packaging/linux/README.md`**

Add a short section to `packaging/linux/README.md` (after its existing content) noting the dev-build-specific behavior:

```markdown

## Autostart (dev builds)

`orchestrator service enable` writes a user-level systemd unit at
`~/.config/systemd/user/orchestrator.service` when no `.deb`-installed
vendor unit is present at `/usr/lib/systemd/user/orchestrator.service` --
pointing `ExecStart` at whatever binary you actually ran `enable` from
(`target/debug/orchestrator` or `target/release/orchestrator`). If you
rebuild to a different profile (debug vs. release) after enabling, re-run
`orchestrator service enable` to refresh the unit (it only writes one if
none exists yet -- delete the old one first, or `service disable` then
`rm ~/.config/systemd/user/orchestrator.service` before re-enabling with
the new build).
```

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator-cli/Cargo.toml README.md packaging/linux/README.md
git commit -m "docs: document orchestrator service autostart, package the systemd unit"
```

---

## Plan-Level Verification

- `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` all pass after all 3 tasks (Task 2's Step 4 covers the first run of this; re-run once more after Task 3 to confirm the `Cargo.toml`/doc-only changes didn't regress anything).
- `orchestrator service enable`/`status`/`disable` were live-verified against this sandbox's real systemd --user instance (Task 2, Step 5) -- not just written and assumed to work.
- The built `.deb` actually contains the unit file at the right path (Task 3, Step 2) -- not just declared in `Cargo.toml` and assumed correct.
- Design-intent check: every Global Constraint is reflected -- Linux-only with a clear non-Linux error message matching `Command::Run`'s existing pattern; the packaged `ExecStart` is the fixed `/usr/bin/orchestrator` path matching the existing binary asset entry; the source-build fallback resolves `std::env::current_exe()` rather than hardcoding a guess; no `orchestrator-gui`/trait-crate changes.
