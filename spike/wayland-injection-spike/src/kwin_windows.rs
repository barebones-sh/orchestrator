//! Phase 2: enumerate + activate windows via KWin's scripting D-Bus interface.
//!
//! KWin scripts run in a sandboxed JS engine with no direct return-value channel
//! back to the D-Bus caller -- `loadScript`/`run` are fire-and-forget. The
//! standard workaround (confirmed via `kdotool`, jinliu/kdotool, prior art named
//! in the design spec) is: the script calls back into a D-Bus method on the
//! *client's own* connection via the `callDBus(service, path, interface, member,
//! ...args)` global KWin-scripting function, addressed at the client's own unique
//! bus name. The client listens for that inbound (unicast, no match-rule needed)
//! method call on its own connection.

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use std::io::Write;
use std::time::Duration;
use zbus::message::Type as MessageType;
use zbus::{Connection, MessageStream};

#[derive(Debug, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub class_name: String,
    pub pid: u32,
}

/// Run an ad-hoc KWin script that ends by calling back
/// `callDBus(dbus_addr, "/", "", "result"|"error", payload)`, and return the
/// `result` payload (or an error built from an `error` callback).
async fn run_kwin_script(js_body_template: &str) -> Result<String> {
    let conn = Connection::session().await.context("session bus")?;
    let dbus_addr = conn.unique_name().context("no unique name")?.to_string();
    println!("[kwin] our unique name (callback target) = {dbus_addr}");

    let script = format!(
        r#"
function output_result(message) {{
    callDBus("{dbus_addr}", "/", "", "result", message.toString());
}}
function output_error(message) {{
    callDBus("{dbus_addr}", "/", "", "error", message.toString());
}}
try {{
{js_body_template}
}} catch (e) {{
    output_error("exception: " + e);
}}
"#
    );

    let mut file = tempfile::NamedTempFile::with_prefix("orchestrator-spike-").context("tempfile")?;
    file.write_all(script.as_bytes()).context("write script")?;
    let path = file.into_temp_path();
    let script_name = format!("orchestrator-spike-{}", std::process::id());

    let script_id: i32 = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "loadScript",
            &(path.to_str().context("non-utf8 path")?, script_name.as_str()),
        )
        .await
        .context("loadScript")?
        .body()
        .deserialize()
        .context("deserialize loadScript reply")?;
    if script_id < 0 {
        return Err(anyhow!(
            "loadScript returned {script_id} (a script with this name may already be loaded)"
        ));
    }
    println!("[kwin] loaded script, id = {script_id}");

    let script_path = format!("/Scripting/Script{script_id}");

    // Start listening BEFORE run(), since callDBus fires as soon as the script
    // body executes -- no reply is expected/sent back to KWin (fire-and-forget).
    let mut stream = MessageStream::from(&conn);

    conn.call_method(
        Some("org.kde.KWin"),
        script_path.as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    )
    .await
    .context("Script.run")?;

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = stream.next().await {
            let msg = msg.context("message stream error")?;
            let header = msg.header();
            if header.message_type() != MessageType::MethodCall {
                continue;
            }
            if header.path().map(|p| p.as_str()) != Some("/") {
                continue;
            }
            let member = header.member().map(|m| m.as_str()).unwrap_or("");
            let payload: String = match msg.body().deserialize() {
                Ok(p) => p,
                Err(_) => continue,
            };
            match member {
                "result" => return Ok(payload),
                "error" => return Err(anyhow!("KWin script error: {payload}")),
                _ => continue,
            }
        }
        Err(anyhow!("message stream ended unexpectedly"))
    })
    .await
    .context("timed out waiting for KWin script callback")?;

    // Best-effort cleanup regardless of success/failure above.
    let _: Result<(), _> = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "unloadScript",
            &(script_name.as_str(),),
        )
        .await
        .map(|_| ());

    result
}

/// KWin sometimes delivers `JSON.stringify` output double-encoded (observed by
/// kdotool too) -- try direct parse first, then fall back to un-stringifying.
fn parse_json_payload<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T> {
    serde_json::from_str(payload).or_else(|_| {
        let unescaped: String =
            serde_json::from_str(payload).context("failed to unescape payload")?;
        serde_json::from_str(&unescaped).context("failed to parse unescaped payload")
    })
}

pub async fn list_windows() -> Result<Vec<WindowInfo>> {
    let js = r#"
    var t = workspace.windowList();
    var results = [];
    for (var i = 0; i < t.length; i++) {
        var w = t[i];
        results.push({
            id: w.internalId.toString(),
            title: w.caption,
            class_name: w.resourceClass ? w.resourceClass.toString() : "",
            pid: w.pid
        });
    }
    output_result(JSON.stringify(results));
"#;
    let payload = run_kwin_script(js).await?;
    parse_json_payload(&payload)
}

/// Returns the internalId of the currently-active (focused) window, or None if
/// no window is active (e.g. focus is on the desktop/nothing).
pub async fn active_window_id() -> Result<Option<String>> {
    let js = r#"
    var w = workspace.activeWindow;
    output_result(w ? w.internalId.toString() : "");
"#;
    let payload = run_kwin_script(js).await?;
    if payload.is_empty() {
        Ok(None)
    } else {
        Ok(Some(payload))
    }
}

pub async fn activate_window(target_id: &str) -> Result<()> {
    let js = format!(
        r#"
    var t = workspace.windowList();
    var found = false;
    for (var i = 0; i < t.length; i++) {{
        var w = t[i];
        if (w.internalId.toString() == "{target_id}") {{
            workspace.activeWindow = w;
            found = true;
            break;
        }}
    }}
    output_result(found ? "activated" : "not_found");
"#
    );
    let payload = run_kwin_script(&js).await?;
    if payload == "not_found" {
        return Err(anyhow!("window {target_id} not found"));
    }
    Ok(())
}

pub async fn run() -> Result<()> {
    let windows = list_windows().await.context("list_windows")?;
    println!("[kwin] found {} windows:", windows.len());
    for (i, w) in windows.iter().enumerate() {
        println!(
            "  [{i}] id={} pid={} class={:?} title={:?}",
            w.id, w.pid, w.class_name, w.title
        );
    }

    if windows.is_empty() {
        println!("[kwin] no windows to activate -- open something and rerun.");
        return Ok(());
    }

    print!("Enter index to activate (or leave blank to skip): ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let idx: usize = line.parse().context("not a number")?;
    let target = windows.get(idx).context("index out of range")?;
    println!("[kwin] activating id={} title={:?}", target.id, target.title);
    activate_window(&target.id).await?;
    println!("[kwin] activate_window call completed -- check which window is now focused.");

    Ok(())
}
