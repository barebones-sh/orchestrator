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
