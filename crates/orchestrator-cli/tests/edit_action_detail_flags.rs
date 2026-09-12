//! Final-review Fix 2: `profile edit` must not silently drop action-detail
//! flags (`--input`/`--interval-ms`/`--jitter-ms`/`--step`/`--loop`) when
//! `--action` is omitted. Runs the real compiled `orchestrator` binary
//! against an isolated temp config file.

use std::path::PathBuf;
use std::process::Command;

fn orchestrator_cmd(config_path: &PathBuf) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orchestrator"));
    cmd.arg("--config").arg(config_path);
    cmd
}

fn temp_config_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "orchestrator-cli-e2e-{label}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    path
}

struct CleanupOnDrop(PathBuf);

impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let mut tmp = self.0.as_os_str().to_owned();
        tmp.push(".tmp");
        let _ = std::fs::remove_file(PathBuf::from(tmp));
    }
}

#[test]
fn edit_with_interval_ms_but_no_action_errors_instead_of_silently_doing_nothing() {
    let path = temp_config_path("edit-no-action");
    let _cleanup = CleanupOnDrop(path.clone());

    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "p1",
            "--scope",
            "desktop",
            "--action",
            "repeat",
            "--input",
            "key:A",
            "--interval-ms",
            "100",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "initial add failed: {out:?}");

    // Forgetting --action while giving --interval-ms must be a clear
    // error, not a silent no-op that prints "profile updated." having
    // changed nothing.
    let out = orchestrator_cmd(&path)
        .args(["profile", "edit", "p1", "--interval-ms", "999"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "edit with --interval-ms but no --action must fail, not silently succeed"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("profile updated"),
        "must not claim success when the flag was silently dropped: {stdout:?}"
    );

    // And the profile's interval_ms must be unchanged.
    let listing = orchestrator_cmd(&path)
        .args(["profile", "list"])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("100ms"),
        "interval_ms must remain 100 after the rejected edit, got: {listing:?}"
    );
    assert!(!listing.contains("999ms"));
}

#[test]
fn add_macro_with_jitter_ms_is_rejected() {
    let path = temp_config_path("macro-jitter");
    let _cleanup = CleanupOnDrop(path.clone());

    // final-review Fix 2 (related gap): --jitter-ms must be treated as
    // repeat-only the same way --input/--interval-ms already are, so
    // combining it with --action macro must be rejected, not silently
    // dropped.
    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "m1",
            "--scope",
            "desktop",
            "--action",
            "macro",
            "--step",
            "key:A:10",
            "--jitter-ms",
            "10",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "--action macro combined with --jitter-ms must be rejected"
    );
}
