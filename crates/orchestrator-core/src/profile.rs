//! Core `Profile` and `Scope` data model (design spec §2 "Core Profile" / "Scope").

use serde::{Deserialize, Serialize};

use crate::action::Action;

/// A `Profile` is the unit of automation: a hotkey trigger bound to an
/// action, scoped to a target, with optional debouncing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub trigger: orchestrator_hotkey::KeyCombo,
    pub action: Action,
    pub scope: Scope,
    pub focus_steal: bool,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u32,
}

fn default_debounce_ms() -> u32 {
    400
}

/// Target selection: either the desktop (no window targeting) or a
/// specific window matched by process name and/or title hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, Jitter};

    fn sample_profile(scope: Scope) -> Profile {
        Profile {
            name: "test-profile".to_string(),
            trigger: orchestrator_hotkey::KeyCombo {
                modifiers: vec![orchestrator_hotkey::Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: orchestrator_input::InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 100,
                jitter: Jitter::None,
            },
            scope,
            focus_steal: true,
            debounce_ms: 400,
        }
    }

    #[test]
    fn scope_desktop_serde_round_trip() {
        let scope = Scope::Desktop;
        let json = serde_json::to_string(&scope).unwrap();
        let back: Scope = serde_json::from_str(&json).unwrap();
        assert_eq!(scope, back);
    }

    #[test]
    fn scope_window_serde_round_trip() {
        let scope = Scope::Window {
            process_name: "firefox".to_string(),
            window_title_hint: "Mozilla Firefox".to_string(),
            backend_hint_id: Some("uuid-1234".to_string()),
        };
        let json = serde_json::to_string(&scope).unwrap();
        let back: Scope = serde_json::from_str(&json).unwrap();
        assert_eq!(scope, back);
    }

    #[test]
    fn profile_debounce_ms_defaults_to_400() {
        let json = r#"{
            "name": "no-debounce",
            "trigger": { "modifiers": ["Ctrl"], "key": "F9" },
            "action": { "Repeat": { "input": { "Scroll": { "dx": 0, "dy": 1 } }, "interval_ms": 100, "jitter": "None" } },
            "scope": "Desktop",
            "focus_steal": true
        }"#;
        let profile: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(profile.debounce_ms, 400);
    }

    #[test]
    fn sample_profile_smoke() {
        let profile = sample_profile(Scope::Desktop);
        let json = serde_json::to_string(&profile).unwrap();
        let back: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(profile, back);
    }

    /// Fixed-string JSON fixture, hand-written independently of the
    /// serializer. This guards against an accidental field rename silently
    /// breaking on-disk config compatibility — a pure round-trip test would
    /// not catch that (a rename breaks and un-breaks symmetrically), only a
    /// fixed external fixture like this one would.
    #[test]
    fn profile_deserializes_from_fixed_json_fixture() {
        let json = r#"
        {
            "name": "quick-macro",
            "trigger": { "modifiers": ["Ctrl", "Alt"], "key": "F5" },
            "action": {
                "Macro": {
                    "steps": [
                        {
                            "KeyPress": {
                                "combo": { "modifiers": ["Shift"], "key": "Tab" },
                                "delay_ms": 50
                            }
                        },
                        {
                            "MouseClick": {
                                "delay_ms": 30,
                                "button": "Left",
                                "position": { "Fixed": { "x": 100, "y": 200 } }
                            }
                        }
                    ],
                    "loop": true
                }
            },
            "scope": {
                "Window": {
                    "process_name": "firefox",
                    "window_title_hint": "Mozilla Firefox",
                    "backend_hint_id": "uuid-abc-123"
                }
            },
            "focus_steal": true,
            "debounce_ms": 500
        }
        "#;

        let profile: Profile = serde_json::from_str(json).unwrap();

        assert_eq!(profile.name, "quick-macro");
        assert_eq!(
            profile.trigger,
            orchestrator_hotkey::KeyCombo {
                modifiers: vec![
                    orchestrator_hotkey::Modifier::Ctrl,
                    orchestrator_hotkey::Modifier::Alt
                ],
                key: "F5".to_string(),
            }
        );
        assert!(profile.focus_steal);
        assert_eq!(profile.debounce_ms, 500);
        assert_eq!(
            profile.scope,
            Scope::Window {
                process_name: "firefox".to_string(),
                window_title_hint: "Mozilla Firefox".to_string(),
                backend_hint_id: Some("uuid-abc-123".to_string()),
            }
        );

        match profile.action {
            Action::Macro { steps, loop_ } => {
                assert!(loop_);
                assert_eq!(steps.len(), 2);
                match &steps[0] {
                    crate::action::MacroStep::KeyPress { combo, delay_ms } => {
                        assert_eq!(*delay_ms, 50);
                        assert_eq!(combo.key, "Tab");
                        assert_eq!(combo.modifiers, vec![orchestrator_hotkey::Modifier::Shift]);
                    }
                    other => panic!("expected KeyPress, got {other:?}"),
                }
                match &steps[1] {
                    crate::action::MacroStep::MouseClick {
                        delay_ms,
                        button,
                        position,
                    } => {
                        assert_eq!(*delay_ms, 30);
                        assert_eq!(*button, orchestrator_input::MouseButton::Left);
                        assert_eq!(
                            *position,
                            crate::action::ClickPosition::Fixed { x: 100, y: 200 }
                        );
                    }
                    other => panic!("expected MouseClick, got {other:?}"),
                }
            }
            other => panic!("expected Action::Macro, got {other:?}"),
        }
    }
}
