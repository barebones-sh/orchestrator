use serde::{Deserialize, Serialize};

/// A portable keyboard shortcut combination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyCombo {
    pub modifiers: Vec<Modifier>,
    /// Portable key name, e.g. "F9", "Return", "Super_L"
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Modifier {
    Ctrl,
    Shift,
    Alt,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HotkeyId(pub u64);

#[derive(Debug, Clone)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}
