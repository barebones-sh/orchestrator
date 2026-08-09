//! Phase 3c: the combo that matters for `Scope::Window` profiles -- snapshot
//! current focus, activate the target window via KWin scripting, inject via
//! ydotool immediately (no gap for focus to drift, unlike the phase 2 standalone
//! test), then restore prior focus. Measures whether the keystroke actually
//! lands in the target and whether focus visibly flickers.

use crate::kwin_windows::{activate_window, list_windows};
use anyhow::{Context, Result};
use std::process::Command;

fn inject_key(evdev_code: u32) -> Result<()> {
    let status = Command::new("ydotool")
        .env("YDOTOOL_SOCKET", "/run/user/1000/.ydotool_socket")
        .arg("key")
        .arg(format!("{evdev_code}:1"))
        .arg(format!("{evdev_code}:0"))
        .status()
        .context("spawn ydotool")?;
    if !status.success() {
        anyhow::bail!("ydotool exited with {status}");
    }
    Ok(())
}

pub async fn run(target_index: usize) -> Result<()> {
    let windows = list_windows().await.context("list_windows")?;
    let target = windows
        .get(target_index)
        .context("target index out of range")?;
    println!("[combo] target: id={} title={:?}", target.id, target.title);

    // Snapshot current focus so we can restore it afterward.
    let prior = crate::kwin_windows::active_window_id().await.context("snapshot prior focus")?;
    println!("[combo] prior active window id = {prior:?}");

    println!("[combo] activating target...");
    activate_window(&target.id).await.context("activate target")?;

    println!("[combo] injecting 'a' (KEY_A=30) immediately...");
    inject_key(30)?;

    println!("[combo] restoring prior focus...");
    if let Some(prior_id) = prior {
        activate_window(&prior_id).await.context("restore prior focus")?;
    } else {
        println!("[combo] no prior window id captured (desktop/none was focused) -- nothing to restore.");
    }

    println!("[combo] done. Please check: (1) did 'a' land in the target window's title/text field, (2) did you see visible focus flicker, (3) is focus now back on whatever you had before.");

    Ok(())
}
