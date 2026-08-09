use serde::{Deserialize, Serialize};

/// Opaque backend identifier, e.g. KWin UUID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowHandle(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub handle: WindowHandle,
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub title: String,
}
