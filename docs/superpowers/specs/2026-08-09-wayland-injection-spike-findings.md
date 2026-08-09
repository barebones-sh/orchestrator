# Wayland Injection Spike — Findings

**Date:** August 9, 2026
**Environment tested:** Kubuntu 26.04 LTS, KDE Plasma 6.6.5, KWin (Wayland), `kglobalacceld` 6.6.5, `ydotool` 1.0.4, `ashpd` 0.13.13 / `zbus` 5.18.0 (from the spike binary), real hardware seat (`$XDG_SESSION_TYPE=wayland`), live human-in-the-loop verification for every phase (dialogs, focus, flicker — none of this is automatable in CI, per the design spec).
**Spike code:** `spike/wayland-injection-spike/` (own workspace, not a product member — see design spec §6).

This document is the source of truth `orchestrator-hotkey`, `orchestrator-window`, and `orchestrator-input`'s real Linux backends are written against. It confirms one expected outcome from the design spec and surfaces one **unexpected, more fundamental** finding that reshapes the hotkey backend.

---

## Headline results

| # | Question | Answer |
|---|---|---|
| 1 | Does the private `org.kde.kglobalaccel` D-Bus protocol let a third-party (non-KWin) app register a *working* global shortcut? | **No.** Registration bookkeeping succeeds, but KWin never grabs the physical key for a foreign component. This is a dead end, not a stretch-goal gap — the design spec's assumption that this step is the unproblematic baseline was wrong. |
| 2 | Does `org.freedesktop.portal.GlobalShortcuts` work as the replacement? | **Yes**, reliably, once two non-obvious setup steps are done (see below). |
| 3 | Can KWin scripting enumerate windows and activate one by ID? | **Yes**, reliably, via the `callDBus` bridge pattern (prior art: `kdotool`). |
| 4 | Is Wayland synthetic input injection (`ydotool`/uinput, `libei`/RemoteDesktop portal) targeted or global? | **Global**, as expected — it always lands wherever real compositor focus currently is, confirmed for both injection paths. |
| 5 | Does "activate target → inject → restore prior focus" actually work as the practical window-targeting mechanism? | **Yes**, cleanly: correct landing, visible flicker (expected), clean restoration of prior focus. |

---

## Phase 1: Global hotkey registration

### 1a. Raw `org.kde.kglobalaccel` D-Bus protocol — dead end for third-party apps

Followed the private protocol used internally by `KGlobalAccel` (KF6): `doRegister(actionId)` → `setShortcut(actionId, keys, flags)` → subscribe to `globalShortcutPressed`/`Released` signals broadcast on `org.kde.kglobalaccel.Component` at `/component/<componentUnique>`.

Registration bookkeeping worked immediately and was independently verified three ways:
- `allShortcutInfos` reflected the registered action and assigned key combo exactly as requested.
- `Component.isActive` — **stayed `false`** across every attempt, including with the registering D-Bus connection held open for the process's entire lifetime (ruling out "the connection must stay open" as the missing piece) and after explicitly calling `activateGlobalShortcutContext` (ruling that out too).
- **Isolating self-test (the decisive evidence):** calling `Component.invokeShortcut("test-hotkey-2")` on ourselves *did* produce a correctly-received `globalShortcutPressed` signal on our own subscription — proving the signal-broadcast/subscription plumbing is 100% functional. Across two separate real-world tests (genuine physical keypresses, ~20s listening windows each), **zero** `globalShortcutPressed` signals arrived from the actual key. Combined, this isolates the failure precisely: **KWin's compositor-level input layer does not route a physical keypress into kglobalaccel's shortcut-matching table for a component it doesn't own itself** (e.g. `kwin`, `plasmashell`). The raw D-Bus surface is apparently retained mostly for introspection/config-UI use (`allMainComponents`, `getComponent`, the "foreign" shortcut editing methods) rather than as a functioning third-party registration path on this Wayland setup.

Side finding, unrelated to the above but worth keeping as a permanent guardrail: our first test combo, **Ctrl+Alt+F12**, was intercepted by the kernel's virtual-terminal switch *below* the compositor and never reached kglobalaccel at all (switched VTs). **Never suggest or default to Ctrl+Alt+F1–F12** anywhere in the product (example profiles, docs, quick-start).

**Verdict: do not build `orchestrator-hotkey`'s KDE backend on the raw kglobalaccel protocol.**

### 1b. `org.freedesktop.portal.GlobalShortcuts` (ashpd) — works, two setup gotchas

Confirmed end-to-end: real physical **Ctrl+Alt+8** press → portal `Activated` signal received by the app, with correct `shortcut_id`.

Two non-obvious requirements for a non-sandboxed ("host", non-Flatpak) app to use *any* app-id-gated portal (this affects `GlobalShortcuts` **and** `RemoteDesktop`, see 3b):

1. **Call `org.freedesktop.host.portal.Registry.Register(app_id, {})` before using the portal**, on the *same* D-Bus connection the portal proxy itself uses. `zbus::Connection::session()` opens a brand-new connection (new unique name) on every call — it does not cache/share one — so the registration and the portal proxy must explicitly share one `Connection` (`GlobalShortcuts::with_connection(conn)` / `RemoteDesktop::with_connection(conn)`), otherwise the portal replies `NotAllowed: An app id is required` even after a successful `Register` call.
2. **The `app_id` must match an installed `.desktop` file** (checked via KDE's app-info lookup) or `Register` itself fails with `Could not register app ID: App info not found for '<id>'`. For the spike this meant creating a throwaway `~/.local/share/applications/org.orchestrator.Spike.desktop`. **For the real app, this becomes a packaging requirement, not an afterthought**: the `.deb` must install a `.desktop` file (e.g. `/usr/share/applications/<app-id>.desktop`) whose id matches exactly what `orchestrator-hotkey`'s KDE backend registers at runtime, or hotkey registration breaks on a machine where the app was built/run without its `.desktop` file installed (e.g. `cargo run` during development). **Action item for later increments:** ship a dev-mode `.desktop` file install step (or a clear README note) so `cargo run` during development doesn't silently fail this.

UX consequence worth designing around, not just a footnote: the *actual key combo* is chosen through **KDE's own native shortcut-assignment dialog** (triggered by `bind_shortcuts`), not through anything our app renders. The app supplies an id + human description ("Toggle profile: Clicker"); the portal returns a `trigger_description` string (e.g. `"Ctrl+Alt+8"`) for display. KDE remembers the assignment per (app_id, shortcut_id) — re-running `bind_shortcuts` with the same ids did **not** re-prompt. Rebinding an existing shortcut from within our own UI (not through System Settings) should use the portal's `ConfigureShortcuts` method (seen in introspection, not yet spike-tested — do this as part of the real backend work, it's a small follow-up, not a new unknown).

**Verdict: `orchestrator-hotkey`'s KDE backend must be built on `org.freedesktop.portal.GlobalShortcuts` via `ashpd`, not on raw `kglobalaccel`.** This is a bigger change from the design spec than anticipated: rename/rewrite `kde_kglobalaccel.rs` (the design spec's placeholder name) — call it e.g. `kde_portal_shortcuts.rs` — and its trait impl needs a `.desktop`-file-installed precondition documented and checked (see Permissions section of the top-level brief: "detect missing permissions and surface a specific, actionable error").

---

## Phase 2: Window enumeration and activation (KWin scripting D-Bus)

Confirmed reliable using the `callDBus` bridge pattern from `kdotool` (jinliu/kdotool), reimplemented directly in Rust/zbus per the design spec (not shelled out to):

1. Open our own session-bus connection, capture our unique name.
2. Generate a small KWin JS script that does the work (`workspace.windowList()`, `workspace.activeWindow`, `workspace.activeWindow = w`) and, at the end, calls `callDBus(ourUniqueName, "/", "", "result"|"error", payload)` — a fire-and-forget unicast method call *back into our own connection*, requiring no D-Bus match-rule registration (unicast calls addressed to you are delivered regardless).
3. Load it via `org.kde.kwin.Scripting.loadScript(path, name)` → get a script id → call `.run()`/`.stop()` on `/Scripting/Script<id>` (interface `org.kde.kwin.Script`) → `unloadScript(name)` when done.
4. Receive the callback by reading raw messages off our own connection (`MessageStream::from(&conn)`), filtering for `MethodCall` at path `/`.

Live-tested against a 6-window desktop (4 plasmashell surfaces, Firefox, VS Code): enumeration returned correct `internalId` (KWin's UUID — matches `Scope::Window.backend_hint_id`), `pid`, `resourceClass` (→ `process_name`), and `caption` (→ `window_title_hint`) for every window. Activation (`workspace.activeWindow = w`) was visually confirmed to raise and focus the target window.

One implementation note carried over from `kdotool`: KWin occasionally delivers `JSON.stringify` output double-encoded; parse defensively (try direct `serde_json::from_str`, fall back to un-stringifying once) exactly as `kdotool` does.

**Verdict: build `orchestrator-window`'s `kwin_dbus.rs` directly on this pattern** — `list_windows()` and `activate_window()` map straight onto the `WindowLocator` trait; `internalId` is the natural `WindowHandle`.

---

## Phase 3: Injection

### 3a. `ydotool`/uinput

Setup required: `hb` added to the `input` group (`sudo usermod -aG input hb` — takes effect on next login; a running process can pick it up immediately via `sg input -c '...'` without a full re-login, useful to know for a "just applied the udev/group change, does the app need a restart" support answer) and `ydotoold` running (ships as a disabled-by-default `ydotool.service` **user** unit on this distro; needs to be enabled, and needs to run *after* the group membership is active for the account it runs as, since it opens `/dev/uinput` itself at startup).

Injected `KEY_A` press+release via the `ydotool` CLI. Confirmed: landed wherever real compositor focus currently was (the user's active chat window at the time, *not* a window we'd activated minutes earlier and let focus drift away from) — i.e. **confirmed global/focus-following, not targetable on its own**, exactly per the design spec's prior.

### 3b. `libei` via `ashpd::desktop::remote_desktop::RemoteDesktop`

Same `Registry.Register` + shared-connection + `.desktop`-file requirements as 1b (this is also an app-id-gated portal). Flow: `create_session` → `select_devices(DeviceType::Keyboard)` → `start()` (pops a real KDE "share input control" permission dialog — confirmed the user must accept it; not yet determined whether/how this grant persists across process restarts, since the spike only ran within one session) → `notify_keyboard_keycode(session, keycode, state)`.

Confirmed: `start()` returned granted devices only after the user accepted the dialog; `notify_keyboard_keycode` then landed the key — also global/focus-following, same as 3a.

**Verdict on 3a vs. 3b:** both work and are equally "global." `ydotool` needs a system-level one-time setup (group + daemon) that's easy to document in the README as planned; the portal path needs a runtime permission dialog (arguably more "asks forgiveness at the right layer" but adds a dialog per install, and its persistence behavior is still unconfirmed). **Recommend `ydotool`/uinput as the primary Linux `InputInjector` backend** (matches the design spec's original lean, and avoids the open question about portal grant persistence across restarts), keeping the `ashpd::RemoteDesktop` path as a documented fallback/alternative rather than the default — revisit if uinput/group setup proves to be a support burden in practice.

### 3c. Activate → inject → restore (the real `Scope::Window` mechanism)

Combined phases 2 + 3a back-to-back (activate target, inject immediately, restore prior focus) with a genuinely different prior-focus window (not the target) so the test wasn't trivially self-confirming. Confirmed live by the human partner: **(1) visible focus flicker to the target and back — expected, not a bug; (2) the injected key landed correctly in the target window's text field; (3) focus was cleanly restored to the original window afterward.**

**Verdict: this is the real, working mechanism for `Scope::Window` profiles**, exactly as the design spec's fallback contract anticipated. Confirms: `requires_focus_steal_for_window_targeting() -> true` is correct and permanent for the Linux/Wayland backend; `focus_steal: true` must be always-on and explained in the UI per the original brief, not just a theoretical flag.

---

## Consequences for the design spec / next increment

1. **Rename/rewrite the KDE hotkey backend.** `orchestrator-hotkey/src/kde_kglobalaccel.rs` (as named in the design spec) should not be built on raw kglobalaccel D-Bus. Build it on `org.freedesktop.portal.GlobalShortcuts` via `ashpd` instead (suggest renaming the file `kde_portal_shortcuts.rs` when implementing, to avoid the name implying the wrong protocol).
2. **New packaging requirement:** a `.desktop` file with an app id matching what the hotkey/injection code registers must ship with the `.deb` and be installed for both `GlobalShortcuts` and (if used) `RemoteDesktop` to work at all. Document this in the README's Linux permissions section alongside the `input`-group/`ydotoold` setup, and make the app's own startup check detect "portal registration failed: app id not found" and surface it as an actionable error (per the brief's permissions requirement), not a generic failure.
3. **UX consequence:** the KDE backend cannot silently bind an arbitrary user-chosen key combo the way the brief's "trigger: a single user-chosen key combo" phrasing implies for a from-scratch picker widget. The real flow is: app requests a named shortcut slot via the portal → KDE's own dialog does the actual key assignment → app receives back a human-readable description. The GUI/CLI should present this as "click to assign in the system dialog" rather than building a custom key-capture widget for Linux. (macOS's real mechanism is a separate, not-yet-spiked question for a future increment.)
4. **`orchestrator-window`'s `kwin_dbus.rs`** can be written directly from Phase 2 above with high confidence — no unknowns remain.
5. **`orchestrator-input`'s `linux_wayland.rs`**: default to `ydotool`/uinput per the 3a/3b verdict; document the `RemoteDesktop`/libei path as an alternate constructor for later, not required for v1.
6. **Never use Ctrl+Alt+F1–F12** as a default/example/documentation hotkey anywhere in the product (kernel VT-switch collision, confirmed the hard way).
7. Add a small `ConfigureShortcuts`-based "rebind" spike/implementation note to the next increment's task list — not spike-tested here but low-risk, visible in introspection.
