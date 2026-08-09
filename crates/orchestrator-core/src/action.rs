//! `Action`, `Jitter`, `MacroStep`, `ClickPosition` data model (design spec §2 "Action" /
//! "Jitter" / "MacroStep").

use serde::{Deserialize, Serialize};

/// An action defines what to do when triggered: repeat an input sequence
/// at intervals, or execute a macro.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// Repeat an input sequence with optional jitter.
    /// Input is specified as an InputEvent (vocabulary type from orchestrator_input
    /// crate), reused directly to maintain consistency with the trait layer.
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

/// For repeat actions, optional jitter is applied to interval timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Jitter {
    /// No jitter applied.
    None,
    /// Uniform random jitter within a range (simpler, more predictable to
    /// configure than Gaussian; no extra rand_distr dependency).
    Uniform { range_ms: u64 },
}

/// A single step in a macro sequence, carrying a delay. Four variants cover
/// the primary automation patterns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClickPosition {
    /// Absolute screen coordinates.
    Fixed { x: i32, y: i32 },
    /// Inject at the current cursor position (resolved at injection time).
    AtCursor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_hotkey::{KeyCombo, Modifier};
    use orchestrator_input::{InputEvent, MouseButton};

    #[test]
    fn action_repeat_serde_round_trip() {
        let action = Action::Repeat {
            input: InputEvent::Scroll { dx: 1, dy: -1 },
            interval_ms: 250,
            jitter: Jitter::Uniform { range_ms: 50 },
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(action, back);
    }

    #[test]
    fn action_macro_serde_round_trip() {
        let action = Action::Macro {
            steps: vec![
                MacroStep::KeyPress {
                    combo: KeyCombo {
                        modifiers: vec![Modifier::Ctrl],
                        key: "C".to_string(),
                    },
                    delay_ms: 10,
                },
                MacroStep::Scroll {
                    delay_ms: 5,
                    dx: 0,
                    dy: -3,
                },
            ],
            loop_: true,
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: Action = serde_json::from_str(&json).unwrap();
        assert_eq!(action, back);
        // Verify the `loop_` field is actually serialized under the renamed
        // `loop` key, per the design spec's `#[serde(rename = "loop")]`.
        assert!(json.contains("\"loop\""));
    }

    #[test]
    fn jitter_none_serde_round_trip() {
        let jitter = Jitter::None;
        let json = serde_json::to_string(&jitter).unwrap();
        let back: Jitter = serde_json::from_str(&json).unwrap();
        assert_eq!(jitter, back);
    }

    #[test]
    fn jitter_uniform_serde_round_trip() {
        let jitter = Jitter::Uniform { range_ms: 30 };
        let json = serde_json::to_string(&jitter).unwrap();
        let back: Jitter = serde_json::from_str(&json).unwrap();
        assert_eq!(jitter, back);
    }

    #[test]
    fn macro_step_key_press_serde_round_trip() {
        let step = MacroStep::KeyPress {
            combo: KeyCombo {
                modifiers: vec![Modifier::Alt],
                key: "Tab".to_string(),
            },
            delay_ms: 20,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn macro_step_mouse_click_serde_round_trip() {
        let step = MacroStep::MouseClick {
            delay_ms: 15,
            button: MouseButton::Left,
            position: ClickPosition::Fixed { x: 100, y: 200 },
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn macro_step_drag_serde_round_trip() {
        let step = MacroStep::Drag {
            delay_ms: 25,
            from: ClickPosition::AtCursor,
            to: ClickPosition::Fixed { x: 50, y: 60 },
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn macro_step_scroll_serde_round_trip() {
        let step = MacroStep::Scroll {
            delay_ms: 5,
            dx: -2,
            dy: 4,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn click_position_fixed_serde_round_trip() {
        let pos = ClickPosition::Fixed { x: 1, y: 2 };
        let json = serde_json::to_string(&pos).unwrap();
        let back: ClickPosition = serde_json::from_str(&json).unwrap();
        assert_eq!(pos, back);
    }

    #[test]
    fn click_position_at_cursor_serde_round_trip() {
        let pos = ClickPosition::AtCursor;
        let json = serde_json::to_string(&pos).unwrap();
        let back: ClickPosition = serde_json::from_str(&json).unwrap();
        assert_eq!(pos, back);
    }
}
