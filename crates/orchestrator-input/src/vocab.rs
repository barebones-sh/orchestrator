use orchestrator_hotkey::KeyCombo;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    KeyPress(KeyCombo),
    KeyRelease(KeyCombo),
    MouseButtonPress(MouseButton),
    MouseButtonRelease(MouseButton),
    MouseMoveAbsolute { x: i32, y: i32 },
    MouseMoveRelative { dx: i32, dy: i32 },
    Scroll { dx: i32, dy: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
