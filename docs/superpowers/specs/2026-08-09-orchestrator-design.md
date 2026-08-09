# Orchestrator v1 Design Specification

**Date:** August 9, 2026
**Scope:** Linux/Wayland + macOS (core traits and data model)

---

## 1. Problem Statement and Design Constraints

Orchestrator is a cross-platform input automation framework designed to inject synthetic input (keyboard, mouse, scrolling) into arbitrary window targets, whether a single focused window or the desktop globally.

### Wayland Focus-Steal Constraint

On Wayland/KWin, there is no `XSendEvent`/`XTest` equivalent — synthetic input injection (`ydotool`/uinput, or libei via the `RemoteDesktop` xdg-desktop-portal) is global: it goes wherever the compositor's keyboard/pointer focus currently is, not to an arbitrary target window. **It is very likely that no-focus-steal targeting is not achievable on Wayland today.** This is a fundamental architectural constraint that must be designed into the system from the start, not retrofitted.

This is precisely why a spike (§6 below) must verify this empirically before the `Scope`/`focus_steal` model is finalized. Until then, the design accommodates two scenarios:

1. **Focus-steal targeting (`Window` scope with `focus_steal: true`)**: activate the target window, inject input, restore previous focus. This is expected to work reliably on Wayland, at the cost of visible focus flicker.

2. **Desktop-wide injection (`Scope::Desktop`)**: inject into whatever is currently focused, no targeting. This works robustly everywhere and must be a first-class, fully-supported mode, not a lesser fallback, since it is the case that works universally.

---

## 2. Data Model

All types in this section are Rust, written as they will appear in the crate code (fenced as Rust code blocks). Serialization defaults are given inline.

### Core Profile

A `Profile` is the unit of automation: a hotkey trigger bound to an action, scoped to a target, with optional debouncing.

```rust
pub struct Profile {
    pub name: String,
    pub trigger: orchestrator_hotkey::KeyCombo,
    pub action: Action,
    pub scope: Scope,
    pub focus_steal: bool,
    pub debounce_ms: u32, // default: 400
}
```

### Scope

Target selection: either the desktop (no window targeting) or a specific window matched by process name and/or title hint.

```rust
pub enum Scope {
    /// Inject into whatever is currently focused globally.
    Desktop,
    /// Inject into a window matching the given criteria.
    Window {
        /// Process executable name (e.g., "firefox").
        process_name: String,
        /// Hint string to disambiguate multiple windows of the same process.
        window_title_hint: String,
        /// Opaque backend-specific identifier (e.g., KWin's UUID).
        /// Allows re-finding the exact window instance on restart.
        backend_hint_id: Option<String>,
    },
}
```

**Disambiguation Policy:** When multiple windows match the `process_name` and `window_title_hint` criteria, the user must disambiguate at setup time. The UI/CLI will show matching windows with their titles and PIDs. Sufficient identifying info is stored in `backend_hint_id` to re-find the exact window instance on a subsequent run. If the originally-identified window is no longer available, the system falls back to "first match" and emits a warning. This policy lives in `orchestrator-core`, not in the `WindowLocator` trait itself.

### Action

An action defines what to do when triggered: repeat an input sequence at intervals, or execute a macro.

```rust
pub enum Action {
    /// Repeat an input sequence with optional jitter.
    /// Input is specified as an InputEvent (vocabulary type from orchestrator_input crate),
    /// reused directly to maintain consistency with the trait layer.
    Repeat {
        input: orchestrator_input::InputEvent,
        interval_ms: u64,
        jitter: Jitter,
    },
    /// Execute a sequence of steps, optionally looping.
    Macro {
        steps: Vec<MacroStep>,
        #[serde(rename = "loop")]
        loop_: bool,
    },
}
```

### Jitter

For repeat actions, optional jitter is applied to interval timing.

```rust
pub enum Jitter {
    /// No jitter applied.
    None,
    /// Uniform random jitter within a range (simpler, more predictable to configure
    /// than Gaussian; no extra rand_distr dependency).
    Uniform { range_ms: u64 },
}
```

Note: v1 uses uniform-only jitter as an explicit design decision. Gaussian jitter is deferred; the simpler uniform model is sufficient and avoids an extra dependency.

### MacroStep

A single step in a macro sequence, carrying a delay. Four variants cover the primary automation patterns.

```rust
pub enum MacroStep {
    KeyPress {
        combo: orchestrator_hotkey::KeyCombo,
        delay_ms: u32,
    },
    MouseClick {
        delay_ms: u32,
        button: orchestrator_input::MouseButton,
        position: ClickPosition,
    },
    Drag {
        delay_ms: u32,
        from: ClickPosition,
        to: ClickPosition,
    },
    Scroll {
        delay_ms: u32,
        dx: i32,
        dy: i32,
    },
}

pub enum ClickPosition {
    /// Absolute screen coordinates.
    Fixed { x: i32, y: i32 },
    /// Inject at the current cursor position (resolved at injection time).
    AtCursor,
}
```

### Authoring

**v1 macro authoring is manual only.** Users author profiles via:
- CLI commands or forms (populated in later increments)
- Direct JSON file editing

Live record-input mode is explicitly deferred; building a capture subsystem is a separate future task. This keeps the v1 scope focused and avoids UI complexity.

---

## 3. Configuration Format

Profiles are persisted in a single JSON file with versioning support.

### File Structure

```rust
pub struct Config {
    pub schema_version: u32,
    pub profiles: Vec<Profile>,
}

const CURRENT_SCHEMA_VERSION: u32 = 1;
```

**Schema migration:** Loading a config with `schema_version > CURRENT_SCHEMA_VERSION` is rejected with an error, requiring the user to upgrade Orchestrator. Migration code for `schema_version < CURRENT_SCHEMA_VERSION` will be added in future increments.

### Default Paths

Resolved via the `dirs` crate:

| Platform | Path |
|---|---|
| Linux | `~/.config/orchestrator/config.json` |
| macOS | `~/Library/Application Support/orchestrator/config.json` |

### I/O

Writes are atomic: temp file is created in the config directory, then renamed into place. `orchestrator-core` owns all serialization, deserialization, and validation. Both CLI and GUI consume the same core types, with no independent parsing logic in either.

---

## 4. Crate Architecture and Feature Flags

The workspace is structured to achieve zero platform-specific dependencies in `orchestrator-core`, enabling unit testability in isolation while preserving a clean trait-based extension boundary.

### Workspace Layout

```
orchestrator/
├── crates/
│   ├── orchestrator-core/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── profile.rs
│   │   │   ├── config.rs
│   │   │   ├── runner.rs
│   │   │   └── error.rs
│   │   └── Cargo.toml
│   │
│   ├── orchestrator-hotkey/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── vocab.rs          # KeyCombo, Modifier, HotkeyId, etc.
│   │   │   ├── trait.rs          # HotkeyBackend trait
│   │   │   ├── kde_kglobalaccel.rs (behind feature "kde")
│   │   │   ├── macos.rs           (behind feature "macos")
│   │   │   └── error.rs
│   │   └── Cargo.toml
│   │
│   ├── orchestrator-input/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── vocab.rs          # InputEvent, MouseButton, etc.
│   │   │   ├── trait.rs          # InputInjector trait
│   │   │   ├── linux_wayland.rs  (behind feature "wayland")
│   │   │   ├── macos.rs           (behind feature "macos")
│   │   │   └── error.rs
│   │   └── Cargo.toml
│   │
│   ├── orchestrator-window/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── vocab.rs          # WindowHandle, WindowInfo, etc.
│   │   │   ├── trait.rs          # WindowLocator trait
│   │   │   ├── kwin_dbus.rs      (behind feature "kde")
│   │   │   ├── macos.rs           (behind feature "macos")
│   │   │   └── error.rs
│   │   └── Cargo.toml
│   │
│   ├── orchestrator-cli/
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   └── commands/
│   │   └── Cargo.toml            # [target.'cfg(target_os = "...")'.dependencies]
│   │
│   └── orchestrator-gui/
│       ├── src/
│       ├── src-tauri/
│       └── Cargo.toml            # Tauri placeholder (no tauri dependency yet)
│
└── spike/
    └── wayland-injection-spike/  # Standalone binary (not a workspace member)
```

### Dependency Philosophy

- **`orchestrator-core`** has zero *default*-feature platform dependencies. It depends on the three trait crates (`orchestrator-hotkey`, `orchestrator-input`, `orchestrator-window`) with `default-features = false`.

- **Each trait crate** (`orchestrator-hotkey`, `orchestrator-input`, `orchestrator-window`) exports:
  - **Vocabulary types** (POD: `KeyCombo`, `InputEvent`, `WindowHandle`, etc.) with minimal deps: `serde`, `thiserror`. These are always available.
  - **Trait definitions** (e.g., `HotkeyBackend`, `InputInjector`, `WindowLocator`).
  - **Backend implementations** behind feature flags: `kde` for KDE Global Shortcuts + KWin D-Bus, `macos` for native macOS APIs, `wayland` for Wayland-specific injection paths.

- **`orchestrator-cli`** and **`orchestrator-gui`** select backends via:
  ```toml
  [target.'cfg(target_os = "linux")'.dependencies]
  orchestrator-core = { version = "0.1", features = ["kde", "wayland"] }

  [target.'cfg(target_os = "macos")'.dependencies]
  orchestrator-core = { version = "0.1", features = ["macos"] }
  ```

### Why This Works

`orchestrator-core` depends on the trait crates with `default-features = false`. This pulls in only the vocabulary types (serde+thiserror) without platform-specific dependencies like `zbus`, `ashpd`, `tokio`, or the macOS frameworks. Result: `orchestrator-core` is unit-testable in isolation, with mocked trait implementations, without linking any platform code.

The trait definitions themselves are generic Rust code. Backend implementations (KDE, macOS, etc.) live behind feature flags in the trait crates, selected at build time via Cargo features and `cfg(target_os)`. Each OS build compiles exactly one backend per trait: a Linux binary gets KDE hotkeys + Wayland/uinput injection + KWin window discovery; a macOS binary gets native APIs only.

**This is a deliberate YAGNI call:** no runtime-pluggable backends are needed. If a future need arises (e.g., supporting X11 alongside Wayland), it is reversible: add a Cargo feature, select it at build time (not runtime), and if core ever needs dynamic dispatch, a boxing layer can be added without disrupting call sites much (generics `fn run<H: HotkeyBackend, ...>()` become `dyn` boxed types).

---

## 5. Trait Signatures and Implementation Strategy

All three trait crates follow the same pattern: native `async fn` in trait definitions, with no `dyn` trait objects and no `async_trait` crate.

### Rationale for Native Async Traits

Backend selection is **compile-time only** via Cargo features and `cfg(target_os)`. Each OS binary compiles exactly one set of real backends, so:

- No runtime polymorphism is needed.
- Generic trait bounds (`fn run<H: HotkeyBackend, I: InputInjector, W: WindowLocator>()`) are monomorphized at compile time into concrete types.
- Native `async fn` in traits (stable since Rust 1.75) is simpler, more efficient, and requires no extra dependency compared to the `async_trait` macro.

If a future requirement for dynamic dispatch emerges, it is straightforward to add a boxing layer without disrupting existing call sites.

### orchestrator-hotkey

Manages global hotkey registration and event dispatch.

```rust
// Vocabulary types

pub struct KeyCombo {
    pub modifiers: Vec<Modifier>,
    pub key: String, // Portable key name, e.g. "F9", "Return", "Super_L"
}

pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Meta,
}

pub struct HotkeyId(pub u64);

pub enum HotkeyEvent {
    Pressed,
    Released,
}

pub enum HotkeyError {
    RegistrationFailed(String),
    AlreadyRegistered(KeyCombo),
    BackendUnavailable(String),
    Io(std::io::Error),
}

// Trait

pub trait HotkeyBackend: Send + Sync {
    /// Register a hotkey combination for a named action.
    /// Returns a unique ID for later unregistration.
    async fn register(&self, combo: &KeyCombo, action_name: &str) -> Result<HotkeyId, HotkeyError>;

    /// Unregister a previously registered hotkey.
    async fn unregister(&self, id: HotkeyId) -> Result<(), HotkeyError>;

    /// Single event stream for all registrations on this backend instance.
    /// Returns an unbounded MPSC receiver emitting (HotkeyId, HotkeyEvent) tuples.
    fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<(HotkeyId, HotkeyEvent)>;

    /// Human-readable backend name for diagnostics.
    fn backend_name(&self) -> &'static str;
}
```

**Implementations:**
- `orchestrator-hotkey/src/kde_kglobalaccel.rs` (feature: `kde`): Registers via D-Bus `org.kde.kglobalaccel`, dispatches events via the same interface.
- `orchestrator-hotkey/src/macos.rs` (feature: `macos`): Native macOS hotkey APIs.

### orchestrator-input

Manages synthetic input injection.

```rust
// Vocabulary types

pub enum InputEvent {
    KeyPress(KeyCombo),
    KeyRelease(KeyCombo),
    MouseButtonPress(MouseButton),
    MouseButtonRelease(MouseButton),
    MouseMoveAbsolute { x: i32, y: i32 },
    MouseMoveRelative { dx: i32, dy: i32 },
    Scroll { dx: i32, dy: i32 },
}

pub enum MouseButton {
    Left,
    Right,
    Middle,
}

pub enum InjectError {
    NotPermitted(String),
    BackendUnavailable(String),
    Io(std::io::Error),
    Unsupported(String),
}

// Trait

pub trait InputInjector: Send + Sync {
    /// Establish a connection to the input system (D-Bus, uinput, etc.).
    /// Must be called before inject().
    async fn connect(&mut self) -> Result<(), InjectError>;

    /// Inject a single input event.
    async fn inject(&self, event: &InputEvent) -> Result<(), InjectError>;

    /// Query whether this backend requires focus steal for window targeting.
    /// Always true on Wayland backends today; false on X11/macOS with native window targeting.
    /// Exposed explicitly so core and UI never silently assume true no-focus-steal targeting.
    fn requires_focus_steal_for_window_targeting(&self) -> bool;

    /// Human-readable backend name for diagnostics.
    fn backend_name(&self) -> &'static str;
}
```

**Implementations:**
- `orchestrator-input/src/linux_wayland.rs` (feature: `wayland`): Injects via `ydotool`/uinput or `ashpd::RemoteDesktop` portal. Sets `requires_focus_steal_for_window_targeting() -> true`.
- `orchestrator-input/src/macos.rs` (feature: `macos`): Native macOS CGEvent APIs.

### orchestrator-window

Manages window enumeration, activation, and disambiguation.

```rust
// Vocabulary types

pub struct WindowHandle(pub String); // Opaque backend identifier, e.g. KWin UUID

pub struct WindowInfo {
    pub handle: WindowHandle,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub title: String,
}

pub enum WindowError {
    BackendUnavailable(String),
    NotFound,
    Io(std::io::Error),
}

// Trait

pub trait WindowLocator: Send + Sync {
    /// List all visible windows on the desktop.
    async fn list_windows(&self) -> Result<Vec<WindowInfo>, WindowError>;

    /// Get the currently keyboard-focused window.
    async fn focused_window(&self) -> Result<Option<WindowHandle>, WindowError>;

    /// Activate (raise and focus) the given window.
    async fn activate_window(&self, handle: &WindowHandle) -> Result<(), WindowError>;
}
```

**Implementations:**
- `orchestrator-window/src/kwin_dbus.rs` (feature: `kde`): D-Bus `org.kde.KWin` scripting API for window enumeration and activation.
- `orchestrator-window/src/macos.rs` (feature: `macos`): Native macOS Accessibility APIs.

---

## 6. Spike Plan: Wayland Input Injection

A standalone spike binary will empirically verify whether no-focus-steal window targeting is achievable on Wayland/KWin. This is not a workspace member; it lives at `spike/wayland-injection-spike/`.

### Spike Scope

The binary will:

1. **Register a shortcut** via `org.kde.kglobalaccel` D-Bus interface.
2. **Enumerate and activate windows** via `org.kde.KWin` scripting D-Bus.
3. **Test three injection approaches** against a real target window:
   - `ydotool` / `uinput` kernel API
   - `libei` via `ashpd::RemoteDesktop` portal
   - KWin activate → inject → restore focus (to measure flicker and focus behavior)
4. **Record findings** for each approach:
   - Does input reliably land in the target window?
   - Is focus visible flicker observed?
   - What setup and permissions are required?

### Expected Outcome (Prior)

**No-focus-steal targeting is unlikely to be achievable on Wayland.** The spike is designed to confirm or refute this empirically. If confirmed, the design consequence is:

- `Scope::Window` with `focus_steal: false` is not a realistic option on Wayland.
- `Window`-scoped actions on Wayland will use activate → inject → restore (always visible focus flicker).
- The Wayland `InputInjector` backend will report `requires_focus_steal_for_window_targeting() -> true`.

**However**, this prior is not final. The findings document (written separately as part of a different task) is the source of truth once the spike is executed.

---

## 7. Out of Scope for This Increment

**Full command implementations** in `orchestrator-cli`, the complete **GUI Tauri app and UI**, real **macOS backend implementations**, **CI/GitHub Actions**, **packaging** (`cargo-deb`, `.app` bundling), and **autostart integration** are explicitly out of scope for v1. These are populated in future increments or as separate efforts.

**Product-level v1 non-goals:** Chorded or sequence triggers (single key combo only), Windows backend implementation (the trait boundary is preserved to avoid breakage, but no real implementation), X11 backend (Wayland is the focus; X11 support may follow as a Cargo feature if it proves trivial to implement alongside Wayland, but it is not required).

---

## 8. Toolchain and Dependencies

The following versions are verified as of August 2026 and should be cited exactly in Cargo manifests:

| Crate | Version | Notes |
|---|---|---|
| `zbus` | 5.18.0 | Canonical repository is now `github.com/z-galaxy/zbus` (crates.io package name unchanged); cite the new org in links and documentation. |
| `ashpd` | 0.13.13 | Use simple `RemoteDesktop::notify_keyboard_keycode`, `notify_pointer_button`, `notify_pointer_motion` methods first; fallback to `connect_to_eis` / raw libei only if needed. |
| `tauri` | 2.11.5 | v2 is current major. No Tauri dependency in v1 core; scaffold placeholder only. |
| `clap` | 4.6.6 | Derive API; CLI uses this for argument parsing. |
| `cargo-deb` | 3.7.0 | Derives package metadata directly from `Cargo.toml` (no second source to drift); handles multi-arch via `--target`. Deferred to packaging phase. |
| `tokio` | 1.x | Async runtime; version pinned per workspace resolution. |
| `serde` | 1.0.x | Serialization; used by all trait crates for vocabulary types. |
| `thiserror` | 1.x | Error trait derive; used by all trait crates. |
| `dirs` | 5.x | Platform-specific config/cache paths; Linux/macOS support. |

**External tools:**
- **`ydotool`** / **uinput kernel API**: Still the practical Linux/Wayland synthetic input path as of 2026.
- **`kdotool`** (`jinliu/kdotool`): Prior art for KWin scripting window enumeration and activation; may inform spike implementation.

