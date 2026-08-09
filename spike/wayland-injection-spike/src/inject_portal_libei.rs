//! Phase 3b: inject a keypress via `libei` through the `org.freedesktop.portal.RemoteDesktop`
//! xdg-desktop-portal (ashpd), the other injection candidate named in the spike plan
//! alongside ydotool. Uses the same Registry.Register host-app-id workaround as
//! portal_shortcuts.rs since this is also an app-id-gated portal.

use anyhow::{Context, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::enumflags2::BitFlags;
use ashpd::desktop::Session;
use std::collections::HashMap;

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
    println!("[libei] registered host app id = {app_id:?}");
    Ok(())
}

pub async fn run() -> Result<()> {
    let conn = zbus::Connection::session().await.context("session bus")?;
    register_host_app_id(&conn, "org.orchestrator.Spike").await?;

    let proxy = RemoteDesktop::with_connection(conn)
        .await
        .context("connect to RemoteDesktop portal")?;
    println!("[libei] connected to org.freedesktop.portal.RemoteDesktop");

    let session: Session<RemoteDesktop> = proxy
        .create_session(Default::default())
        .await
        .context("create_session")?;
    println!("[libei] session created");

    let select_request = proxy
        .select_devices(
            &session,
            ashpd::desktop::remote_desktop::SelectDevicesOptions::default()
                .set_devices(BitFlags::from(DeviceType::Keyboard)),
        )
        .await
        .context("select_devices request")?;
    select_request.response().context("select_devices response")?;
    println!("[libei] select_devices ok (Keyboard)");

    println!("[libei] calling start() -- a KDE permission dialog should appear now asking to share input control. Please accept it.");
    let start_request = proxy
        .start(&session, None, Default::default())
        .await
        .context("start request")?;
    let devices = start_request.response().context("start response")?;
    println!("[libei] start ok, granted devices: {:?}", devices.devices());

    println!("[libei] injecting KEY_A (30) press+release via notify_keyboard_keycode...");
    proxy
        .notify_keyboard_keycode(&session, 30, KeyState::Pressed, Default::default())
        .await
        .context("notify_keyboard_keycode press")?;
    proxy
        .notify_keyboard_keycode(&session, 30, KeyState::Released, Default::default())
        .await
        .context("notify_keyboard_keycode release")?;
    println!("[libei] injected. Check wherever focus currently is for a landed 'a'.");

    Ok(())
}
