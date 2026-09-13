# Orchestrator GUI — Live Control Design Specification

## 1. Problem Statement

The GUI profile-management increment (`docs/superpowers/specs/2026-09-13-gui-design.md`)
deliberately deferred this piece: "embeds `orchestrator-core`'s `Runner`
directly in the Tauri Rust backend (mirroring `orchestrator-cli`'s
`linux_run`), adds start/stop/status commands, and a system tray icon so
closing the window doesn't kill a running session." That spec's §2 left the
shape undesigned, for a reason stated there: "its shape depends on
decisions made [in the profile-management increment]."

This spec covers that piece: running Orchestrator's automation live from
the GUI, without requiring `orchestrator run` in a separate terminal or the
systemd service from the `autostart` increment.

## 2. A Necessary Correction: There Is No "Set a Keybind" UI, Anywhere

Before designing this increment, it's worth stating a fact about the whole
app (CLI included, not something this increment introduces or can fix):
**neither `orchestrator-cli` nor `orchestrator-gui` has ever had a way to
assign or display a profile's actual hotkey.** `Profile.trigger` exists in
the data model but is populated with an empty placeholder
(`profile_ops::placeholder_trigger()`) at creation and never updated
afterward. `Runner::run()` (`crates/orchestrator-core/src/runner.rs:141-153`)
confirms this at the source level: it calls
`self.hotkey.register(&profile.name, None)` — passing `None`, not
`profile.trigger`, as the preferred combo — and discards the `KeyCombo` the
registration returns (`let (id, _combo) = ...`). The real key combo lives
only inside KDE's own `kglobalaccel`/System Settings → Shortcuts, assigned
via KDE's native dialog the first time an app registers a given action name
via the `GlobalShortcuts` portal — a deliberate security boundary from the
portal design (confirmed during this project's original Wayland spike), not
something Orchestrator can observe or bypass.

Practical consequence for this increment: the GUI **cannot** show a
per-profile "unassigned" badge or any other keybind status — there is no
data source for it. What it *can* do is explain, once, near the action that
actually triggers registration (clicking Start), that KDE will prompt for a
key combo the first time each profile's action name is registered, and
point to where to change it afterward. See §6.

## 3. Runner Lifecycle in the Tauri Backend

A new `linux_runner` module in `src-tauri/src/lib.rs` (or a separate file
if `lib.rs` is getting large by then — implementer's call), gated
`#[cfg(target_os = "linux")]` with a non-Linux stub, mirroring the existing
`list_windows` split (`src-tauri/src/lib.rs:87-98`).

**State**, held in Tauri managed state (`app.manage(...)`):

```rust
enum RunnerState {
    Idle,
    Running { stop: std::sync::Arc<tokio::sync::Notify> },
    Error(String),
}

struct RunnerHandle {
    state: std::sync::Mutex<RunnerState>,
}
```

No separate "Starting" state: `Runner::run()` registers every profile's
hotkey and then enters its event loop in one `.await` with no observable
midpoint, so a GUI-level "starting" state would be fictional busywork, not
real backend information. `start_runner()` spawns the run and returns
immediately, setting state to `Running` optimistically; if registration
actually fails (a real error, or — per §2 — the user cancelling a KDE
assignment dialog, which the earlier `kde_portal_shortcuts` work already
established surfaces as a portal error), the spawned task resolves with an
`Err`, and state flips to `Error` with the message, emitted as an event
(§5). If the user is slow answering KDE's dialog for a never-before-seen
profile, the GUI just shows "Running" while KDE's own native dialog sits on
top asking for a key combo — self-explanatory in the moment, and exactly
the behavior `orchestrator run` already has from the terminal.

**Commands** (`#[tauri::command]`, registered in `generate_handler!`
alongside the five existing ones):

- `start_runner() -> Result<(), String>` — if state is already `Running`,
  return `Err("already running")` rather than spawning a second `Runner`
  (two `Runner`s over the same config would double-register every hotkey).
  Otherwise: read `config.json` fresh (same "load once, at start" semantics
  `orchestrator run` already has — edits made while running need a
  restart, matching the CLI exactly, not a new inconsistency this
  increment introduces), construct the real `KdePortalHotkeyBackend`/
  `YdotoolInputInjector`/`KwinWindowLocator` (identical construction to
  `orchestrator-cli/src/main.rs`'s `linux_run` module). This module needs
  its own `APP_ID` constant, copied **verbatim** from
  `orchestrator-cli/src/main.rs`'s (`"io.github.barebonessh.Orchestrator"`)
  — not a GUI-specific id. This is a deliberate, load-bearing choice, not
  an oversight to fix later: the `.deb` package ships exactly one
  `.desktop` file (`io.github.barebonessh.Orchestrator.desktop`), and the
  portal validates an app-id against an installed `.desktop` file with a
  matching id (the exact constraint this project's original spike
  discovered and the packaging increment built around). A GUI-specific
  app-id would have no matching `.desktop` file and fail registration
  outright. Sharing the id also means profiles registered from the CLI and
  the GUI share the same KDE shortcut assignments for the same profile
  name — one consistent set of keybinds regardless of which front-end last
  ran them, which is the correct behavior given both read/write the same
  `config.json`. Build a `Runner`, create a fresh `Notify`, spawn
  `tokio::spawn(async move { runner.run(notify.notified()).await })`, set
  state to `Running`, emit the status event.
- `stop_runner() -> Result<(), String>` — if state is `Running`, call
  `stop.notify_one()` and set state to `Idle` (optimistically — the
  spawned task's teardown, per `Runner::run`'s own documented
  `spawn_blocking`-abort caveat, may leave one bounded background OS
  thread alive briefly; this is `Runner`'s own accepted, already-documented
  limitation, not something this increment needs to solve). Otherwise
  `Err("not running")`.
- `runner_status() -> RunnerStatusDto` where
  `RunnerStatusDto = { state: "idle" | "running" | "error", error: Option<String> }`
  — a synchronous read of the current state, for the frontend's initial
  load (before any event has fired).

**Event**: `"runner-status-changed"`, payload `RunnerStatusDto` (same
shape), emitted by `start_runner`/`stop_runner` and by the spawned task
when it resolves with an error. The frontend listens via
`@tauri-apps/api/event`'s `listen()` rather than polling.

**Cargo.toml**: `src-tauri/Cargo.toml`'s Linux dependency block needs
`orchestrator-core`'s `runner` feature added alongside the existing `kde`/
`wayland` (`features = ["kde", "wayland", "runner"]`) — this pulls in
`tokio`/`tracing` as `orchestrator-core` optional deps, already proven safe
(the CLI does the same). `tauri`'s feature list needs `tray-icon` added
(currently `features = []`) for §4.

## 4. System Tray

Tauri v2's `TrayIconBuilder` (`tauri::tray`), confirmed against the current
Tauri v2 docs (not assumed from older training data): build a `Menu`
(`tauri::menu::{Menu, MenuItem}`) with items `Start`/`Stop` (a single item
whose text/id toggles based on current state — rebuild the menu on each
status-changed event rather than trying to mutate one item in place, since
`Menu`/`MenuItem` construction is cheap and this avoids a stale-menu-text
class of bug), `Show window`, `Quit`. Attach via `on_menu_event`, reusing
the same `start_runner`/`stop_runner` logic the Tauri commands call (factor
the actual start/stop logic into plain Rust functions taking the
`AppHandle`/managed state, called from both the `#[tauri::command]`
wrappers and the tray's menu-event handler — no logic duplication between
the two entry points). Reuse the icon Task 1 already generated
(`src-tauri/icons/32x32.png` or similar — implementer's call on which
generated size fits the tray).

**Window close behavior**: register a `WindowEvent::CloseRequested`
handler on the main window that calls `api.prevent_close()` and
`window.hide()` instead of letting the app quit — this is the actual reason
the tray exists (per the original profile-management spec's own framing:
"so closing the window doesn't kill a running session"). `Quit` in the tray
menu is the only way to actually exit, calling `app.exit(0)`.

## 5. Frontend

A status section in `+page.svelte` (or a new `RunnerStatus.svelte`
component, implementer's call on whether it's substantial enough to
warrant its own file — follow `ProfileList.svelte`/`ProfileForm.svelte`'s
existing precedent for that judgment): shows current state (Idle/Running/
Error, with the error message if present) and a Start/Stop button reflecting
it. Calls `runner_status()` on mount for the initial state, then
`listen("runner-status-changed", ...)` for updates — matching the event
name and payload shape in §3 exactly.

## 6. The Keybind-Assignment Note

Per §2's correction: a short, static explanatory note near the Start
button (not a per-profile indicator, which isn't implementable), shown
always rather than conditionally (there's no data to condition it on):

> The first time you click Start, KDE may ask you to press a key
> combination for each profile that doesn't have one assigned yet — this
> happens once per profile. Change an assigned shortcut anytime in KDE's
> System Settings → Shortcuts → Orchestrator.

## 7. Testing Strategy

- **Pure logic**: none of substance exists in this increment beyond what
  `orchestrator-core::runner` already tests (this increment adds no new
  pure functions — `RunnerState`'s transitions are trivial and exercised
  implicitly by the command/tray code that mutates them, not worth
  extracting into a separately-tested pure module).
- **Command/tray wiring**: verified live, same "I/O glue, not unit tested"
  pattern this project uses throughout (`linux_run`, `linux_service`,
  Task 3's `list_windows`).
- **Live verification, with an explicit human-only boundary**: per this
  session's established safety constraint (no agent-driven interactive UI
  testing — clicking Start/Stop in the running app, watching a tray icon
  behave, or confirming a KDE dialog appears are all things only the human
  partner does). An agent implementing this can verify non-interactively:
  the code compiles and type-checks, `cargo build`/`clippy`/`fmt` pass,
  `npm run check`/`build` pass, and can trace the command/event wiring by
  reading code carefully (the same substitute used for the profile-
  management increment's Task 4, which caught two real bugs this way). A
  manual test script (same pattern as `crates/orchestrator-gui/TESTING.md`)
  covering: start with no profiles configured (confirm no crash, idle-ish
  behavior), start with a fresh profile (confirm KDE's dialog appears once),
  stop, start again (confirm no duplicate dialog for the same profile),
  close the window (confirm it hides, not quits, and a hotkey still fires),
  tray Start/Stop toggle, tray Quit — is this increment's deliverable
  alongside the code, for the human to actually run.

## 8. Out of Scope

- Any change to `Profile.trigger`'s meaning, or making the real assigned
  key combo visible anywhere in Orchestrator's own UI — that would require
  a way to read the assignment back from KDE (unclear if the portal or
  kglobalaccel exposes this at all; a real spike, not part of this
  increment).
- Auto-starting the runner when the app launches, or persisting any
  "was running last time" preference — always starts `Idle`, matching this
  project's YAGNI bias against unrequested settings/preferences UI.
- App icon design, `.desktop` entry, `.deb` packaging for the GUI — same
  standing deferral as the profile-management spec's own §9.
- macOS: same standing hardware limitation as everywhere else in this
  project.
