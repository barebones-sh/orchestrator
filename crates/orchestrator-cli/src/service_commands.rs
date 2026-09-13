//! Pure logic for `orchestrator service` (autostart via systemd --user).
//!
//! Split from the actual `systemctl`/filesystem side effects (in `main.rs`'s
//! `linux_service` module) the same way `profile_commands.rs` separates pure
//! config-mutation logic from `main.rs`'s I/O -- this half is unit-tested,
//! the glue half is live-verified only (see design spec's established
//! pattern for backend-touching code, e.g. `linux_run`).
//!
//! All public items here are unused (and so would otherwise warn as dead
//! code) on a non-Linux build: their only consumer, the `linux_service`
//! module in `main.rs`, is `#[cfg(target_os = "linux")]`-gated and so is
//! compiled out entirely outside Linux builds. This is permanent, not a
//! transient state pending some later change.

#![allow(dead_code)]

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
///
/// `exec_start` is escaped before interpolation: any literal `%` is doubled
/// (systemd's own escape for a literal percent sign, since `%d` etc. are
/// specifiers expanded by systemd) and the whole command is wrapped in
/// double quotes so a space in the path doesn't split it into multiple
/// arguments (final-review Fix 2 -- confirmed live: an unescaped path
/// containing a space truncates the command at the first space, and one
/// containing `%` gets specifier-expanded).
///
/// `RestartSec=5` plus the explicit `StartLimitIntervalSec=60` /
/// `StartLimitBurst=3` in `[Unit]` are final-review Fix 1: `RestartSec=2`
/// sat exactly on systemd's default rate-limit threshold
/// (`StartLimitIntervalUSec=10s` / `StartLimitBurst=5`, i.e. 2s/restart), so
/// real process startup overhead pushed every actual restart just past that
/// threshold and the limiter never engaged -- verified live as an
/// unbounded crash loop (25 restarts in 35 seconds, still climbing). The
/// explicit settings make the limiter engage predictably regardless of
/// systemd's own defaults.
pub fn render_unit(exec_start: &str) -> String {
    let escaped = exec_start.replace('%', "%%");
    format!(
        "[Unit]\n\
         Description=Orchestrator hotkey automation runner\n\
         After=graphical-session.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=3\n\
         \n\
         [Service]\n\
         ExecStart=\"{escaped}\" run\n\
         Restart=on-failure\n\
         RestartSec=5\n\
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
        assert!(unit.contains("ExecStart=\"/usr/bin/orchestrator\" run"));
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
        assert!(
            unit.contains("ExecStart=\"/home/dev/src/orchestrator/target/debug/orchestrator\" run")
        );
    }

    #[test]
    fn user_unit_path_ends_with_systemd_user_orchestrator_service() {
        let path = user_unit_path();
        assert!(path.ends_with("systemd/user/orchestrator.service"));
    }

    // final-review Fix 1: pins the restart-rate-limit invariant in code, not
    // just in a commit message -- RestartSec=5 combined with
    // StartLimitIntervalSec=60/StartLimitBurst=3 clears systemd's rate-limit
    // threshold (unlike the old RestartSec=2, which sat exactly on the
    // default threshold and never actually engaged the limiter).
    #[test]
    fn render_unit_uses_a_restart_interval_that_clears_the_rate_limit_threshold() {
        let unit = render_unit("/usr/bin/orchestrator");
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("StartLimitBurst=3"));
        assert!(unit.contains("StartLimitIntervalSec=60"));
    }

    // final-review Fix 2: exec_start must be escaped before interpolation.
    #[test]
    fn render_unit_escapes_a_literal_percent_in_exec_start() {
        let unit = render_unit("/usr/bin/orchestrator-%d");
        assert!(unit.contains("ExecStart=\"/usr/bin/orchestrator-%%d\" run"));
    }

    #[test]
    fn render_unit_quotes_a_path_containing_a_space() {
        let unit = render_unit("/home/dev/my project/orchestrator");
        assert!(unit.contains("ExecStart=\"/home/dev/my project/orchestrator\" run"));
    }

    // final-review Fix 4e: locks render_unit and the committed packaging
    // file together so they can't silently drift apart.
    #[test]
    fn render_unit_matches_the_committed_packaging_file() {
        let committed = include_str!("../../../packaging/linux/orchestrator.service");
        assert_eq!(render_unit("/usr/bin/orchestrator"), committed);
    }
}
