# Orchestrator CLI UX — Design Specification

**Date:** September 12, 2026
**Scope:** `orchestrator-cli`'s real command surface — `profile add/edit/remove/list` and `run` — replacing the Task 9 hidden diagnostic subcommands with genuine product commands. First increment to wire the runner (`orchestrator-core::runner`, complete as of the prior increment) into an actual binary.

---

## 1. Problem Statement

Every piece the CLI needs already exists: `orchestrator-core`'s `Config`/`Profile` data model and `Runner`, and real Linux/KDE backends for all three trait crates (hotkey, input, window — live-verified working). Nothing yet lets a user actually author a profile or run one without hand-editing JSON and writing throwaway Rust. `orchestrator-cli` is still a bare `clap::Parser` with only hidden, one-shot diagnostic subcommands left over from the runner increment's human-verification pass. This spec defines the real command surface.

## 2. A Resolved Simplification: `Profile.trigger` Needs No Author-Time Input

`Runner::run()` registers each profile's hotkey via `hotkey.register(&profile.name, None)` — the portal action id is the profile's **name**, not its `trigger` field, and `preferred` is always `None` (the KDE backend ignores it regardless, per the runner increment's resolved design decision). The actual key combo is assigned live, through the portal's own system dialog, the first time a profile's hotkey is registered — not at authoring time.

Consequence: `profile add`/`edit` never prompt for or accept a hotkey combo. `Profile.trigger` is written as a placeholder (`KeyCombo { modifiers: vec![], key: String::new() }`) at authoring time and is purely informational until the profile has actually been run at least once (the design spec's existing "last resolved combo" framing already covers this — this increment doesn't change `Profile`'s schema at all, just confirms the authoring flow doesn't need to populate it meaningfully).

## 3. Command Surface

```
orchestrator profile add --name <name> --scope desktop|window [window-selection flags] \
    --action repeat --input <step-spec> --interval-ms <n> [--jitter-ms <n>] [--debounce-ms <n>]

orchestrator profile add --name <name> --scope desktop|window [window-selection flags] \
    --action macro --step <step-spec> [--step <step-spec> ...] [--loop] [--debounce-ms <n>]

orchestrator profile edit <name> [any subset of the add flags above — only given ones change]

orchestrator profile remove <name>

orchestrator profile list

orchestrator run [--config <path>]
```

Global default config path: `Config::config_path()` (already implemented — XDG-aware on Linux, `~/Library/Application Support` on macOS). `--config <path>` overrides it for all `profile *`/`run` subcommands.

### 3.1 Window selection (`--scope window`)

No flags carry `process_name`/`window_title_hint` directly. Instead, `--scope window` triggers a live picker:
1. Construct a real `KwinWindowLocator` (Linux) — this subcommand requires a live KDE/Wayland session, same requirement `run` already has.
2. Call `list_windows()`, print a numbered table: index, pid, `process_name`, `title`.
3. Prompt for a selection (plain stdin read, parse as an index into the list).
4. Populate `Scope::Window { process_name, window_title_hint: <chosen window's title, verbatim>, backend_hint_id: Some(<chosen window's handle>) }`.

If zero windows are returned, or the index is out of range, or stdin can't be read, fail the command cleanly with a specific error — do not fall back to any default.

`focus_steal` is never a CLI flag. It's derived automatically: `true` for `Scope::Window`, `false` for `Scope::Desktop` — matching the top-level brief's "auto-set and explained, not user-hidden."

### 3.2 Step notation (`--input`, `--step`)

A single colon-separated mini-notation, shared between `--action repeat --input <spec>` (exactly one, no delay component — timing comes from `--interval-ms`/`--jitter-ms`) and `--action macro --step <spec>` (repeatable, each carries its own delay). **`--action repeat --input` only accepts `key` and `scroll`** — not `click`/`drag`. Rationale: `Action::Repeat.input` is a single raw `InputEvent`, not a composite action. A repeated bare `KeyPress` with no release matches genuine OS key-repeat semantics (holding a key down) — a legitimate, common case (e.g. "hold W to walk"). A repeated `Scroll` is a naturally discrete, repeatable event (each tick is one wheel-click). But a repeated bare `MouseButtonPress` with no interleaved `MouseButtonRelease` does not correspond to how mouse clicks are recognized at the OS level (unlike keyboards, there's no standard "button auto-repeat" concept) — it would not reliably register as repeated clicking. Anything needing press+release (clicking, dragging) belongs in `--action macro --loop`, where `MacroStep::MouseClick`/`Drag` already correctly emit a complete press+release pair per cycle.

| Step kind | Notation | Valid in | Example |
|---|---|---|---|
| Key tap | `key:<combo>:<delay_ms>` (macro) / `key:<combo>` (repeat) | repeat, macro | `key:Ctrl+Alt+F5:50` |
| Mouse click | `click:<left\|right\|middle>:<x,y\|cursor>:<delay_ms>` | macro only | `click:left:100,200:30` |
| Drag | `drag:<x,y\|cursor>:<x,y\|cursor>:<delay_ms>` | macro only | `drag:cursor:300,400:50` |
| Scroll | `scroll:<dx>,<dy>:<delay_ms>` (macro) / `scroll:<dx>,<dy>` (repeat) | repeat, macro | `scroll:0,-3:20` |

`--action repeat --input click:...` or `--input drag:...` is rejected with a clear error naming the restriction and pointing at `--action macro --loop` as the alternative.

`<combo>` is `+`-joined modifiers and key, e.g. `Ctrl+Alt+F5` or a bare `A` — parsed into `orchestrator_hotkey::KeyCombo` using the same modifier-name mapping already established in `kde_portal_shortcuts.rs`'s `parse_trigger_description` (`ctrl`/`control`, `shift`, `alt`, `meta`/`super`/`win`, case-insensitive), reused or duplicated as a small standalone parser in the CLI crate — do not depend on `orchestrator-hotkey`'s internal (non-`pub`) parser function; write a fresh one against the public `KeyCombo`/`Modifier` types if the existing one isn't exported.

`drag` always uses `MouseButton::Left` (no button in the notation) — matches `MacroStep::Drag`'s existing schema (no button field) and the runner's existing hardcoding; do not add a button component to the drag notation.

For `--action repeat`, the parsed step is converted directly to the single `orchestrator_input::InputEvent` `Action::Repeat` needs (`key` → `InputEvent::KeyPress`+`KeyRelease` is wrong here — `Action::Repeat.input` is a single `InputEvent`, not a tap; a `key:` spec for `--input` maps to `InputEvent::KeyPress(combo)` alone, since "repeat this key" for an autoclicker-style tool means repeatedly issuing the down event at each tick, matching a fast-clicker style use rather than nested tap semantics — this is a genuine judgment call being made explicit here, not silently assumed elsewhere in the codebase). `click`/`scroll` map to their corresponding single `InputEvent` variant directly (`MouseButtonPress`, `Scroll`).

Malformed notation (wrong number of colon-separated fields, unparseable combo, unparseable coordinates, unknown step kind) fails the command with a specific error naming what was wrong and where — never silently drops or defaults a step.

### 3.3 `profile edit`

Same flag surface as `add`. Looks up the profile by `name` (the positional argument, which is fixed — renaming a profile is not supported by `edit`'s `--name`; `--name` on `edit` is not accepted at all, since the lookup key and the field would collide confusingly. If renaming is ever wanted, that's `remove` + `add` under a new name). Any other flag given overwrites that field; omitted flags keep their current value. `--step` given at all replaces the entire step list (no partial insert/delete/reorder). `--scope window` re-triggers the live picker (does not attempt to preserve or reuse the old `backend_hint_id`).

### 3.4 `profile remove <name>`

Removes by exact name match. Errors clearly (naming the profile) if no such profile exists. Saves via the existing atomic `Config::save`.

### 3.5 `profile list`

Prints one line per profile: `name`, scope summary (`Desktop` or `Window(<process_name>)`), action summary (`Repeat every <interval_ms>ms±<jitter or "no jitter">` or `Macro (<N> steps, looping|once)`), `debounce_ms`. Plain formatted text (a table via manual column alignment, not a table-rendering dependency — no new crate needed for this).

### 3.6 `run [--config <path>]`

1. Load and validate config (`Config::load`, already atomic/validated).
2. Install a `tracing_subscriber` (e.g. `tracing_subscriber::fmt::init()`) so the runner's existing `tracing::warn!` calls (added in the runner increment's final review) actually surface to the terminal — this is the first real binary that needs a subscriber; nothing installs one today.
3. Construct real backends per `#[cfg(target_os = "linux")]`/`#[cfg(target_os = "macos")]`, using the fixed app-id `io.github.barebones-sh.Orchestrator` for `KdePortalHotkeyBackend::new(app_id)`.
4. Build `Runner::new(hotkey, input, window, config.profiles)`, call `.run(ctrl_c_future)` where `ctrl_c_future` wraps `tokio::signal::ctrl_c()`.
5. Print a short startup summary (profile count, config path used) before blocking; print nothing further unless a profile's own `tracing::warn!` fires or `run()` returns an error, which is printed and the process exits non-zero.

### 3.7 App-id / `.desktop` file

App-id: `io.github.barebones-sh.Orchestrator` (fixed, hardcoded in `run`'s backend construction — not a CLI flag; a single app has one identity).

Ship `packaging/linux/io.github.barebones-sh.Orchestrator.desktop` in the repo (minimal: `Type=Application`, `Name=Orchestrator`, `Exec=orchestrator`, `NoDisplay=true` — mirroring the throwaway ones used during the spike, but a real, committed, permanent file this time). Document (in the crate's README or a short `packaging/linux/README.md`) the one-time dev-install step: copy to `~/.local/share/applications/` and run `kbuildsycoca6` (or `update-desktop-database`) — matching exactly what the spike found necessary. Real `.deb` packaging (installing this file system-wide as part of a package) remains out of scope, per the original project brief's non-goals for this stage.

## 4. Removing the Task 9 Diagnostics

The `InternalHotkeySmokeTest`/`InternalWindowSmokeTest`/`InternalInputSmokeTest` hidden subcommands in `orchestrator-cli/src/main.rs` are deleted entirely as part of this increment. They served their one-time live-verification purpose during the runner increment; `run` now exercises the same real backends for real, and `profile add --scope window`'s live picker exercises the window backend directly. Keeping dead diagnostic-only code around has no value once real commands cover the same ground.

## 5. Testing Strategy

- Pure notation-parsing functions (the `key:`/`click:`/`drag:`/`scroll:` parser, and the combo-string parser) are unit-testable without any live backend or config file — cover valid cases for each step kind and malformed-input error cases.
- `profile add/edit/remove/list`'s config-mutation logic (given an in-memory or temp-file `Config`) is testable against `orchestrator-core`'s existing `Config::load`/`save`/`validate` without needing a live window picker — structure the window-picker interaction (list + prompt) behind a small trait or function parameter so the config-mutation logic itself can be tested with a fake selection, matching the runner increment's established "fake the boundary, test the logic" pattern.
- The live window picker itself, and `run`'s end-to-end behavior against real backends, are **not** unit-testable (same "not automatable, needs a human" class as the runner/spike's own live-verification requirements) — this increment's plan should call for a human-in-the-loop verification pass after implementation, the same way the runner and KDE-backend increments each closed with one.

## 6. Out of Scope

GUI (Tauri app) — untouched, still a placeholder. macOS real backends — untouched, still stubs; `run` on macOS fails cleanly with existing "not yet implemented" errors, which is expected and not a regression. Real `.deb`/packaging automation beyond the single `.desktop` file + a documented manual dev-install step. Any change to `orchestrator-core`'s `Profile`/`Action`/`Scope`/`Config` schema — none is needed, this increment only adds a CLI layer on top of what already exists. Interactive/TUI profile editing beyond the flag-based `add`/`edit` described here. Renaming a profile in place (use `remove` + `add`).
