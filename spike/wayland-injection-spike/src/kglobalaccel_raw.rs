//! Phase 1a: register a global shortcut via the raw `org.kde.kglobalaccel` D-Bus
//! protocol (private/undocumented, but this is what KWin itself and KDE Frameworks'
//! `KGlobalAccel` C++ class use under the hood).
//!
//! Key finding from manual `busctl` probing before writing this: the registering
//! D-Bus connection must stay open for the shortcut to remain "active"
//! (`org.kde.kglobalaccel.Component.isActive`). Each `busctl call` opens a
//! short-lived connection, so the shortcut deregistered itself immediately after
//! setup. A real client (this one) holds one connection for its whole run.

use anyhow::{Context, Result};
use futures_util::stream::StreamExt;
use zbus::Connection;

const COMPONENT_UNIQUE: &str = "orchestrator-spike";
const COMPONENT_FRIENDLY: &str = "Orchestrator Spike";
const ACTION_UNIQUE: &str = "test-hotkey-2";
const ACTION_FRIENDLY: &str = "Test Hotkey 2";

// Qt::Key_9 (0x39) | Qt::ControlModifier (0x04000000) | Qt::AltModifier (0x08000000)
// Chosen specifically to avoid the Ctrl+Alt+F1..F12 range, which the kernel's VT
// switch grabs *below* the compositor -- confirmed the hard way during this spike
// (Ctrl+Alt+F12 switched virtual consoles instead of reaching kglobalaccel at all).
const TEST_KEY_COMBO: i32 = 0x04000000 + 0x08000000 + 0x39;

pub async fn run() -> Result<()> {
    let conn = Connection::session().await.context("connect to session bus")?;
    println!("[kglobalaccel] connected, unique name = {:?}", conn.unique_name());

    let action_id = vec![
        COMPONENT_UNIQUE.to_string(),
        ACTION_UNIQUE.to_string(),
        COMPONENT_FRIENDLY.to_string(),
        ACTION_FRIENDLY.to_string(),
    ];

    // doRegister(as) -- register the action so kglobalaccel knows about it.
    conn.call_method(
        Some("org.kde.kglobalaccel"),
        "/kglobalaccel",
        Some("org.kde.KGlobalAccel"),
        "doRegister",
        &(action_id.clone(),),
    )
    .await
    .context("doRegister")?;
    println!("[kglobalaccel] doRegister ok");

    // setShortcut(as, ai, u) -> ai (actually-assigned keys)
    let assigned: (Vec<i32>,) = conn
        .call_method(
            Some("org.kde.kglobalaccel"),
            "/kglobalaccel",
            Some("org.kde.KGlobalAccel"),
            "setShortcut",
            &(action_id.clone(), vec![TEST_KEY_COMBO], 1u32),
        )
        .await
        .context("setShortcut")?
        .body()
        .deserialize()
        .context("deserialize setShortcut reply")?;
    println!("[kglobalaccel] setShortcut assigned = {:?} (requested {:?})", assigned.0, TEST_KEY_COMBO);

    let component_path = format!("/component/{}", COMPONENT_UNIQUE.replace('-', "_"));

    // Try explicitly activating the "default" shortcut context for our component --
    // kglobalaccel supports multiple named contexts per component (e.g. per-window
    // context-sensitive shortcuts) and only one may be active at a time. Our
    // shortcut was registered under context "default" (per allShortcutInfos), so
    // test the hypothesis that it needs to be made the active context explicitly.
    match conn
        .call_method(
            Some("org.kde.kglobalaccel"),
            "/kglobalaccel",
            Some("org.kde.KGlobalAccel"),
            "activateGlobalShortcutContext",
            &(COMPONENT_UNIQUE, "default"),
        )
        .await
    {
        Ok(_) => println!("[kglobalaccel] activateGlobalShortcutContext ok"),
        Err(e) => println!("[kglobalaccel] activateGlobalShortcutContext failed: {e}"),
    }

    let is_active: bool = conn
        .call_method(
            Some("org.kde.kglobalaccel"),
            component_path.as_str(),
            Some("org.kde.kglobalaccel.Component"),
            "isActive",
            &(),
        )
        .await
        .context("isActive")?
        .body()
        .deserialize()
        .context("deserialize isActive reply")?;
    println!(
        "[kglobalaccel] isActive = {is_active} (expect true now -- connection is held open, unlike the busctl probe)"
    );

    println!(
        "[kglobalaccel] Listening for Ctrl+Alt+9 on {component_path} (org.kde.kglobalaccel.Component.globalShortcutPressed/Released). Press it now, or wait 20s to time out."
    );

    // Self-test in parallel: after 3s, call invokeShortcut() on ourselves. This
    // isolates "does our signal subscription plumbing work at all" from "does the
    // compositor actually route a *physical* keypress to kglobalaccel for a
    // non-KWin-owned component" -- two very different failure points.
    {
        let conn = conn.clone();
        let component_path = component_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            println!("[kglobalaccel] self-test: calling invokeShortcut(\"{ACTION_UNIQUE}\") now...");
            match conn
                .call_method(
                    Some("org.kde.kglobalaccel"),
                    component_path.as_str(),
                    Some("org.kde.kglobalaccel.Component"),
                    "invokeShortcut",
                    &(ACTION_UNIQUE,),
                )
                .await
            {
                Ok(_) => println!("[kglobalaccel] self-test: invokeShortcut call returned OK"),
                Err(e) => println!("[kglobalaccel] self-test: invokeShortcut call failed: {e}"),
            }
        });
    }

    // Subscribe generically to the whole interface: the signal could be Pressed,
    // Released, or Repeated and we want to see all three.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.kde.kglobalaccel")?
        .path(component_path.as_str())?
        .interface("org.kde.kglobalaccel.Component")?
        .build();

    use zbus::MessageStream;
    let mut stream = MessageStream::for_match_rule(rule, &conn, None)
        .await
        .context("subscribe to Component signals")?;

    let timeout = tokio::time::sleep(std::time::Duration::from_secs(20));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                println!("[kglobalaccel] timed out waiting for a press -- no signal observed.");
                break;
            }
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.context("message stream error")?;
                let header = msg.header();
                if header.path().map(|p| p.as_str()) == Some(component_path.as_str())
                    && header.interface().map(|i| i.as_str()) == Some("org.kde.kglobalaccel.Component")
                {
                    let member = header.member().map(|m| m.as_str()).unwrap_or("?");
                    let body: (String, String, i64) = msg.body().deserialize().unwrap_or_default();
                    println!("[kglobalaccel] SIGNAL {member} component={} action={} timestamp={}", body.0, body.1, body.2);
                    if member == "globalShortcutPressed" {
                        println!("[kglobalaccel] SUCCESS: press landed while connection held open.");
                        break;
                    }
                }
            }
        }
    }

    // NOTE: deliberately not calling Component.cleanUp() here anymore -- keep the
    // registration alive so `busctl` probing in between spike iterations can
    // inspect its state. Clean up manually via busctl when done experimenting.

    Ok(())
}
