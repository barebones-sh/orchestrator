//! Real KDE/Wayland global-hotkey backend, built on the
//! `org.freedesktop.portal.GlobalShortcuts` xdg-desktop-portal interface via
//! `ashpd`. This is the confirmed-working mechanism from the Wayland
//! injection spike; see
//! `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`
//! (sections 1a/1b) for the full story of *why*: the private,
//! internally-used `org.kde.kglobalaccel` D-Bus protocol is a dead end for
//! third-party apps (registration bookkeeping succeeds, but KWin never
//! routes a physical keypress into it for a non-KWin-owned component -- an
//! isolated self-test proved the signal plumbing itself works, ruling that
//! out as the failure point). The mechanics below are ported directly from
//! the spike's `spike/wayland-injection-spike/src/portal_shortcuts.rs`
//! (confirmed end-to-end against a real KDE Plasma 6.6.5/Wayland session)
//! into this crate's trait-based shape.
//!
//! Two non-obvious setup steps, both required for a non-sandboxed ("host")
//! app to use this (or any other app-id-gated) portal, and both explained in
//! the findings doc:
//!
//! 1. `org.freedesktop.host.portal.Registry.Register(app_id, {})` must be
//!    called on the *same* `zbus::Connection` the portal proxy itself uses.
//!    `zbus::Connection::session()` opens a brand-new connection (new unique
//!    name) on every call -- it does not cache or share one -- so a fresh
//!    connection for the portal proxy would be unregistered again, and the
//!    portal replies `NotAllowed: An app id is required` even after a
//!    successful `Register` call.
//! 2. `app_id` must match an installed `.desktop` file (checked via KDE's
//!    app-info lookup), or `Register` itself fails with `Could not register
//!    app ID: App info not found for '<id>'`. This is a packaging
//!    requirement for the shipped `.deb` (a `.desktop` file must be
//!    installed with a matching id), not just a runtime nicety -- see the
//!    findings doc's "Consequences" section.
//!
//! The portal, not this backend, decides the actual key combo: `register`
//! only requests a named shortcut slot (`bind_shortcuts`); KDE's own native
//! shortcut-assignment dialog performs the actual key assignment, and the
//! portal hands back a human-readable `trigger_description` string (e.g.
//! `"Ctrl+Alt+8"`) that this module best-effort-parses back into a
//! `KeyCombo` for display purposes only -- see [`parse_trigger_description`].
//! `HotkeyBackend::register`'s `preferred` parameter is therefore ignored by
//! this backend.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::Session;
use futures_util::StreamExt;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::HotkeyError;
use crate::trait_def::HotkeyBackend;
use crate::vocab::{HotkeyEvent, HotkeyId, KeyCombo, Modifier};

/// KDE/Wayland global-hotkey backend built on
/// `org.freedesktop.portal.GlobalShortcuts`. See the module doc comment for
/// the mechanism and its two setup preconditions.
pub struct KdePortalHotkeyBackend {
    proxy: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
    /// All shortcuts requested so far, action_name alongside the opaque
    /// `NewShortcut` value. `NewShortcut` (the *request* type) doesn't
    /// expose an id accessor -- only `Shortcut` (the *response* type) does
    /// -- so we track the action_name ourselves for later removal.
    ///
    /// This is a `tokio::sync::Mutex`, not a `std::sync::Mutex`, and
    /// deliberately so: both `register` and `unregister` hold the guard
    /// across their entire read-modify-write-then-`bind_shortcuts` critical
    /// section, including the `.await` on the portal round trip. `register`
    /// and `unregister` take `&self`, so concurrent calls are possible (e.g.
    /// registering several profiles' hotkeys concurrently); without this,
    /// two interleaved calls could each snapshot the list before either
    /// `bind_shortcuts` response came back, and whichever response was
    /// processed last would silently overwrite the other's binding. Holding
    /// the lock across the `.await` serializes them instead. A
    /// `std::sync::Mutex` guard can't soundly span an `.await` point, hence
    /// `tokio::sync::Mutex` here.
    shortcuts: AsyncMutex<Vec<(String, NewShortcut)>>,
    /// action_name -> minted `HotkeyId`. Also used by the forwarding task to
    /// translate `Activated`/`Deactivated` signals' `shortcut_id` (which we
    /// set to the action_name, see `register`) into a `HotkeyId`.
    ids: Arc<Mutex<HashMap<String, HotkeyId>>>,
    next_id: AtomicU64,
    /// Ids explicitly unregistered. It is not yet confirmed whether the
    /// portal's `bind_shortcuts`, called again with a reduced list, actually
    /// drops the KDE-side binding (see findings doc / `unregister`'s doc
    /// comment) -- so the forwarding task also always checks this set as an
    /// unconditional local backstop, regardless of what the portal did.
    suppressed: Arc<Mutex<HashSet<HotkeyId>>>,
    /// Handed out by the first `subscribe()` call; `None` after that (see
    /// `subscribe`'s doc comment for why a second call doesn't panic).
    event_rx: Mutex<Option<std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)>>>,
    /// Keeps the background forwarding task alive for the backend's
    /// lifetime. Never polled directly. Note that dropping a
    /// `JoinHandle` on its own does *not* cancel the task -- per Tokio's
    /// documented semantics it only detaches it, leaving it (and the D-Bus
    /// connection its captured streams hold) running in the background
    /// indefinitely. The `Drop` impl below calls `.abort()` on this handle
    /// explicitly to actually stop the task when the backend is dropped.
    forward_task: tokio::task::JoinHandle<()>,
}

impl KdePortalHotkeyBackend {
    /// Construct the backend. In order:
    /// 1. Open one `zbus::Connection::session()`.
    /// 2. Call `org.freedesktop.host.portal.Registry.Register(app_id, {})`
    ///    on that connection.
    /// 3. Build the `GlobalShortcuts` proxy on the *same* connection.
    /// 4. `create_session`.
    /// 5. Spawn the background forwarding task for the backend's lifetime.
    ///
    /// `app_id` must match an installed `.desktop` file (see module docs).
    ///
    /// Not part of the `HotkeyBackend` trait: a real caller uses this at
    /// startup, before the trait-generic runner takes over.
    pub async fn new(app_id: impl Into<String>) -> Result<Self, HotkeyError> {
        let app_id = app_id.into();

        // Steps 1-2: one shared connection, Registry.Register on it.
        let conn = zbus::Connection::session().await.map_err(map_portal_err)?;
        register_host_app_id(&conn, &app_id).await?;

        // Step 3: the SAME connection as step 2, not a fresh one -- see
        // module docs point 1.
        let proxy = GlobalShortcuts::with_connection(conn)
            .await
            .map_err(map_portal_err)?;

        // Step 4.
        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(map_portal_err)?;

        // Step 5: background forwarding task, bridging the async signal
        // streams into the sync mpsc channel `subscribe()` hands out.
        let activated = proxy.receive_activated().await.map_err(map_portal_err)?;
        let deactivated = proxy.receive_deactivated().await.map_err(map_portal_err)?;

        let ids: Arc<Mutex<HashMap<String, HotkeyId>>> = Arc::new(Mutex::new(HashMap::new()));
        let suppressed: Arc<Mutex<HashSet<HotkeyId>>> = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = std::sync::mpsc::channel();

        let task_ids = Arc::clone(&ids);
        let task_suppressed = Arc::clone(&suppressed);
        let forward_task = tokio::spawn(async move {
            tokio::pin!(activated);
            tokio::pin!(deactivated);
            loop {
                tokio::select! {
                    signal = activated.next() => {
                        match signal {
                            Some(sig) => forward_event(
                                &task_ids,
                                &task_suppressed,
                                &tx,
                                sig.shortcut_id(),
                                HotkeyEvent::Pressed,
                            ),
                            None => break,
                        }
                    }
                    signal = deactivated.next() => {
                        match signal {
                            Some(sig) => forward_event(
                                &task_ids,
                                &task_suppressed,
                                &tx,
                                sig.shortcut_id(),
                                HotkeyEvent::Released,
                            ),
                            None => break,
                        }
                    }
                }
            }
        });

        Ok(Self {
            proxy,
            session,
            shortcuts: AsyncMutex::new(Vec::new()),
            ids,
            next_id: AtomicU64::new(1),
            suppressed,
            event_rx: Mutex::new(Some(rx)),
            forward_task,
        })
    }
}

impl Drop for KdePortalHotkeyBackend {
    /// A bare `JoinHandle` drop only detaches the spawned task rather than
    /// cancelling it (see `forward_task`'s doc comment) -- abort it
    /// explicitly so the background signal-forwarding task actually stops
    /// when the backend is dropped, instead of continuing to run detached.
    fn drop(&mut self) {
        self.forward_task.abort();
    }
}

/// Look up the `HotkeyId` for an incoming signal's `shortcut_id` and forward
/// `(id, event)` on the sync channel, unless the id has been unregistered
/// (see `suppressed`'s doc comment) or is otherwise unknown to us.
fn forward_event(
    ids: &Mutex<HashMap<String, HotkeyId>>,
    suppressed: &Mutex<HashSet<HotkeyId>>,
    tx: &std::sync::mpsc::Sender<(HotkeyId, HotkeyEvent)>,
    shortcut_id: &str,
    event: HotkeyEvent,
) {
    let Some(id) = ids.lock().unwrap().get(shortcut_id).cloned() else {
        return;
    };
    if suppressed.lock().unwrap().contains(&id) {
        return;
    }
    let _ = tx.send((id, event));
}

/// xdg-desktop-portal >= 1.20 requires non-sandboxed ("host") apps to
/// declare an app id via `org.freedesktop.host.portal.Registry.Register`
/// before app-id-gated portals (like `GlobalShortcuts`) will serve them.
/// Flatpak apps get this for free from the sandbox; we don't have one.
async fn register_host_app_id(conn: &zbus::Connection, app_id: &str) -> Result<(), HotkeyError> {
    let options: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.host.portal.Registry"),
        "Register",
        &(app_id, options),
    )
    .await
    .map_err(map_portal_err)?;
    Ok(())
}

/// Map the two portal/zbus failure modes actually observed in the spike into
/// an actionable `HotkeyError::BackendUnavailable`, quoting the findings
/// doc's remedy. Anything else is passed through with its `Display` text
/// intact rather than swallowed.
fn map_portal_err(err: impl std::fmt::Display) -> HotkeyError {
    map_portal_err_message(&err.to_string())
}

fn map_portal_err_message(msg: &str) -> HotkeyError {
    if msg.contains("An app id is required") {
        HotkeyError::BackendUnavailable(format!(
            "portal rejected the request ({msg}): the app-id registration and the \
             GlobalShortcuts proxy must share one zbus::Connection -- \
             zbus::Connection::session() opens a new connection every call and does not \
             cache/share one. See the shared-connection step in \
             docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md section 1b."
        ))
    } else if msg.contains("App info not found") {
        HotkeyError::BackendUnavailable(format!(
            "portal rejected the app id ({msg}): it must match an installed .desktop file \
             (e.g. /usr/share/applications/<app_id>.desktop). See \
             docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md section 1b."
        ))
    } else {
        HotkeyError::BackendUnavailable(msg.to_string())
    }
}

/// Best-effort, display-only parse of a portal `trigger_description` (e.g.
/// `"Ctrl+Alt+8"`) back into a `KeyCombo`. This is never load-bearing for
/// actually triggering the hotkey -- the portal/KWin already own that -- so
/// on anything unparseable it falls back to a modifier-less `KeyCombo`
/// carrying the original string as the key, rather than erroring.
fn parse_trigger_description(s: &str) -> KeyCombo {
    let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
    match tokens.split_last() {
        Some((key, mods)) if !key.is_empty() => {
            let modifiers = mods
                .iter()
                .filter_map(|m| match m.to_lowercase().as_str() {
                    "ctrl" | "control" => Some(Modifier::Ctrl),
                    "shift" => Some(Modifier::Shift),
                    "alt" => Some(Modifier::Alt),
                    "meta" | "super" | "win" => Some(Modifier::Meta),
                    _ => None,
                })
                .collect();
            KeyCombo {
                modifiers,
                key: (*key).to_string(),
            }
        }
        _ => KeyCombo {
            modifiers: Vec::new(),
            key: s.to_string(),
        },
    }
}

/// Find the action_name currently mapped to `id`, if any. Pure/testable
/// bookkeeping used by `unregister`.
fn find_action_by_id(ids: &HashMap<String, HotkeyId>, id: &HotkeyId) -> Option<String> {
    ids.iter().find(|(_, v)| *v == id).map(|(k, _)| k.clone())
}

/// Drop the entry for `action_name` from the accumulated shortcuts list.
/// Pure/testable bookkeeping used by `unregister`.
fn remove_action(shortcuts: &mut Vec<(String, NewShortcut)>, action_name: &str) {
    shortcuts.retain(|(name, _)| name != action_name);
}

impl HotkeyBackend for KdePortalHotkeyBackend {
    /// `preferred` is ignored -- see the module doc comment: the portal's
    /// own dialog assigns the actual combo. Appends a new shortcut slot to
    /// the accumulated list and re-`bind_shortcuts`es the *full* list every
    /// time (not just the new entry), which is correct whether or not the
    /// portal's `bind_shortcuts` appends or replaces per call.
    ///
    /// Holds `self.shortcuts`'s lock across the whole mutate-then-
    /// `bind_shortcuts` sequence (see that field's doc comment) so a
    /// concurrent `register`/`unregister` call can't race on a stale
    /// snapshot and silently overwrite this registration.
    async fn register(
        &self,
        action_name: &str,
        _preferred: Option<&KeyCombo>,
    ) -> Result<(HotkeyId, KeyCombo), HotkeyError> {
        let mut shortcuts = self.shortcuts.lock().await;
        shortcuts.push((
            action_name.to_string(),
            NewShortcut::new(action_name, action_name),
        ));
        let snapshot: Vec<NewShortcut> = shortcuts.iter().map(|(_, s)| s.clone()).collect();

        // From here on, any early return must first undo the push above --
        // otherwise a failed registration (portal error, user cancelling
        // KDE's assignment dialog, or a malformed response) leaves a dead
        // entry in `shortcuts` forever: it has no corresponding `ids` entry
        // for `unregister` to ever find and remove, and it gets re-sent on
        // every future `bind_shortcuts` call. Combined with `register` having
        // no duplicate-registration guard, a natural retry after a failure
        // would then push a *second* entry for the same action_name,
        // compounding the leak. `shortcuts` is still locked here, so
        // `remove_action` is race-free with any concurrent caller.
        let request = match self
            .proxy
            .bind_shortcuts(&self.session, &snapshot, None, Default::default())
            .await
        {
            Ok(r) => r,
            Err(e) => {
                remove_action(&mut shortcuts, action_name);
                return Err(map_portal_err(e));
            }
        };
        let response = match request.response() {
            Ok(r) => r,
            Err(e) => {
                remove_action(&mut shortcuts, action_name);
                return Err(map_portal_err(e));
            }
        };

        let bound = match response.shortcuts().iter().find(|s| s.id() == action_name) {
            Some(b) => b,
            None => {
                remove_action(&mut shortcuts, action_name);
                return Err(HotkeyError::RegistrationFailed(format!(
                    "portal bind_shortcuts response did not include an entry for action {action_name:?}"
                )));
            }
        };
        let combo = parse_trigger_description(bound.trigger_description());

        let id = HotkeyId(self.next_id.fetch_add(1, Ordering::SeqCst));
        self.ids
            .lock()
            .unwrap()
            .insert(action_name.to_string(), id.clone());

        Ok((id, combo))
    }

    /// Removes the shortcut both from the portal (re-`bind_shortcuts` with
    /// the reduced list) and from local bookkeeping (`suppressed`), since it
    /// is not yet confirmed whether the portal round-trip alone actually
    /// drops the KDE-side binding -- see the findings doc and `suppressed`'s
    /// doc comment. Both exist deliberately: the portal call is the "real"
    /// unregistration if it works, and the local filter is a backstop that
    /// works regardless.
    ///
    /// Like `register`, holds `self.shortcuts`'s lock across the whole
    /// mutate-then-`bind_shortcuts` sequence, so it can't race a concurrent
    /// `register`/`unregister` call (see that field's doc comment).
    async fn unregister(&self, id: HotkeyId) -> Result<(), HotkeyError> {
        self.suppressed.lock().unwrap().insert(id.clone());

        let action_name = {
            let mut ids = self.ids.lock().unwrap();
            let found = find_action_by_id(&ids, &id);
            if let Some(name) = &found {
                ids.remove(name);
            }
            found
        };

        let Some(action_name) = action_name else {
            // Unknown id: nothing was registered under it, already a no-op.
            return Ok(());
        };

        let mut shortcuts = self.shortcuts.lock().await;
        remove_action(&mut shortcuts, &action_name);
        let snapshot: Vec<NewShortcut> = shortcuts.iter().map(|(_, s)| s.clone()).collect();

        self.proxy
            .bind_shortcuts(&self.session, &snapshot, None, Default::default())
            .await
            .map_err(map_portal_err)?;

        Ok(())
    }

    /// Returns the receiver paired with the background forwarding task
    /// spawned in `new()`. A second call cannot hand out the same receiver
    /// again (it was moved out), so it returns an already-closed receiver
    /// instead of panicking -- matching the macOS stub's documented pattern.
    fn subscribe(&self) -> std::sync::mpsc::Receiver<(HotkeyId, HotkeyEvent)> {
        let mut slot = self.event_rx.lock().unwrap();
        if let Some(rx) = slot.take() {
            return rx;
        }
        let (_tx, rx) = std::sync::mpsc::channel();
        rx
    }

    fn backend_name(&self) -> &'static str {
        "kde-portal-global-shortcuts"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_trigger_description --------------------------------------

    #[test]
    fn parses_plain_key_no_modifiers() {
        let combo = parse_trigger_description("F9");
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![],
                key: "F9".to_string(),
            }
        );
    }

    #[test]
    fn parses_one_modifier() {
        let combo = parse_trigger_description("Ctrl+F9");
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            }
        );
    }

    #[test]
    fn parses_multiple_modifiers_and_is_case_insensitive() {
        let combo = parse_trigger_description("ctrl+ALT+8");
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![Modifier::Ctrl, Modifier::Alt],
                key: "8".to_string(),
            }
        );
    }

    #[test]
    fn recognizes_all_modifier_spellings() {
        let combo = parse_trigger_description("control+shift+alt+super+Z");
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![
                    Modifier::Ctrl,
                    Modifier::Shift,
                    Modifier::Alt,
                    Modifier::Meta,
                ],
                key: "Z".to_string(),
            }
        );
    }

    #[test]
    fn falls_back_on_empty_string() {
        let combo = parse_trigger_description("");
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![],
                key: "".to_string(),
            }
        );
    }

    #[test]
    fn preserves_key_casing() {
        let combo = parse_trigger_description("Ctrl+Return");
        assert_eq!(combo.key, "Return");
    }

    // -- map_portal_err_message ------------------------------------------

    #[test]
    fn maps_app_id_required_error() {
        let err =
            map_portal_err_message("Portal request failed: NotAllowed: An app id is required");
        match err {
            HotkeyError::BackendUnavailable(msg) => {
                assert!(msg.contains("shared-connection") || msg.contains("share one zbus"));
                assert!(msg.contains("wayland-injection-spike-findings.md"));
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn maps_app_info_not_found_error() {
        let err = map_portal_err_message(
            "Could not register app ID: App info not found for 'org.orchestrator.App'",
        );
        match err {
            HotkeyError::BackendUnavailable(msg) => {
                assert!(msg.contains(".desktop file"));
                assert!(msg.contains("wayland-injection-spike-findings.md"));
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn passes_through_unknown_errors_without_swallowing() {
        let err = map_portal_err_message("some other zbus failure: connection reset");
        match err {
            HotkeyError::BackendUnavailable(msg) => {
                assert!(msg.contains("some other zbus failure: connection reset"));
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    // -- internal bookkeeping ---------------------------------------------

    #[test]
    fn find_action_by_id_finds_registered_id() {
        let mut ids = HashMap::new();
        ids.insert("clicker-toggle".to_string(), HotkeyId(1));
        ids.insert("recorder-toggle".to_string(), HotkeyId(2));

        assert_eq!(
            find_action_by_id(&ids, &HotkeyId(2)),
            Some("recorder-toggle".to_string())
        );
    }

    #[test]
    fn find_action_by_id_returns_none_for_unknown_id() {
        let ids = HashMap::new();
        assert_eq!(find_action_by_id(&ids, &HotkeyId(99)), None);
    }

    #[test]
    fn remove_action_drops_only_the_matching_entry() {
        let mut shortcuts = vec![
            ("a".to_string(), NewShortcut::new("a", "a")),
            ("b".to_string(), NewShortcut::new("b", "b")),
        ];
        remove_action(&mut shortcuts, "a");
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].0, "b");
    }

    #[test]
    fn remove_action_is_a_no_op_for_unknown_action() {
        let mut shortcuts = vec![("a".to_string(), NewShortcut::new("a", "a"))];
        remove_action(&mut shortcuts, "does-not-exist");
        assert_eq!(shortcuts.len(), 1);
    }

    #[test]
    fn register_style_push_then_failure_leaves_no_trace() {
        // `register()` itself can't be exercised without a live portal
        // session (see the "lifecycle / concurrency mechanisms" note
        // below), but its push-then-on-error-`remove_action` shape (the
        // Finding 2 fix) is exactly this: push a new entry, then on ANY
        // failure path (bind_shortcuts erroring, or the response missing the
        // expected action) call `remove_action` before returning the error.
        // Model that shape directly against the real `remove_action` used by
        // the fix, proving a failed "registration" leaves `shortcuts`
        // byte-for-byte as it was before the attempt -- so a retry starts
        // clean instead of accumulating dead entries.
        let mut shortcuts: Vec<(String, NewShortcut)> = vec![(
            "existing-action".to_string(),
            NewShortcut::new("existing-action", "existing-action"),
        )];
        let before = shortcuts.len();

        let action_name = "new-action";
        shortcuts.push((
            action_name.to_string(),
            NewShortcut::new(action_name, action_name),
        ));
        assert_eq!(shortcuts.len(), before + 1);

        // Simulate any of register()'s error paths (bind_shortcuts failing,
        // or the response not containing the action) by immediately
        // rolling back via remove_action, exactly as the fixed code does.
        remove_action(&mut shortcuts, action_name);

        assert_eq!(
            shortcuts.len(),
            before,
            "a failed registration must not leave a dead entry behind"
        );
        assert!(shortcuts.iter().all(|(name, _)| name != action_name));

        // A subsequent retry with the same action_name must be able to push
        // exactly one fresh entry, not stack a second dead one on top.
        shortcuts.push((
            action_name.to_string(),
            NewShortcut::new(action_name, action_name),
        ));
        assert_eq!(shortcuts.len(), before + 1);
    }

    // -- lifecycle / concurrency mechanisms -------------------------------
    //
    // Neither `KdePortalHotkeyBackend::new` nor the trait methods can be
    // exercised directly in a unit test without a live D-Bus session (see
    // the module doc comment and the task report). The two tests below
    // instead directly verify the underlying tokio mechanisms the Drop impl
    // and the register/unregister locking pattern rely on, using the exact
    // same primitives (`JoinHandle::abort`, a `tokio::sync::Mutex` guard
    // held across an `.await`) rather than duplicating unverifiable D-Bus
    // logic.

    #[tokio::test]
    async fn abort_stops_a_running_background_task() {
        // Mirrors what `forward_task` would do if left unaborted: loop
        // forever. A bare `JoinHandle` drop would only detach this (per
        // Tokio's documented semantics) -- it keeps running until something
        // calls `.abort()` on it, which is exactly why `KdePortalHotkeyBackend`
        // has an explicit `Drop` impl instead of relying on the field drop.
        let handle = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });

        tokio::task::yield_now().await;
        assert!(
            !handle.is_finished(),
            "the loop should still be running before any abort"
        );

        handle.abort();
        let result = handle.await;
        assert!(
            result.unwrap_err().is_cancelled(),
            "abort() must actually stop the task, not just detach it"
        );
    }

    #[tokio::test]
    async fn async_mutex_serializes_critical_section_across_await() {
        // Same shape as register()/unregister(): lock a tokio::sync::Mutex,
        // mutate, then `.await` (standing in for the bind_shortcuts round
        // trip) *before* releasing the guard. Proves concurrent callers
        // never observe/mutate the shared state mid-critical-section, which
        // is what prevents the lost-update race the fix addresses.
        use std::sync::atomic::AtomicBool;

        let shortcuts: AsyncMutex<Vec<i32>> = AsyncMutex::new(Vec::new());
        let in_critical_section = AtomicBool::new(false);
        let overlap_detected = AtomicBool::new(false);

        async fn register_like(
            shortcuts: &AsyncMutex<Vec<i32>>,
            in_critical_section: &AtomicBool,
            overlap_detected: &AtomicBool,
            value: i32,
        ) {
            let mut guard = shortcuts.lock().await;
            if in_critical_section.swap(true, Ordering::SeqCst) {
                overlap_detected.store(true, Ordering::SeqCst);
            }
            guard.push(value);
            // Stands in for the `bind_shortcuts` `.await` -- the guard is
            // still held here, unlike the pre-fix code.
            tokio::task::yield_now().await;
            in_critical_section.store(false, Ordering::SeqCst);
        }

        tokio::join!(
            register_like(&shortcuts, &in_critical_section, &overlap_detected, 1),
            register_like(&shortcuts, &in_critical_section, &overlap_detected, 2),
            register_like(&shortcuts, &in_critical_section, &overlap_detected, 3),
        );

        assert!(
            !overlap_detected.load(Ordering::SeqCst),
            "two callers were inside the critical section at once"
        );
        let final_state = shortcuts.lock().await;
        assert_eq!(
            final_state.len(),
            3,
            "no concurrent registration should be lost"
        );
    }
}
