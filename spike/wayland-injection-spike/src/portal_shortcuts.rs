//! Phase 1b: register a global shortcut via the `org.freedesktop.portal.GlobalShortcuts`
//! xdg-desktop-portal interface (`ashpd`), the modern compositor-agnostic replacement
//! for hand-rolling the private `org.kde.kglobalaccel` protocol (see kglobalaccel_raw.rs
//! for why that path is a dead end for foreign/non-KWin components on this system).

use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::Session;
use futures_util::StreamExt;
use std::collections::HashMap;

/// xdg-desktop-portal >= 1.20 requires non-sandboxed ("host") apps to declare an
/// app id via org.freedesktop.host.portal.Registry.Register before app-id-gated
/// portals (like GlobalShortcuts) will serve them -- otherwise: "NotAllowed: An
/// app id is required". Flatpak apps get this for free from the sandbox; we
/// don't have one, so we call it ourselves, matching what our real .desktop-file
/// app id will eventually be.
async fn register_host_app_id(conn: &zbus::Connection, app_id: &str) -> Result<()> {
    let options: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.host.portal.Registry"),
        "Register",
        &(app_id, options),
    )
    .await
    .context("Registry.Register")?;
    println!("[portal] registered host app id = {app_id:?}");
    Ok(())
}

pub async fn run() -> Result<()> {
    let conn = zbus::Connection::session().await.context("session bus")?;
    register_host_app_id(&conn, "org.orchestrator.Spike").await?;

    // Reuse the SAME connection for the portal proxy -- zbus::Connection::session()
    // opens a brand new connection (unique name) every call, no sharing/caching.
    // Registry.Register associates the app id with the calling connection's unique
    // name, so a fresh connection here would be unregistered again.
    let proxy = GlobalShortcuts::with_connection(conn)
        .await
        .context("connect to GlobalShortcuts portal")?;
    println!("[portal] connected to org.freedesktop.portal.GlobalShortcuts");

    let session: Session<GlobalShortcuts> = proxy
        .create_session(Default::default())
        .await
        .context("create_session")?;
    println!("[portal] session created");

    let shortcut = NewShortcut::new("test-hotkey", "Orchestrator spike test hotkey");

    println!("[portal] calling bind_shortcuts -- a KDE system dialog may appear now asking you to assign/confirm a key combo. Please handle it (assign something memorable, e.g. Ctrl+Alt+9) and accept.");

    let request = proxy
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await
        .context("bind_shortcuts request")?;
    let response = request.response().context("bind_shortcuts response")?;
    println!("[portal] bind_shortcuts response: {:?}", response.shortcuts());

    println!("[portal] Listening for Activated/Deactivated signals for 90s -- press whatever combo you assigned in the dialog.");

    let mut activated = proxy.receive_activated().await.context("receive_activated")?;
    let mut deactivated = proxy.receive_deactivated().await.context("receive_deactivated")?;

    let timeout = tokio::time::sleep(std::time::Duration::from_secs(90));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                println!("[portal] timed out waiting for activation.");
                break;
            }
            Some(sig) = activated.next() => {
                println!("[portal] SIGNAL Activated: shortcut_id={:?} timestamp={:?}", sig.shortcut_id(), sig.timestamp());
                println!("[portal] SUCCESS: portal-based global shortcut fired.");
                break;
            }
            Some(sig) = deactivated.next() => {
                println!("[portal] SIGNAL Deactivated: shortcut_id={:?} timestamp={:?}", sig.shortcut_id(), sig.timestamp());
            }
        }
    }

    Ok(())
}
