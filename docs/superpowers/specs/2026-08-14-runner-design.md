# Orchestrator Runner — Design Specification

**Date:** August 14, 2026
**Scope:** `orchestrator-core`'s runner/state-machine module (`runner.rs`), the piece that turns a loaded `Profile` list plus the three trait-crate backends into actual running toggle/repeat/macro behavior. Library-only — no CLI/GUI wiring in this increment (that's deferred to a later "CLI UX" increment).

---

## 1. Problem Statement

Every other piece of Orchestrator now exists: the `Profile`/`Action`/`Scope` data model (`orchestrator-core`), and real Linux/KDE implementations of `HotkeyBackend`, `InputInjector`, and `WindowLocator` (confirmed working live against a real KDE Plasma/Wayland session — see `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`). Nothing yet turns a `Profile` into running behavior: there is no toggle logic, no debounce, no interval/jitter scheduling, no macro-step execution, and no window-scope resolution-at-fire-time. `runner.rs` was named in the original design spec's file layout with zero further detail. This spec fills that gap.

## 2. Public API

```rust
pub struct Runner<H: HotkeyBackend, I: InputInjector, W: WindowLocator> {
    hotkey: H,
    input: I,
    window: W,
    profiles: Vec<Profile>,
}

impl<H: HotkeyBackend, I: InputInjector, W: WindowLocator> Runner<H, I, W> {
    pub fn new(hotkey: H, input: I, window: W, profiles: Vec<Profile>) -> Self;

    /// Registers every profile's hotkey trigger, then drives toggle/repeat/
    /// macro execution for all profiles concurrently until `shutdown`
    /// resolves. Consumes `self`. No CLI/signal handling lives here — the
    /// caller supplies the shutdown future (e.g. `tokio::signal::ctrl_c()`
    /// wrapped to `Future<Output = ()>`, or a test's own oneshot channel).
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), RunnerError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("failed to register hotkey for profile {profile_name:?}: {source}")]
    HotkeyRegistration { profile_name: String, #[source] source: orchestrator_hotkey::HotkeyError },
    // (additional variants as implementation needs arise; not exhaustively
    // enumerated here since fatal-vs-logged-and-continue is decided per
    // error site in §7, not by variant count)
}
```

`H`/`I`/`W` are generic (not `dyn`), matching the workspace's existing compile-time-backend-selection architecture (design spec §5). `run()` is the sole entry point; there is no separate "start"/"stop"/"poll" API surface — internal per-profile state (§4) is not exposed. If a future increment needs to observe live status (for a GUI), that's new scope, not part of this one (YAGNI).

## 3. Startup Sequence

1. For each `Profile` in `profiles`, call `hotkey.register(&profile.name, None)` (no `preferred` combo — per the resolved Task 6 design decision, the KDE backend always defers to the portal's own dialog regardless, and this stays backend-agnostic). Build a `HashMap<HotkeyId, usize>` mapping the returned id to the profile's index.
   - If any registration fails, `run()` returns `Err(RunnerError::HotkeyRegistration { .. })` immediately without starting anything — a profile whose hotkey can't be registered can never be toggled, so partial startup isn't useful.
2. Wrap `input` and `window` in `Arc` internally (backends are `Send + Sync`, and multiple profiles' tasks need shared `&self` access concurrently).
3. Bridge `hotkey.subscribe()`'s `std::sync::mpsc::Receiver` onto a blocking thread (`tokio::task::spawn_blocking`, forwarding into a `tokio::sync::mpsc::UnboundedSender`) so it can be `select!`-ed alongside async work — the trait's `subscribe()` is deliberately sync per its doc comment, and previous tasks (e.g. Task 9's smoke test) already established this bridging pattern.
4. Enter the main loop: `tokio::select!` between the bridged hotkey-event stream and `shutdown`. On `shutdown` resolving, cancel every currently-running profile task and return `Ok(())` once they've all torn down.

## 4. Per-Profile State & Toggle/Debounce

Each profile has runtime state (not part of the persisted `Profile` struct — this lives in the runner only):

```rust
struct ProfileState {
    running_task: Option<(CancellationToken, JoinHandle<()>)>,
    last_toggle_at: Option<Instant>,
}
```

On a `HotkeyEvent::Pressed` for a profile's registered id:

1. **Debounce check:** if `last_toggle_at` is `Some(t)` and `now - t < Duration::from_millis(profile.debounce_ms)`, ignore this press entirely — no state change. This is a cooldown on the *toggle action*, not a "wait for a pause in presses" mechanism: it works identically regardless of whether the backend delivers repeated `Pressed` events during a held key or not (unverified either way for the portal backend).
2. **Otherwise, flip state:**
   - **Currently stopped → start:** resolve `Scope` (§6). On resolution failure (e.g. zero matching windows), log and leave the profile stopped — do not toggle. On success, spawn the repeat/macro task (§5) with a fresh `CancellationToken`, store `(token, handle)`, update `last_toggle_at`.
   - **Currently running → stop:** cancel the token, `.await` the `JoinHandle` (task teardown is fast — a `tokio::select!` on the cancellation token inside the loop, see §5), clear `running_task`, update `last_toggle_at`, and if `Scope::Window` was resolved with "activate once" (§6), restore the prior focus now.

`HotkeyEvent::Released` is ignored for toggle purposes — toggling is edge-triggered on press only, per the top-level brief ("pressing the same registered combo... stops it").

## 5. Action Execution

### 5.1 `Action::Repeat`

```rust
async fn run_repeat(input: Arc<I>, event: InputEvent, interval_ms: u64, jitter: Jitter, cancel: CancellationToken) {
    loop {
        if let Err(e) = input.inject(&event).await {
            tracing::warn!("injection failed, continuing: {e}"); // see §7
        }
        let delay = compute_delay(interval_ms, &jitter);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => break,
        }
    }
}
```

Fires immediately on toggle-on (before the first sleep) — pressing the hotkey starts clicking right away, not after waiting one full interval.

`compute_delay`: `Jitter::None` → `interval_ms` fixed. `Jitter::Uniform { range_ms }` → `interval_ms as i64 + uniform(-range_ms as i64, range_ms as i64)`, symmetric around the interval. This resolves an ambiguity the original design spec left implicit, and is exactly why `orchestrator-core`'s existing `validate()` already requires `range_ms <= interval_ms` — that bound is precisely what keeps the computed delay from ever going negative.

For `Scope::Window`, `run_repeat` is unchanged — it calls `input.inject()` directly every tick with no re-activation, because activation already happened once at toggle-on (§6).

### 5.2 `Action::Macro`

Iterates `steps` in declared order. For each step: sleep `delay_ms` first (uniform treatment, no special-cased first step), then execute:
- `KeyPress { combo, .. }` → `input.inject(&InputEvent::KeyPress(combo))` immediately followed by `input.inject(&InputEvent::KeyRelease(combo))` (a "tap" — `MacroStep` has no separate release variant, confirmed against the current `orchestrator-core::action` source).
- `MouseClick { button, position, .. }` → if `position` is `Fixed { x, y }`, `MouseMoveAbsolute` first; if `AtCursor`, skip the move. Then `MouseButtonPress`/`MouseButtonRelease`.
- `Drag { from, to, .. }` → if `from` is `Fixed`, move there first; if `AtCursor`, press down without moving. Press the button, move to `to`, release. `MacroStep::Drag` has no `button` field in the current schema (confirmed against `orchestrator-core::action`'s source — unlike `MouseClick`, which does) — this increment hardcodes `MouseButton::Left`, the overwhelmingly common drag case, and does not change the schema to add a button field (out of scope, no consumer needs it yet; a future increment can add `button: MouseButton` to `Drag` with a `#[serde(default)]` if a real need arises). No position-query primitive is needed anywhere — `AtCursor` is implemented purely as "skip the move," never as "read the current position back."
- `Scroll { dx, dy, .. }` → `InputEvent::Scroll`.

If `loop_` is true, repeat the whole sequence (checking cancellation between steps, same `tokio::select!` pattern as §5.1) until cancelled. If `loop_` is false, run the sequence once and then the task ends on its own — the runner treats this as the profile self-toggling off (clears `running_task`, updates state) rather than waiting for a second hotkey press that would otherwise toggle a non-existent running task back on immediately.

## 6. Window Scope Resolution

At toggle-on time only (not re-resolved per tick, per the resolved design decision): `orchestrator-core` (not any trait) does the matching, per the original design spec's disambiguation policy:

1. Call `window.list_windows()`.
2. Filter by `process_name`; whenever `window_title_hint` is non-empty, further filter by it (title substring match) — this applies regardless of how many `process_name` matches there are, including exactly one, not only when there are multiple. If applying a non-empty hint leaves zero candidates, that is treated as no match at all (see point 5) rather than a silent fallback to the unfiltered `process_name` matches: a hint that matches nothing means the profile's targeting intent can't be satisfied, and toggle-on should fail cleanly instead of silently acting on the wrong window.
3. Among remaining matches, prefer the entry whose `WindowHandle` equals the profile's stored `backend_hint_id`, if present and still among the matches.
4. Otherwise, fall back to the first remaining match and log a warning (window IDs aren't stable across restarts — this is expected, not an error condition).
5. If zero windows match at all — either no `process_name` match, or a non-empty `window_title_hint` that matched nothing among the `process_name` matches — toggle-on fails (§4) — the profile does not start desktop-wide as a silent fallback.

Once resolved, call `window.activate_window(&handle)` exactly once, then start the repeat/macro task (§5). On toggle-off, call `window.activate_window(&previously_focused_handle)` to restore — the previously-focused handle is captured via `window.focused_window()` *before* the toggle-on activation call, mirroring the spike's confirmed activate→inject→restore pattern (`docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`, Phase 3c), just amortized across the whole run instead of once per tick.

`Scope::Desktop` skips all of this — no activation, no restoration, `input.inject()` goes straight to whatever currently has real focus, exactly as `requires_focus_steal_for_window_targeting()` already documents.

## 7. Error Handling

- **Hotkey registration failure at startup:** fatal, `run()` returns `Err` before anything starts (§3).
- **Window resolution/activation failure at toggle-on:** non-fatal to the runner as a whole — that one profile simply doesn't start, logged, the hotkey-event loop continues serving other profiles.
- **A single injection failure mid-repeat/mid-macro:** logged, the loop continues to the next tick/step. A transient `ydotool` hiccup shouldn't permanently kill an otherwise-working profile. (If a backend is *persistently* failing, every tick logs — this increment doesn't add backoff/circuit-breaking; YAGNI unless real usage shows it's needed.)
- **Shutdown:** cancels every running profile task and awaits clean teardown (including toggle-off's focus-restoration step) before `run()` returns.

## 8. Testing Strategy

`Runner<H, I, W>` is generic, so tests use small in-memory fake implementations of all three traits (channel-driven — e.g. a fake `HotkeyBackend` whose `subscribe()` receiver is fed by the test itself to simulate presses, a fake `InputInjector` that records every `inject()` call into a `Vec` the test asserts against) rather than mocks-of-behavior. This is the same "test what's testable without live D-Bus" principle the previous increment established, now applied to logic that's *entirely* testable this way, since none of the runner's own logic is D-Bus-bound — only the three backends it's generic over are, and those are swapped out entirely in tests.

Coverage this increment's tests must include: debounce suppressing a rapid second press; toggle-on/toggle-off state transitions; jitter's delay bounds (statistical/range assertion, not exact-value, given randomness); a non-looping macro self-toggling off after completion; window-scope toggle-on failing cleanly when zero windows match; concurrent toggle of two different profiles not interfering with each other's state.

## 9. Out of Scope

CLI/GUI wiring (a later increment); persisting/observing live per-profile running-status externally; backoff/circuit-breaking on persistent injection failure; macOS backends (still stubs); any change to `orchestrator-core`'s `Profile`/`Action`/`Scope` schema (none is needed — this increment is additive, a new `runner.rs` module plus its own `RunnerError` type).
