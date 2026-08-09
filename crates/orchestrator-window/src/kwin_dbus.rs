//! Real KDE/Wayland window-enumeration/activation backend, built on KWin's
//! scripting D-Bus interface (`org.kde.kwin.Scripting` / `org.kde.kwin.Script`).
//! This is the confirmed-working mechanism from the Wayland injection spike;
//! see
//! `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`
//! (Phase 2) for the full story.
//!
//! KWin scripts run in a sandboxed JS engine with no direct return-value
//! channel back to the D-Bus caller -- `loadScript`/`run` are fire-and-forget
//! -- so the confirmed workaround (prior art: `kdotool`, jinliu/kdotool) has
//! the script call back into a D-Bus method on the *client's own* connection
//! via the `callDBus(service, path, interface, member, ...args)` global
//! KWin-scripting function, addressed at the client's own unique bus name.
//! The client listens for that inbound (unicast, no match-rule needed)
//! method call on its own connection. The mechanics below are ported
//! directly from the spike's `spike/wayland-injection-spike/src/kwin_windows.rs`
//! (confirmed reliable live against a real 6-window KDE Plasma 6.6.5/Wayland
//! desktop: enumeration returned correct ids/pids/classes/titles for every
//! window, activation visually confirmed to raise and focus the target) into
//! this crate's `WindowLocator` trait shape, with `anyhow::Error` replaced by
//! this crate's `WindowError`.
//!
//! **Not re-verified here:** live enumeration/activation against a real KWin
//! session is NOT re-run by this module's tests -- that part requires a live
//! D-Bus session and a running KWin instance, neither of which is
//! automatable in this environment. It was already spike-verified per the
//! findings doc linked above; a human-in-the-loop pass happens after a later
//! task wires this backend into a runnable binary. What *is* tested here
//! (see the `tests` module) is the pure JSON-parsing helper, including its
//! double-encoded-payload fallback path, against fixed string fixtures.

use std::io::Write;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use zbus::message::Type as MessageType;
use zbus::{Connection, MessageStream};

use crate::error::WindowError;
use crate::trait_def::WindowLocator;
use crate::vocab::{WindowHandle, WindowInfo};

/// Real KDE/Wayland `WindowLocator`, built on KWin's scripting D-Bus
/// interface. See the module doc comment for the mechanism.
pub struct KwinWindowLocator {
    conn: Connection,
}

impl KwinWindowLocator {
    /// Open the session-bus connection used by all trait methods below.
    pub async fn new() -> Result<Self, WindowError> {
        let conn = Connection::session().await.map_err(map_err_msg)?;
        Ok(Self { conn })
    }
}

/// Raw shape of a single window entry as produced by the KWin JS side of
/// `list_windows`'s script. Mapped onto the crate's `WindowInfo` vocab type
/// by [`raw_to_window_info`].
#[derive(Debug, Deserialize)]
struct RawWindowInfo {
    id: String,
    title: String,
    class_name: String,
    pid: u32,
}

/// Map any D-Bus/zbus/(de)serialization failure into an actionable
/// `WindowError::BackendUnavailable`. This crate has no dedicated D-Bus
/// error variant (see `error.rs`), so all such failures collapse into this
/// one, carrying the original message.
fn map_err_msg(err: impl std::fmt::Display) -> WindowError {
    WindowError::BackendUnavailable(err.to_string())
}

/// Run an ad-hoc KWin script that ends by calling back
/// `callDBus(dbus_addr, "/", "", "result"|"error", payload)`, and return the
/// `result` payload (or an error built from an `error` callback). Ported
/// near-verbatim from the spike's `run_kwin_script`, with `anyhow::Error`
/// replaced by `WindowError`.
async fn run_kwin_script(conn: &Connection, js_body_template: &str) -> Result<String, WindowError> {
    let dbus_addr = conn
        .unique_name()
        .ok_or_else(|| map_err_msg("connection has no unique name"))?
        .to_string();

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

    let mut file = tempfile::NamedTempFile::with_prefix("orchestrator-window-")?;
    file.write_all(script.as_bytes())?;
    let path = file.into_temp_path();
    let script_name = format!("orchestrator-window-{}", std::process::id());

    let path_str = path
        .to_str()
        .ok_or_else(|| map_err_msg("script tempfile path is not valid UTF-8"))?;

    let script_id: i32 = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "loadScript",
            &(path_str, script_name.as_str()),
        )
        .await
        .map_err(map_err_msg)?
        .body()
        .deserialize()
        .map_err(map_err_msg)?;
    if script_id < 0 {
        return Err(WindowError::BackendUnavailable(format!(
            "loadScript returned {script_id} (a script with this name may already be loaded)"
        )));
    }

    let script_path = format!("/Scripting/Script{script_id}");

    // Start listening BEFORE run(), since callDBus fires as soon as the
    // script body executes -- no reply is expected/sent back to KWin
    // (fire-and-forget).
    let mut stream = MessageStream::from(conn);

    conn.call_method(
        Some("org.kde.KWin"),
        script_path.as_str(),
        Some("org.kde.kwin.Script"),
        "run",
        &(),
    )
    .await
    .map_err(map_err_msg)?;

    let result = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = stream.next().await {
            let msg = msg.map_err(|e| map_err_msg(format!("message stream error: {e}")))?;
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
                "error" => {
                    return Err(WindowError::BackendUnavailable(format!(
                        "KWin script error: {payload}"
                    )))
                }
                _ => continue,
            }
        }
        Err(map_err_msg("message stream ended unexpectedly"))
    })
    .await
    .map_err(|_| map_err_msg("timed out waiting for KWin script callback"))?;

    // Best-effort cleanup regardless of success/failure above.
    let _ = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "unloadScript",
            &(script_name.as_str(),),
        )
        .await;

    result
}

/// KWin sometimes delivers `JSON.stringify` output double-encoded (observed
/// by `kdotool` too) -- try direct parse first, then fall back to
/// un-stringifying once. Pure/testable; see the `tests` module for fixtures
/// covering both paths.
fn parse_json_payload<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, WindowError> {
    serde_json::from_str(payload).or_else(|_| {
        let unescaped: String = serde_json::from_str(payload)
            .map_err(|e| map_err_msg(format!("failed to unescape payload: {e}")))?;
        serde_json::from_str(&unescaped)
            .map_err(|e| map_err_msg(format!("failed to parse unescaped payload: {e}")))
    })
}

/// Map KWin's raw JS-side JSON shape onto the crate's `WindowInfo` vocab
/// type. `resourceClass` -> `process_name`, empty string -> `None` (a window
/// can have no resource class, e.g. some Plasma popups/dialogs). Pure/
/// testable, used by `list_windows`.
fn raw_to_window_info(raw: RawWindowInfo) -> WindowInfo {
    WindowInfo {
        handle: WindowHandle(raw.id),
        pid: Some(raw.pid),
        process_name: if raw.class_name.is_empty() {
            None
        } else {
            Some(raw.class_name)
        },
        title: raw.title,
    }
}

/// Interpret `focused_window`'s script payload: an empty string means no
/// window is currently focused (e.g. focus on the desktop/nothing). Pure/
/// testable, used by `focused_window`.
fn map_focused_payload(payload: String) -> Option<WindowHandle> {
    if payload.is_empty() {
        None
    } else {
        Some(WindowHandle(payload))
    }
}

/// Interpret `activate_window`'s script payload: `"not_found"` means the
/// script didn't find a window with the given handle; anything else (the
/// script's `"activated"`) is success. Pure/testable, used by
/// `activate_window`.
fn map_activate_payload(payload: &str) -> Result<(), WindowError> {
    if payload == "not_found" {
        Err(WindowError::NotFound)
    } else {
        Ok(())
    }
}

impl WindowLocator for KwinWindowLocator {
    /// The spike's `list_windows`, ported verbatim (script body, parse
    /// helper) with field mapping to `WindowInfo` via [`raw_to_window_info`].
    async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError> {
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
        let payload = run_kwin_script(&self.conn, js).await?;
        let raw: Vec<RawWindowInfo> = parse_json_payload(&payload)?;
        Ok(raw.into_iter().map(raw_to_window_info).collect())
    }

    /// The spike's `active_window_id`, wrapping the result in `WindowHandle`.
    async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError> {
        let js = r#"
    var w = workspace.activeWindow;
    output_result(w ? w.internalId.toString() : "");
"#;
        let payload = run_kwin_script(&self.conn, js).await?;
        Ok(map_focused_payload(payload))
    }

    /// The spike's `activate_window`, verbatim, returning
    /// `WindowError::NotFound` when the script reports `"not_found"`.
    async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError> {
        let target_id = &handle.0;
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
        let payload = run_kwin_script(&self.conn, &js).await?;
        map_activate_payload(&payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_json_payload -------------------------------------------------
    //
    // No D-Bus/KWin instance is used or required by any test in this module
    // -- see the module doc comment's "Not re-verified here" note. These
    // fixtures are fixed strings standing in for what KWin's JS side could
    // plausibly hand back over `callDBus`.

    #[test]
    fn parses_directly_encoded_json() {
        let payload = r#"[{"id":"abc-123","title":"Firefox","class_name":"firefox","pid":4242}]"#;
        let parsed: Vec<RawWindowInfo> = parse_json_payload(payload).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "abc-123");
        assert_eq!(parsed[0].title, "Firefox");
        assert_eq!(parsed[0].class_name, "firefox");
        assert_eq!(parsed[0].pid, 4242);
    }

    #[test]
    fn falls_back_to_unescaping_double_encoded_json() {
        // KWin sometimes hands back JSON.stringify output that has been
        // stringified twice -- i.e. the payload itself is a JSON string
        // literal whose *contents* are the real JSON. Build exactly that
        // shape here (serde_json::to_string on a JSON-array string produces
        // a JSON string literal containing the escaped array).
        let inner = r#"[{"id":"xyz-789","title":"VS Code","class_name":"code","pid":99}]"#;
        let double_encoded = serde_json::to_string(inner).unwrap();

        // A direct parse of the double-encoded payload as Vec<RawWindowInfo>
        // must fail (it's a JSON string, not an array) -- otherwise this
        // test wouldn't actually be exercising the fallback path.
        assert!(serde_json::from_str::<Vec<RawWindowInfo>>(&double_encoded).is_err());

        let parsed: Vec<RawWindowInfo> = parse_json_payload(&double_encoded).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "xyz-789");
        assert_eq!(parsed[0].title, "VS Code");
        assert_eq!(parsed[0].class_name, "code");
        assert_eq!(parsed[0].pid, 99);
    }

    #[test]
    fn reports_backend_unavailable_on_unparseable_payload() {
        let result: Result<Vec<RawWindowInfo>, WindowError> = parse_json_payload("not json at all");
        match result {
            Err(WindowError::BackendUnavailable(_)) => {}
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    // -- raw_to_window_info ---------------------------------------------------

    #[test]
    fn maps_empty_class_name_to_none() {
        let raw = RawWindowInfo {
            id: "id-1".to_string(),
            title: "Some Popup".to_string(),
            class_name: String::new(),
            pid: 10,
        };
        let info = raw_to_window_info(raw);
        assert_eq!(info.handle, WindowHandle("id-1".to_string()));
        assert_eq!(info.pid, Some(10));
        assert_eq!(info.process_name, None);
        assert_eq!(info.title, "Some Popup");
    }

    #[test]
    fn maps_present_class_name_to_some() {
        let raw = RawWindowInfo {
            id: "id-2".to_string(),
            title: "Mozilla Firefox".to_string(),
            class_name: "firefox".to_string(),
            pid: 4242,
        };
        let info = raw_to_window_info(raw);
        assert_eq!(info.process_name, Some("firefox".to_string()));
    }

    // -- map_focused_payload / map_activate_payload ---------------------------

    #[test]
    fn focused_payload_empty_string_is_none() {
        assert_eq!(map_focused_payload(String::new()), None);
    }

    #[test]
    fn focused_payload_non_empty_wraps_handle() {
        assert_eq!(
            map_focused_payload("some-id".to_string()),
            Some(WindowHandle("some-id".to_string()))
        );
    }

    #[test]
    fn activate_payload_not_found_maps_to_not_found_error() {
        let result = map_activate_payload("not_found");
        assert!(matches!(result, Err(WindowError::NotFound)));
    }

    #[test]
    fn activate_payload_activated_is_ok() {
        assert!(map_activate_payload("activated").is_ok());
    }
}
