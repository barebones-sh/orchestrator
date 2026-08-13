//! Real Linux/Wayland input-injection backend, built on the `ydotool` CLI
//! (which talks to the `ydotoold` uinput daemon over a Unix socket). This is
//! the confirmed-working mechanism from the Wayland injection spike; see
//! `docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md`
//! (Phase 3a/3b) for the full story of *why* `ydotool`/uinput was chosen
//! over the `ashpd::desktop::remote_desktop::RemoteDesktop` (`libei`)
//! alternative: both work and are equally "global" (focus-following, not
//! independently targetable -- confirmed for both), but `ydotool` needs only
//! a one-time system setup (an `input`-group membership plus the
//! `ydotoold` user service) versus a runtime permission dialog whose
//! grant-persistence-across-restarts behavior the spike left unconfirmed.
//! The `RemoteDesktop`/`libei` path is *not* implemented here -- the
//! findings doc recommends deferring it, and this task doesn't add it.
//!
//! The only command shape the spike actually ran live is a single `KEY_A`
//! press+release via `ydotool key <code>:1 <code>:0`, ported near-verbatim
//! from `spike/wayland-injection-spike/src/activate_inject_restore.rs`'s
//! `inject_key`. Everything else below (mouse clicks, moves, scroll, and
//! multi-key/modifier chords) extends that pattern onto the rest of
//! `InputEvent` based on `ydotool` 1.0.4's real CLI surface as discovered by
//! running `ydotool --help` / `ydotool click --help` / `ydotool mousemove
//! --help` on the development machine (**not** covered by the spike) --
//! see this module's doc comments on [`mouse_button_code`], [`build_click_args`],
//! and [`build_scroll_args`] for exactly what was found. **Live injection
//! against a real `ydotoold` for these newly-added mappings (mouse
//! clicks/scroll/moves) is NOT re-verified by this task** -- only the
//! `KEY_A` case was ever confirmed to actually land, by the spike itself. A
//! human-in-the-loop pass covers the rest later.
//!
//! ## `ydotool` 1.0.4 CLI surface, as found on this machine
//!
//! `ydotool --help` lists: `click`, `mousemove`, `type`, `key`, `debug`,
//! `bakers`. No dedicated scroll/wheel subcommand exists -- wheel motion is
//! a *flag* on `mousemove`, not a separate subcommand (see
//! [`build_scroll_args`]).
//!
//! - `ydotool key <keycode>:<0|1> ...` -- exactly what the spike used.
//! - `ydotool click [BUTTONS]...` -- buttons are single hex bytes combining
//!   a base button id with an up/down bitmask in the *same* byte, not
//!   separate press/release flags: `0x00`=LEFT, `0x01`=RIGHT, `0x02`=MIDDLE
//!   (also `0x03`=SIDE, `0x04`=EXTR, `0x05`=FORWARD, `0x06`=BACK, `0x07`=TASK,
//!   unused by this crate's `MouseButton`), OR'd with `0x40` (down/press) or
//!   `0x80` (up/release) -- e.g. the help text's own example `0xC0` is a full
//!   left click (`0x40 | 0x80 | 0x00`), `0x41` is right-button-down alone.
//!   See [`build_click_args`].
//! - `ydotool mousemove [-a|--absolute] [-w|--wheel] -- <x> <y>` -- one
//!   subcommand handles absolute pointer motion, relative pointer motion
//!   (the default, no flag), *and* wheel/scroll motion (`--wheel`, always
//!   relative -- the help text notes `--absolute` "not applicable to
//!   wheel"). There is no separate `scroll`/`wheel` subcommand to discover;
//!   `--wheel` on `mousemove` *is* the scroll mechanism. See
//!   [`build_mousemove_absolute_args`], [`build_mousemove_relative_args`],
//!   [`build_scroll_args`].

use std::path::PathBuf;

use orchestrator_hotkey::{KeyCombo, Modifier};
use tokio::process::Command;

use crate::error::InjectError;
use crate::trait_def::InputInjector;
use crate::vocab::{InputEvent, MouseButton};

/// Real Linux/Wayland `InputInjector`, built on shelling out to the
/// `ydotool` CLI. See the module doc comment for the mechanism and what was
/// (and wasn't) re-verified live.
pub struct YdotoolInputInjector {
    /// Resolved once at construction time, mirroring `ydotool`'s own
    /// resolution order: `YDOTOOL_SOCKET` env var if set, else
    /// `/run/user/<uid>/.ydotool_socket`.
    socket_path: PathBuf,
}

impl YdotoolInputInjector {
    /// Resolve the socket path per `ydotool`'s own precedence (`YDOTOOL_SOCKET`
    /// env var, else the default per-uid path) and construct the backend.
    pub fn new() -> Self {
        Self {
            socket_path: resolve_socket_path(std::env::var("YDOTOOL_SOCKET").ok()),
        }
    }

    /// Construct with an explicit socket path, bypassing env/uid resolution.
    /// Exposed for tests and for callers that already know the path.
    pub fn with_socket_path(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
        }
    }

    /// Spawn `ydotool <args>` with `YDOTOOL_SOCKET` pointed at our resolved
    /// socket path, exactly as the spike's `inject_key` did (there, the path
    /// was hardcoded; here it's resolved via [`resolve_socket_path`]).
    async fn run_ydotool(&self, args: &[String]) -> Result<(), InjectError> {
        let status = Command::new("ydotool")
            .env("YDOTOOL_SOCKET", &self.socket_path)
            .args(args)
            .status()
            .await
            .map_err(|e| {
                InjectError::BackendUnavailable(format!("failed to spawn `ydotool`: {e}"))
            })?;
        if !status.success() {
            return Err(InjectError::BackendUnavailable(format!(
                "ydotool exited with {status} (args: {args:?})"
            )));
        }
        Ok(())
    }
}

impl Default for YdotoolInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector for YdotoolInputInjector {
    /// Verify the `ydotoold` Unix socket is reachable. Does not attempt to
    /// start/fix the daemon itself -- per the brief, that's out of scope;
    /// this only detects the problem and surfaces an actionable error
    /// pointing at the findings doc's setup steps (`input` group +
    /// `ydotoold` user service).
    ///
    /// Uses `UnixDatagram::connect`, not `UnixStream::connect`: `ydotoold`'s
    /// socket is `SOCK_DGRAM`, not `SOCK_STREAM` (confirmed empirically
    /// against a real running `ydotoold` -- a stream connect attempt fails
    /// with `EPROTOTYPE`, "Protocol wrong type for socket", even though the
    /// daemon is up and `inject()`'s real `ydotool` CLI calls work fine
    /// against the same socket).
    async fn connect(&mut self) -> Result<(), InjectError> {
        std::os::unix::net::UnixDatagram::unbound()
            .and_then(|sock| sock.connect(&self.socket_path))
            .map_err(|e| {
                InjectError::BackendUnavailable(format!(
                    "ydotoold socket not reachable at {}: {e}. Ensure your user is in the \
                     `input` group and the `ydotoold` user service is enabled and running -- \
                     see the setup steps in \
                     docs/superpowers/specs/2026-08-09-wayland-injection-spike-findings.md \
                     (Phase 3a).",
                    self.socket_path.display()
                ))
            })?;
        Ok(())
    }

    /// Map `event` to a `ydotool` invocation and shell out. See the module
    /// doc comment and [`build_command_args`] for the mapping.
    async fn inject(&self, event: &InputEvent) -> Result<(), InjectError> {
        let args = build_command_args(event)?;
        self.run_ydotool(&args).await
    }

    /// Always true on this Wayland backend -- synthetic input via
    /// `ydotool`/uinput is global/focus-following, never independently
    /// targetable at a specific window. Confirmed by the spike for both the
    /// `ydotool` and `libei` injection paths (Phase 3a/3b).
    fn requires_focus_steal_for_window_targeting(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "ydotool"
    }
}

/// `ydotool`'s own socket-path resolution order: `YDOTOOL_SOCKET` env var if
/// set, else `/run/user/<uid>/.ydotool_socket`. Pure/testable (the env
/// lookup itself happens in [`YdotoolInputInjector::new`]; this function
/// takes the already-looked-up value so it can be tested without mutating
/// process-global env state).
fn resolve_socket_path(env_override: Option<String>) -> PathBuf {
    if let Some(path) = env_override {
        return PathBuf::from(path);
    }
    PathBuf::from(format!("/run/user/{}/.ydotool_socket", current_uid()))
}

/// The current process's real uid, without pulling in a `libc`/`nix`
/// dependency: `/proc/self` is always owned by the calling process's real
/// uid, so its metadata's `uid()` (from `std::os::unix::fs::MetadataExt`,
/// already in `std`) gives us the number for free. Falls back to `0`
/// (root's own default path, which will simply fail to connect rather than
/// panic) if `/proc` is unavailable for any reason.
fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0)
}

/// Portable key name -> Linux evdev `KEY_*` code, covering exactly the set
/// the brief requires: `A`-`Z`, `0`-`9`, `F1`-`F12`, `Return`/`Enter`,
/// `Escape`, `Tab`, `Space`, `Backspace`, `Delete`, `Up`/`Down`/`Left`/
/// `Right`, `Home`/`End`/`PageUp`/`PageDown`. Case-insensitive. Codes are
/// the standard, stable `linux/input-event-codes.h` values (the same table
/// `KEY_A = 30` used by the spike comes from) -- not re-derived from any
/// live source, since this header's numbering has been kernel ABI for
/// decades. Returns `None` for anything not in that list; callers turn that
/// into `InjectError::Unsupported`, never a panic.
fn key_name_to_evdev(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    Some(match upper.as_str() {
        "A" => 30,
        "B" => 48,
        "C" => 46,
        "D" => 32,
        "E" => 18,
        "F" => 33,
        "G" => 34,
        "H" => 35,
        "I" => 23,
        "J" => 36,
        "K" => 37,
        "L" => 38,
        "M" => 50,
        "N" => 49,
        "O" => 24,
        "P" => 25,
        "Q" => 16,
        "R" => 19,
        "S" => 31,
        "T" => 20,
        "U" => 22,
        "V" => 47,
        "W" => 17,
        "X" => 45,
        "Y" => 21,
        "Z" => 44,
        "0" => 11,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        "RETURN" | "ENTER" => 28,
        "ESCAPE" => 1,
        "TAB" => 15,
        "SPACE" => 57,
        "BACKSPACE" => 14,
        "DELETE" => 111,
        "UP" => 103,
        "DOWN" => 108,
        "LEFT" => 105,
        "RIGHT" => 106,
        "HOME" => 102,
        "END" => 107,
        "PAGEUP" => 104,
        "PAGEDOWN" => 109,
        _ => return None,
    })
}

/// Each `Modifier` variant's own physical-key evdev code (the *left* variant
/// of each, per the brief): `Ctrl`->`KEY_LEFTCTRL`(29), `Shift`->
/// `KEY_LEFTSHIFT`(42), `Alt`->`KEY_LEFTALT`(56), `Meta`->`KEY_LEFTMETA`(125).
/// Unlike [`key_name_to_evdev`] this is total -- every `Modifier` variant has
/// a well-defined physical key, so there's no `Unsupported` case here.
fn modifier_to_evdev(modifier: &Modifier) -> u32 {
    match modifier {
        Modifier::Ctrl => 29,
        Modifier::Shift => 42,
        Modifier::Alt => 56,
        Modifier::Meta => 125,
    }
}

/// Build `ydotool key ...` args for a `KeyPress`: modifiers first, then the
/// key, in listed order, all with state `1` (pressed, no release) -- per the
/// brief, mirrors how a real chord is physically pressed (modifiers down
/// before the main key).
fn build_key_press_args(combo: &KeyCombo) -> Result<Vec<String>, InjectError> {
    let key_code = key_name_to_evdev(&combo.key)
        .ok_or_else(|| InjectError::Unsupported(format!("unrecognized key: {}", combo.key)))?;
    let mut args = vec!["key".to_string()];
    for modifier in &combo.modifiers {
        args.push(format!("{}:1", modifier_to_evdev(modifier)));
    }
    args.push(format!("{key_code}:1"));
    Ok(args)
}

/// Build `ydotool key ...` args for a `KeyRelease`: the key releases first,
/// then modifiers in *reverse* listed order, all with state `0` -- per the
/// brief, mirrors correct chord-release order and avoids stuck modifiers
/// (releasing a modifier before the key it was guarding would let e.g. a
/// bare `A` land immediately after the intended `Ctrl+A`).
fn build_key_release_args(combo: &KeyCombo) -> Result<Vec<String>, InjectError> {
    let key_code = key_name_to_evdev(&combo.key)
        .ok_or_else(|| InjectError::Unsupported(format!("unrecognized key: {}", combo.key)))?;
    let mut args = vec!["key".to_string(), format!("{key_code}:0")];
    for modifier in combo.modifiers.iter().rev() {
        args.push(format!("{}:0", modifier_to_evdev(modifier)));
    }
    Ok(args)
}

/// This crate's `MouseButton` -> `ydotool click`'s base button id (the low
/// nibble before OR-ing in the press/release bitmask -- see the module doc
/// comment's CLI-surface notes and [`build_click_args`]).
fn mouse_button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0x00,
        MouseButton::Right => 0x01,
        MouseButton::Middle => 0x02,
    }
}

/// Build `ydotool click <byte>` args. `ydotool click` has no separate
/// press/release flag -- press/release is encoded as a bitmask OR'd into the
/// *same* byte as the button id: `0x40` = down/press, `0x80` = up/release
/// (see the module doc comment's CLI-surface notes, straight from `ydotool
/// click --help`'s own worked examples). Formatted as `0x`-prefixed
/// lowercase hex to match that help text's own examples.
fn build_click_args(button: MouseButton, pressed: bool) -> Vec<String> {
    let mask: u8 = if pressed { 0x40 } else { 0x80 };
    let value = mask | mouse_button_code(button);
    vec!["click".to_string(), format!("0x{value:02x}")]
}

/// Build `ydotool mousemove --absolute -- x y` args, per the brief's
/// specified shape.
fn build_mousemove_absolute_args(x: i32, y: i32) -> Vec<String> {
    vec![
        "mousemove".to_string(),
        "--absolute".to_string(),
        "--".to_string(),
        x.to_string(),
        y.to_string(),
    ]
}

/// Build `ydotool mousemove -- dx dy` args (relative motion is `mousemove`'s
/// default when `--absolute` is omitted), per the brief's specified shape.
fn build_mousemove_relative_args(dx: i32, dy: i32) -> Vec<String> {
    vec![
        "mousemove".to_string(),
        "--".to_string(),
        dx.to_string(),
        dy.to_string(),
    ]
}

/// Build args for `Scroll`. As documented in the module doc comment,
/// `ydotool` 1.0.4 has no dedicated scroll/wheel subcommand -- wheel motion
/// is `mousemove --wheel`, always relative (its help text notes
/// `--absolute` is "not applicable to wheel"). Not live-tested by this task
/// (see the module doc comment and the task report); this is the most
/// direct reading of the discovered CLI surface, not a re-verified mapping.
fn build_scroll_args(dx: i32, dy: i32) -> Vec<String> {
    vec![
        "mousemove".to_string(),
        "--wheel".to_string(),
        "--".to_string(),
        dx.to_string(),
        dy.to_string(),
    ]
}

/// Map any `InputEvent` to the `ydotool` argument list that would inject it.
/// Pure and fully testable without spawning a process -- see the `tests`
/// module. This is the single source of truth `inject()` calls; no parallel
/// logic exists elsewhere.
fn build_command_args(event: &InputEvent) -> Result<Vec<String>, InjectError> {
    match event {
        InputEvent::KeyPress(combo) => build_key_press_args(combo),
        InputEvent::KeyRelease(combo) => build_key_release_args(combo),
        InputEvent::MouseButtonPress(button) => Ok(build_click_args(*button, true)),
        InputEvent::MouseButtonRelease(button) => Ok(build_click_args(*button, false)),
        InputEvent::MouseMoveAbsolute { x, y } => Ok(build_mousemove_absolute_args(*x, *y)),
        InputEvent::MouseMoveRelative { dx, dy } => Ok(build_mousemove_relative_args(*dx, *dy)),
        InputEvent::Scroll { dx, dy } => Ok(build_scroll_args(*dx, *dy)),
    }
}

/// Only used by tests, to assert the exists-but-unconnectable-socket branch
/// of `connect()` without depending on a real `/proc/self`-derived uid.
#[cfg(test)]
fn socket_path_for_test(injector: &YdotoolInputInjector) -> &std::path::Path {
    &injector.socket_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_hotkey::KeyCombo;
    use std::path::Path;

    // -- resolve_socket_path ----------------------------------------------

    #[test]
    fn resolve_socket_path_prefers_env_override() {
        let path = resolve_socket_path(Some("/tmp/custom.sock".to_string()));
        assert_eq!(path, PathBuf::from("/tmp/custom.sock"));
    }

    #[test]
    fn resolve_socket_path_falls_back_to_default_per_uid_path() {
        let path = resolve_socket_path(None);
        let s = path.to_string_lossy();
        assert!(s.starts_with("/run/user/"));
        assert!(s.ends_with("/.ydotool_socket"));
    }

    // -- key_name_to_evdev --------------------------------------------------

    #[test]
    fn recognizes_letters() {
        assert_eq!(key_name_to_evdev("A"), Some(30));
        assert_eq!(
            key_name_to_evdev("a"),
            Some(30),
            "should be case-insensitive"
        );
        assert_eq!(key_name_to_evdev("Z"), Some(44));
    }

    #[test]
    fn recognizes_digits() {
        assert_eq!(key_name_to_evdev("0"), Some(11));
        assert_eq!(key_name_to_evdev("1"), Some(2));
        assert_eq!(key_name_to_evdev("9"), Some(10));
    }

    #[test]
    fn recognizes_function_keys() {
        assert_eq!(key_name_to_evdev("F1"), Some(59));
        assert_eq!(key_name_to_evdev("F9"), Some(67));
        assert_eq!(key_name_to_evdev("F12"), Some(88));
    }

    #[test]
    fn recognizes_named_keys() {
        assert_eq!(key_name_to_evdev("Return"), Some(28));
        assert_eq!(key_name_to_evdev("Enter"), Some(28));
        assert_eq!(key_name_to_evdev("Escape"), Some(1));
        assert_eq!(key_name_to_evdev("Tab"), Some(15));
        assert_eq!(key_name_to_evdev("Space"), Some(57));
        assert_eq!(key_name_to_evdev("Backspace"), Some(14));
        assert_eq!(key_name_to_evdev("Delete"), Some(111));
        assert_eq!(key_name_to_evdev("Up"), Some(103));
        assert_eq!(key_name_to_evdev("Down"), Some(108));
        assert_eq!(key_name_to_evdev("Left"), Some(105));
        assert_eq!(key_name_to_evdev("Right"), Some(106));
        assert_eq!(key_name_to_evdev("Home"), Some(102));
        assert_eq!(key_name_to_evdev("End"), Some(107));
        assert_eq!(key_name_to_evdev("PageUp"), Some(104));
        assert_eq!(key_name_to_evdev("PageDown"), Some(109));
    }

    #[test]
    fn unrecognized_key_returns_none() {
        assert_eq!(key_name_to_evdev("NotAKey"), None);
        assert_eq!(key_name_to_evdev(""), None);
    }

    // -- modifier_to_evdev --------------------------------------------------

    #[test]
    fn modifier_codes_match_left_variants() {
        assert_eq!(modifier_to_evdev(&Modifier::Ctrl), 29);
        assert_eq!(modifier_to_evdev(&Modifier::Shift), 42);
        assert_eq!(modifier_to_evdev(&Modifier::Alt), 56);
        assert_eq!(modifier_to_evdev(&Modifier::Meta), 125);
    }

    // -- build_key_press_args / build_key_release_args ----------------------

    #[test]
    fn key_press_with_no_modifiers() {
        let combo = KeyCombo {
            modifiers: vec![],
            key: "A".to_string(),
        };
        assert_eq!(build_key_press_args(&combo).unwrap(), vec!["key", "30:1"]);
    }

    #[test]
    fn key_press_presses_modifiers_before_key_in_listed_order() {
        let combo = KeyCombo {
            modifiers: vec![Modifier::Ctrl, Modifier::Alt],
            key: "Delete".to_string(),
        };
        assert_eq!(
            build_key_press_args(&combo).unwrap(),
            vec!["key", "29:1", "56:1", "111:1"]
        );
    }

    #[test]
    fn key_release_releases_key_before_modifiers_in_reverse_order() {
        let combo = KeyCombo {
            modifiers: vec![Modifier::Ctrl, Modifier::Alt],
            key: "Delete".to_string(),
        };
        assert_eq!(
            build_key_release_args(&combo).unwrap(),
            vec!["key", "111:0", "56:0", "29:0"]
        );
    }

    #[test]
    fn key_press_unrecognized_key_is_unsupported_not_panic() {
        let combo = KeyCombo {
            modifiers: vec![],
            key: "Nonexistent".to_string(),
        };
        match build_key_press_args(&combo) {
            Err(InjectError::Unsupported(msg)) => assert!(msg.contains("Nonexistent")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn key_release_unrecognized_key_is_unsupported_not_panic() {
        let combo = KeyCombo {
            modifiers: vec![],
            key: "Nonexistent".to_string(),
        };
        match build_key_release_args(&combo) {
            Err(InjectError::Unsupported(msg)) => assert!(msg.contains("Nonexistent")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // -- build_click_args -----------------------------------------------------

    #[test]
    fn click_press_left_matches_help_texts_down_mask() {
        // 0x40 | 0x00 (LEFT) = 0x40.
        assert_eq!(
            build_click_args(MouseButton::Left, true),
            vec!["click", "0x40"]
        );
    }

    #[test]
    fn click_release_left_matches_help_texts_up_mask() {
        // 0x80 | 0x00 (LEFT) = 0x80.
        assert_eq!(
            build_click_args(MouseButton::Left, false),
            vec!["click", "0x80"]
        );
    }

    #[test]
    fn click_press_right_matches_help_texts_worked_example() {
        // The ydotool click --help text's own example: 0x41 = right button down.
        assert_eq!(
            build_click_args(MouseButton::Right, true),
            vec!["click", "0x41"]
        );
    }

    #[test]
    fn click_release_middle_matches_help_texts_worked_example() {
        // The ydotool click --help text's own example: 0x82 = middle button up.
        assert_eq!(
            build_click_args(MouseButton::Middle, false),
            vec!["click", "0x82"]
        );
    }

    // -- build_mousemove_absolute_args / build_mousemove_relative_args --------

    #[test]
    fn mousemove_absolute_args_shape() {
        assert_eq!(
            build_mousemove_absolute_args(100, 200),
            vec!["mousemove", "--absolute", "--", "100", "200"]
        );
    }

    #[test]
    fn mousemove_relative_args_shape() {
        assert_eq!(
            build_mousemove_relative_args(-5, 10),
            vec!["mousemove", "--", "-5", "10"]
        );
    }

    // -- build_scroll_args ----------------------------------------------------

    #[test]
    fn scroll_args_use_mousemove_wheel_flag() {
        assert_eq!(
            build_scroll_args(-1, 3),
            vec!["mousemove", "--wheel", "--", "-1", "3"]
        );
    }

    // -- build_command_args (the InputEvent -> args dispatch itself) ----------

    #[test]
    fn dispatches_all_event_variants_to_the_right_builder() {
        assert_eq!(
            build_command_args(&InputEvent::KeyPress(KeyCombo {
                modifiers: vec![],
                key: "A".to_string(),
            }))
            .unwrap(),
            vec!["key", "30:1"]
        );
        assert_eq!(
            build_command_args(&InputEvent::KeyRelease(KeyCombo {
                modifiers: vec![],
                key: "A".to_string(),
            }))
            .unwrap(),
            vec!["key", "30:0"]
        );
        assert_eq!(
            build_command_args(&InputEvent::MouseButtonPress(MouseButton::Left)).unwrap(),
            vec!["click", "0x40"]
        );
        assert_eq!(
            build_command_args(&InputEvent::MouseButtonRelease(MouseButton::Left)).unwrap(),
            vec!["click", "0x80"]
        );
        assert_eq!(
            build_command_args(&InputEvent::MouseMoveAbsolute { x: 1, y: 2 }).unwrap(),
            vec!["mousemove", "--absolute", "--", "1", "2"]
        );
        assert_eq!(
            build_command_args(&InputEvent::MouseMoveRelative { dx: 1, dy: 2 }).unwrap(),
            vec!["mousemove", "--", "1", "2"]
        );
        assert_eq!(
            build_command_args(&InputEvent::Scroll { dx: 0, dy: 1 }).unwrap(),
            vec!["mousemove", "--wheel", "--", "0", "1"]
        );
    }

    // -- struct-level plumbing ------------------------------------------------

    #[test]
    fn with_socket_path_uses_the_given_path_verbatim() {
        let injector = YdotoolInputInjector::with_socket_path("/tmp/does-not-exist.sock");
        assert_eq!(
            socket_path_for_test(&injector),
            Path::new("/tmp/does-not-exist.sock")
        );
    }

    #[tokio::test]
    async fn connect_reports_backend_unavailable_for_missing_socket() {
        // No live ydotoold required/contacted: this path does not exist, so
        // connect() must fail fast with an actionable BackendUnavailable
        // rather than hang or panic.
        let mut injector =
            YdotoolInputInjector::with_socket_path("/tmp/orchestrator-input-test-nonexistent.sock");
        let result = injector.connect().await;
        match result {
            Err(InjectError::BackendUnavailable(msg)) => {
                assert!(msg.contains("wayland-injection-spike-findings.md"));
            }
            other => panic!("expected BackendUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_succeeds_against_a_real_datagram_socket() {
        // Regression test for the SOCK_DGRAM-vs-SOCK_STREAM bug:
        // `ydotoold` binds a `SOCK_DGRAM` Unix socket, not `SOCK_STREAM`, so
        // connect() must use `UnixDatagram`, not `UnixStream`, or it will
        // falsely report the backend unavailable even when a real daemon is
        // listening. This binds a real `UnixDatagram` at a temp path
        // (simulating what `ydotoold` does) and asserts connect() succeeds.
        let dir = std::env::temp_dir().join(format!(
            "orchestrator-input-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("ydotoold.sock");

        let _listener = std::os::unix::net::UnixDatagram::bind(&socket_path)
            .expect("failed to bind test UnixDatagram socket");

        let mut injector = YdotoolInputInjector::with_socket_path(&socket_path);
        let result = injector.connect().await;
        assert!(
            result.is_ok(),
            "expected connect() to succeed against a real SOCK_DGRAM socket, got {result:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn requires_focus_steal_is_true() {
        let injector = YdotoolInputInjector::with_socket_path("/tmp/whatever.sock");
        assert!(injector.requires_focus_steal_for_window_targeting());
    }

    #[test]
    fn backend_name_is_ydotool() {
        let injector = YdotoolInputInjector::with_socket_path("/tmp/whatever.sock");
        assert_eq!(injector.backend_name(), "ydotool");
    }
}
