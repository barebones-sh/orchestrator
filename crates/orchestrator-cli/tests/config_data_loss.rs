//! Final-review Fix 1 (CRITICAL): an invalid `profile add`/`edit` must
//! neither be written to disk NOR cause a later command to treat an
//! existing, valid config file as empty. This is an end-to-end reproduction
//! of the exact sequence the reviewer demonstrated live: two valid
//! profiles, then one invalid `add`, then `profile list`, then one more
//! valid `add` -- at every step the two original profiles must survive.
//!
//! Runs the real compiled `orchestrator` binary (via
//! `CARGO_BIN_EXE_orchestrator`, which Cargo sets for integration tests
//! automatically -- no extra test-harness dependency needed) against an
//! isolated temp config file, so this exercises the actual `main.rs` glue
//! (argument parsing, `load_or_default_config`, the `validate()`-before-
//! `save()` ordering) rather than just the pure `profile_commands`/`Config`
//! layers underneath it.

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

#[test]
fn invalid_add_does_not_destroy_existing_profiles() {
    let path = temp_config_path("invalid-add");
    let _cleanup = CleanupOnDrop(path.clone());

    // 1. Add two valid profiles.
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
    assert!(out.status.success(), "p1 add failed: {out:?}");

    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "p2",
            "--scope",
            "desktop",
            "--action",
            "repeat",
            "--input",
            "key:B",
            "--interval-ms",
            "200",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "p2 add failed: {out:?}");

    let listing_before = orchestrator_cmd(&path)
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert!(listing_before.status.success());
    let listing_before = String::from_utf8_lossy(&listing_before.stdout).to_string();
    assert!(listing_before.contains("p1"), "{listing_before}");
    assert!(listing_before.contains("p2"), "{listing_before}");

    // 2. Attempt to add an invalid profile: interval_ms 0 fails
    // `Config::validate` (`ConfigError::InvalidInterval`). This must fail
    // loudly and must NOT be written to disk.
    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "bad",
            "--scope",
            "desktop",
            "--action",
            "repeat",
            "--input",
            "key:C",
            "--interval-ms",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "adding a profile with --interval-ms 0 must fail, not succeed"
    );

    // 3. `profile list` right after the failed add must still show the
    // original two profiles -- NOT an empty list. This is the core of the
    // data-loss bug: `load_or_default_config` swallowing the "file exists
    // but now let's pretend it's fine" case would show nothing here.
    let listing_after = orchestrator_cmd(&path)
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert!(listing_after.status.success());
    let listing_after = String::from_utf8_lossy(&listing_after.stdout).to_string();
    assert!(
        listing_after.contains("p1"),
        "p1 must survive a failed add, got: {listing_after:?}"
    );
    assert!(
        listing_after.contains("p2"),
        "p2 must survive a failed add, got: {listing_after:?}"
    );
    assert!(
        !listing_after.contains("bad"),
        "the invalid profile must never have been saved, got: {listing_after:?}"
    );

    // 4. One more valid add must NOT physically overwrite the file with a
    // fresh empty config plus just the new profile -- p1/p2 must still be
    // there afterwards.
    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "p3",
            "--scope",
            "desktop",
            "--action",
            "repeat",
            "--input",
            "key:D",
            "--interval-ms",
            "300",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "p3 add failed: {out:?}");

    let listing_final = orchestrator_cmd(&path)
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert!(listing_final.status.success());
    let listing_final = String::from_utf8_lossy(&listing_final.stdout).to_string();
    assert!(listing_final.contains("p1"), "{listing_final}");
    assert!(listing_final.contains("p2"), "{listing_final}");
    assert!(listing_final.contains("p3"), "{listing_final}");
}

#[test]
fn invalid_edit_does_not_destroy_existing_profiles() {
    let path = temp_config_path("invalid-edit");
    let _cleanup = CleanupOnDrop(path.clone());

    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "add",
            "--name",
            "keep-me",
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

    // A jitter range exceeding interval_ms fails `Config::validate`
    // (`ConfigError::JitterRangeExceedsInterval`).
    let out = orchestrator_cmd(&path)
        .args([
            "profile",
            "edit",
            "keep-me",
            "--action",
            "repeat",
            "--input",
            "key:A",
            "--interval-ms",
            "100",
            "--jitter-ms",
            "999",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "editing to a jitter_ms exceeding interval_ms must fail, not succeed"
    );

    let listing = orchestrator_cmd(&path)
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("keep-me"),
        "the original profile must survive a failed edit, got: {listing:?}"
    );
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
