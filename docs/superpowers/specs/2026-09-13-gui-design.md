# Orchestrator GUI — Design Specification

## 1. Problem Statement

`orchestrator-gui` has been a placeholder binary (`println!` and nothing
else) since the project's first increment. The original design spec named
"the complete GUI Tauri app and UI" as explicitly out of scope for v1,
scaffolded only as a workspace member with no `tauri` dependency yet
(`docs/superpowers/specs/2026-08-09-orchestrator-design.md` §4, §7, §8).

This spec covers building the real thing: a Tauri v2 desktop app for
managing Orchestrator profiles, sharing `orchestrator-core`'s types and
config file with the CLI (same invariant the CLI's own spec states: "Both
CLI and GUI consume the same core types, with no independent parsing logic
in either" — `docs/superpowers/specs/2026-08-09-orchestrator-design.md`
line 178).

## 2. Scope Split

The full vision — profile management, live start/stop of the automation
runner, and a system tray icon — is too much for one implementation plan at
this project's established review rigor (one task, one fresh reviewer, one
diff worth holding in a reviewer's head). It splits into two increments:

- **This spec / this increment ("GUI: profile management"):** a real,
  launchable Tauri app that lists, adds, edits, and removes profiles —
  writing the same `config.json` the CLI already reads. You'd still start
  the automation via `orchestrator run` or the systemd service from the
  `autostart` increment; the GUI does not run anything itself yet.
- **A follow-up increment ("GUI: live control"), not designed here:**
  embeds `orchestrator-core`'s `Runner` directly in the Tauri Rust backend
  (mirroring `orchestrator-cli`'s `linux_run` module), adds start/stop/status
  commands, and a system tray icon so closing the window doesn't kill a
  running session. Left for its own brainstorm once this increment lands —
  its shape depends on decisions made here (the command surface, the
  frontend's state model).

App icon design, a proper (non-`NoDisplay`) `.desktop` entry, and `.deb`
packaging for the GUI binary are also out of scope for this spec — same
"separate future increment" pattern the CLI's own CI/`.deb`/autostart work
followed.

## 3. Environment Constraint (Read Before Implementing)

Tauri needs system libraries (`webkit2gtk`, GTK, `libxdo`, etc.) to build or
launch on Linux. The development sandbox this project has used all session
does not have them installed by default, and the agent executing this plan
cannot install them (no passwordless `sudo`). **The human partner must run
this once, themselves, before Task 1 can be attempted for real:**

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Task 1 (below) is deliberately structured as a spike-shaped first step —
prove the toolchain actually produces a running window in this environment
— before anything else is built on top of it, precisely because this is the
one dependency this whole plan can't route around.

## 4. Workspace Layout

`crates/orchestrator-gui/` restructures from a flat Rust binary crate into
the standard Tauri v2 project shape:

```
crates/orchestrator-gui/
├── package.json              # Svelte + Vite frontend
├── vite.config.js
├── index.html
├── src/                      # Svelte frontend source
│   ├── main.js
│   ├── App.svelte
│   └── lib/
│       └── ...
└── src-tauri/                # Rust backend (the actual Cargo crate)
    ├── Cargo.toml
    ├── tauri.conf.json
    ├── build.rs
    ├── icons/                 # generated via `cargo tauri icon`
    └── src/
        └── main.rs
```

The root workspace `Cargo.toml`'s `members` list currently reads
`["crates/*"]`, a glob that finds `crates/orchestrator-gui/Cargo.toml`
today. Once that manifest moves to `crates/orchestrator-gui/src-tauri/`,
the glob no longer reaches it (one level too deep) — add
`"crates/orchestrator-gui/src-tauri"` as an explicit second entry alongside
the existing glob. Cargo supports mixing a glob and explicit paths in one
`members` list.

Frontend tooling (`npm`/Vite) is independent of the Cargo workspace — no
change needed there. `tauri.conf.json`'s `build.beforeDevCommand`/
`build.beforeBuildCommand` point at the frontend's `npm run dev`/
`npm run build` scripts, and `build.frontendDist` points at Vite's build
output directory, per Tauri's own conventions.

## 5. Shared Logic Refactor: `profile_commands.rs` → `orchestrator-core`

`orchestrator-cli/src/profile_commands.rs` today mixes two concerns in one
file:

1. **Notation-string parsing** (`ProfileActionArgs`'s `Repeat { input: String,
   ... }` / `Macro { steps: Vec<String>, ... }` shape, and `build_action`,
   which calls `orchestrator-cli::notation::{parse_repeat_input,
   parse_macro_step}`) — this is CLI-specific. The GUI's forms produce
   typed values directly (a dropdown for modifiers, a number input for
   `interval_ms`) and have no reason to round-trip through notation text.
2. **Profile mutation logic** (`add_profile`/`edit_profile`/`remove_profile`:
   name-uniqueness checks, atomic-on-failure field resolution, deriving
   `focus_steal` from `Scope`, defaulting `debounce_ms`) — this is
   notation-agnostic. It operates on already-typed `Scope`/`Action` values
   once `build_action` has run.

Without separating these, the GUI's Rust backend would either duplicate
concern 2 verbatim (a DRY violation this project's own conventions
wouldn't accept from a human developer) or awkwardly manufacture notation
strings from structured GUI input just to reuse the existing functions
(the wrong direction — notation exists to make the CLI *terse*, not as this
logic's natural type).

**The fix:** move concern 2 into a new `orchestrator_core::profile_ops`
module: `add_profile`/`edit_profile`/`remove_profile` taking already-typed
`Scope`/`Action` parameters (not `ProfileActionArgs`), plus the small pure
helpers they depend on (`placeholder_trigger`, `focus_steal_for`) and a new
core-level error type covering exactly the notation-independent failure
modes (`NoSuchProfile`, `DuplicateProfileName`). `resolve_window_scope`
(window-index → `Scope` resolution) is equally notation-agnostic and moves
alongside them, since the GUI's window-scope picker needs the identical
operation (a list of windows, a chosen one, resolve to `Scope::Window`).

`orchestrator-cli/src/profile_commands.rs` keeps `ProfileActionArgs`,
`build_action` (notation parsing), `format_profile_list` (CLI-only text
formatting — the GUI renders profiles as its own UI, not a formatted
string), and `ProfileCommandError` — now a thin wrapper adding
`Notation`/`MissingRequiredField`/`RepeatAndMacroBothOrNeitherSpecified` on
top of `orchestrator_core::profile_ops`'s error type via a `From` impl
(the same pattern `ProfileCommandError` already uses for
`NotationError` today). Its own `add_profile`/`edit_profile`/`remove_profile`
become thin: resolve notation via `build_action`, then delegate to
`orchestrator_core::profile_ops`.

This refactor is purely internal — the CLI's actual command-line behavior,
error messages, and test coverage carry over unchanged (tests move/adapt to
call the new split, but assert the same behavior). No `Config` file format
change, no CLI-visible behavior change.

## 6. GUI Command Surface (Tauri IPC)

`src-tauri/src/main.rs` exposes these `#[tauri::command]` functions to the
frontend (all synchronous — same "no live D-Bus/window-listing in the pure
layer" boundary the CLI's own design spec draws, except `list_windows`,
which necessarily talks to the KWin D-Bus bridge the same way the CLI's
`linux_window_picker` already does):

- `list_profiles() -> Vec<ProfileDto>` — reads the config file, returns a
  serializable view of every profile. `ProfileDto` mirrors `Profile` closely
  enough for direct `serde`-derived (de)serialization across the IPC
  boundary — no separate hand-written DTO type is needed unless a
  frontend-only field turns out to be necessary once building the UI makes
  that concrete (defer that call to the implementation task, not this spec).
- `add_profile(name: String, scope: ScopeInput, action: ActionInput,
  debounce_ms: Option<u32>) -> Result<(), String>`
- `edit_profile(name: String, scope: Option<ScopeInput>, action:
  Option<ActionInput>, debounce_ms: Option<u32>) -> Result<(), String>`
- `remove_profile(name: String) -> Result<(), String>`
- `list_windows() -> Result<Vec<WindowDto>, String>` — for the window-scope
  picker; wraps `orchestrator_window::kwin_dbus::KwinWindowLocator`, same
  backend the CLI's picker uses.

`ScopeInput`/`ActionInput` are the frontend-facing shapes for `Scope`/
`Action` — likely identical to the core types via `serde`, since both are
already plain, JSON-serializable enums with no CLI-notation dependency.
Tauri commands return `Result<T, String>` by convention (errors serialize
to a message the frontend displays) rather than a structured error enum —
simpler than threading `ProfileCommandError`/`profile_ops`'s error type
across the IPC boundary for a single-string toast/alert UI.

## 7. Frontend (Svelte)

Plain Svelte + Vite, no additional UI component library (YAGNI — a form-
and-list app this size doesn't need one). Structure:

- `App.svelte` — top-level layout: a profile list, an "Add profile" button,
  edit/remove actions per row.
- A profile form component (shared between add and edit) with fields
  matching `ScopeInput`/`ActionInput`: scope (Desktop vs. Window, with a
  "pick a window" flow calling `list_windows`), action (Repeat vs. Macro,
  with the same fields the CLI's `--action repeat --input ...`/`--action
  macro --step ...` expose, but as real form widgets instead of notation
  text), debounce.
- All backend communication via Tauri's `invoke()` calling the commands in
  §6. No direct filesystem/config access from the frontend — the Rust
  backend is the only thing that touches `Config::load`/`save`, matching
  the CLI's own boundary.

## 8. Testing Strategy

- **`orchestrator_core::profile_ops`** (moved/refactored logic): unit tests
  carry over from `profile_commands.rs`'s existing suite, adapted to the new
  signatures — same coverage (duplicate names, atomic-edit-on-failure,
  `focus_steal` derivation, window-index resolution), same rigor.
- **`orchestrator-cli::profile_commands`** (thinned wrapper): existing
  notation-specific tests (bad notation, missing required fields, the
  repeat/macro conflict check) stay here, adapted to call through the new
  thin wrapper.
- **Tauri commands**: thin glue over `profile_ops`, same "verified live,
  not unit tested" pattern this project uses for I/O glue (`linux_run`,
  `linux_service`) — Task 1's spike proves the toolchain works, later tasks
  live-verify each command by actually clicking through the running app.
- **Frontend**: no test framework introduced for this increment (YAGNI for
  a first pass at a small form-and-list UI) — verified by live use during
  each task, same bar as the Rust-side I/O glue.

## 9. Out of Scope

- Live start/stop of the `Runner`, system tray icon, running-state UI (next
  increment, per §2).
- App icon *design* (a placeholder is generated via `cargo tauri icon` from
  a simple source image so the build succeeds; a real icon is a cosmetic
  follow-up, not gated on this plan).
- A visible `.desktop` entry, `.deb` packaging, or autostart integration for
  the GUI binary specifically (all exist for the CLI already; extending them
  to the GUI is separate future work).
- macOS: `orchestrator-gui`'s Cargo.toml already carries macOS-conditional
  dependencies (unchanged by this spec) but there is no macOS hardware in
  this environment to build or verify a macOS Tauri bundle — same standing
  limitation as the CLI's macOS backends.
