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
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// KWin's `Window.pid` is a JS `int` and can be `-1` when unknown, or
    /// (depending on the JS engine's JSON encoding) absent entirely. Kept as
    /// a signed, optional field here -- unlike the final public
    /// `WindowInfo::pid: Option<u32>` -- so that one window with no pid
    /// fails to deserialize *that field* rather than failing
    /// `parse_json_payload::<Vec<RawWindowInfo>>` (and therefore
    /// `list_windows()`) for the entire desktop. See [`raw_to_window_info`]
    /// for the negative/missing -> `None` mapping.
    #[serde(default)]
    pid: Option<i64>,
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
    // Unique per invocation (process id + a monotonically increasing
    // counter), not just the process id: a fixed per-process name means
    // that if a script is ever left loaded in KWin (e.g. cleanup failing to
    // run -- see the timeout handling below), the *next* call would collide
    // with it and `loadScript` would return a negative id, permanently
    // bricking the backend until the process restarts. A unique name per
    // call sidesteps that collision even if a previous script's cleanup
    // didn't happen for some other reason.
    static SCRIPT_COUNTER: AtomicU64 = AtomicU64::new(0);
    let script_name = format!(
        "orchestrator-window-{}-{}",
        std::process::id(),
        SCRIPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

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

    // Bind the timeout's own `Result` (timed-out-or-not) to a local instead
    // of an early `?` here -- the `unloadScript` cleanup below must run
    // unconditionally, including on timeout. An early return on timeout
    // would skip it, leaving the script loaded in KWin under `script_name`
    // forever (see this function's doc comment / the module's Finding 1
    // fix); the outer `Result<Result<..>, Elapsed>` is flattened into the
    // original error *after* cleanup has already run.
    let outcome: Result<Result<String, WindowError>, tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(5), async {
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
        .await;

    // Best-effort cleanup regardless of success/failure/timeout above --
    // this now genuinely runs unconditionally, matching the comment.
    let _ = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "unloadScript",
            &(script_name.as_str(),),
        )
        .await;

    outcome.map_err(|_| map_err_msg("timed out waiting for KWin script callback"))?
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
        // Negative (KWin's "unknown", e.g. -1) or missing pid -> None,
        // rather than failing the parse -- see `RawWindowInfo::pid`'s doc
        // comment.
        pid: raw.pid.and_then(|p| u32::try_from(p).ok()),
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

/// Build the KWin JS body for `activate_window`, given the target window's
/// handle. Pure/testable, used by `activate_window`.
///
/// `target_id` is JSON-encoded (via `serde_json::to_string`, which correctly
/// escapes quotes/backslashes/control characters) rather than raw-formatted
/// inside manually-written quote characters, so it cannot break out of the
/// generated string literal. This matters because `WindowHandle` values are
/// no longer guaranteed to originate only from KWin's own UUIDs:
/// `orchestrator-core`'s `Scope::Window::backend_hint_id` field holds
/// exactly this kind of value and is loaded from user-editable JSON config
/// on disk, so a hand-edited/malformed config already has a schema'd path to
/// an arbitrary string reaching this interpolation site.
fn build_activate_script(target_id: &str) -> String {
    // `serde_json::to_string` on a `&str` cannot fail.
    let encoded_target_id =
        serde_json::to_string(target_id).expect("string serialization is infallible");
    format!(
        r#"
    var t = workspace.windowList();
    var found = false;
    var target = {encoded_target_id};
    for (var i = 0; i < t.length; i++) {{
        var w = t[i];
        if (w.internalId.toString() == target) {{
            workspace.activeWindow = w;
            found = true;
            break;
        }}
    }}
    output_result(found ? "activated" : "not_found");
"#
    )
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

    /// The spike's `activate_window`, adapted to build its script body via
    /// [`build_activate_script`] (JSON-encoded interpolation of the target
    /// handle -- see that function's doc comment), returning
    /// `WindowError::NotFound` when the script reports `"not_found"`.
    async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError> {
        let js = build_activate_script(&handle.0);
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
        assert_eq!(parsed[0].pid, Some(4242));
    }

    #[test]
    fn parses_negative_pid_without_failing_the_whole_array() {
        // KWin's `Window.pid` is a JS `int` and can be `-1` when unknown
        // (see `RawWindowInfo::pid`'s doc comment) -- one such window must
        // not fail deserialization of the surrounding array/desktop.
        let payload = r#"[
            {"id":"abc-123","title":"Firefox","class_name":"firefox","pid":4242},
            {"id":"def-456","title":"Unknown","class_name":"","pid":-1}
        ]"#;
        let parsed: Vec<RawWindowInfo> = parse_json_payload(payload).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].pid, Some(4242));
        assert_eq!(parsed[1].pid, Some(-1));
        assert_eq!(
            raw_to_window_info(parsed.into_iter().nth(1).unwrap()).pid,
            None
        );
    }

    #[test]
    fn parses_missing_pid_field_without_failing_the_whole_array() {
        // Absent `pid` key entirely (not just null/-1) must also degrade
        // gracefully via #[serde(default)] rather than failing the parse.
        let payload = r#"[{"id":"ghi-789","title":"No Pid","class_name":""}]"#;
        let parsed: Vec<RawWindowInfo> = parse_json_payload(payload).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].pid, None);
        assert_eq!(
            raw_to_window_info(parsed.into_iter().next().unwrap()).pid,
            None
        );
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
        assert_eq!(parsed[0].pid, Some(99));
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
            pid: Some(10),
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
            pid: Some(4242),
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

    // -- build_activate_script -------------------------------------------

    #[test]
    fn build_activate_script_embeds_plain_handle_as_a_string_literal() {
        let js = build_activate_script("abc-123");
        assert!(js.contains(r#"var target = "abc-123";"#));
        assert!(js.contains(r#"if (w.internalId.toString() == target)"#));
    }

    #[test]
    fn build_activate_script_escapes_a_quote_in_the_handle() {
        // A handle containing a `"` must not be able to break out of the
        // generated string literal -- this is exactly the injection Finding
        // 4 addresses. Assert the generated *script text* is well-formed
        // (the quote appears escaped, not raw) without needing a live D-Bus
        // round trip.
        let malicious = r#"" + (workspace.activeWindow = null) + ""#;
        let js = build_activate_script(malicious);

        // The raw, unescaped payload must never appear verbatim in the
        // output -- if it did, the quote would have broken out of the
        // string literal.
        assert!(!js.contains(&format!("var target = \"{malicious}\";")));

        // The value must instead show up as a single JSON-encoded string
        // literal assigned to `target`, with all embedded quotes escaped.
        let expected_literal = serde_json::to_string(malicious).unwrap();
        assert!(js.contains(&format!("var target = {expected_literal};")));

        // Sanity-check the generated script is syntactically well-formed JS
        // by ensuring the number of unescaped (non-`\"`) double quotes on
        // the `var target = ...;` line is exactly two (the literal's own
        // opening/closing quotes).
        let target_line = js
            .lines()
            .find(|l| l.trim_start().starts_with("var target ="))
            .expect("generated script must contain a `var target = ...;` line");
        let mut chars = target_line.chars().peekable();
        let mut unescaped_quotes = 0;
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
            } else if c == '"' {
                unescaped_quotes += 1;
            }
        }
        assert_eq!(
            unescaped_quotes, 2,
            "expected exactly the literal's own opening/closing quotes, got line: {target_line}"
        );
    }

    #[test]
    fn build_activate_script_handles_backslashes_and_control_chars() {
        let tricky = "back\\slash\nand\ttab";
        let js = build_activate_script(tricky);
        let expected_literal = serde_json::to_string(tricky).unwrap();
        assert!(js.contains(&format!("var target = {expected_literal};")));
    }
}
