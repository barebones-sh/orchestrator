//! Colon-separated step/combo notation parser for `profile add`/`edit`'s
//! `--input`/`--step` flags. See design spec §3.2 for the notation and the
//! rationale behind repeat-mode's key/scroll-only restriction.
//! docs/superpowers/specs/2026-09-12-cli-ux-design.md

use orchestrator_core::action::{ClickPosition, MacroStep};
use orchestrator_hotkey::{KeyCombo, Modifier};
use orchestrator_input::{InputEvent, MouseButton};

#[derive(Debug, PartialEq, Eq)]
pub enum NotationError {
    UnknownStepKind(String),
    WrongFieldCount {
        kind: String,
        expected: usize,
        got: usize,
    },
    UnrecognizedModifierOrEmptyKey(String),
    InvalidCoordinates(String),
    InvalidButton(String),
    InvalidDelay(String),
    NotAllowedInRepeatMode(String),
}

impl std::fmt::Display for NotationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStepKind(k) => write!(f, "unknown step kind {k:?} (expected key/click/drag/scroll)"),
            Self::WrongFieldCount { kind, expected, got } => {
                write!(f, "{kind} notation expects {expected} colon-separated fields, got {got}")
            }
            Self::UnrecognizedModifierOrEmptyKey(s) => write!(f, "could not parse key combo {s:?}"),
            Self::InvalidCoordinates(s) => write!(f, "invalid position {s:?} (expected \"x,y\" or \"cursor\")"),
            Self::InvalidButton(s) => write!(f, "invalid mouse button {s:?} (expected left/right/middle)"),
            Self::InvalidDelay(s) => write!(f, "invalid delay_ms {s:?} (expected a non-negative integer)"),
            Self::NotAllowedInRepeatMode(kind) => write!(
                f,
                "{kind} is not allowed with --action repeat (a repeated bare press has no release, \
                 which doesn't correspond to a real click/drag) -- use --action macro --loop instead"
            ),
        }
    }
}

impl std::error::Error for NotationError {}

pub fn parse_combo(s: &str) -> Result<KeyCombo, NotationError> {
    if s.is_empty() {
        return Err(NotationError::UnrecognizedModifierOrEmptyKey(s.to_string()));
    }
    let parts: Vec<&str> = s.split('+').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(NotationError::UnrecognizedModifierOrEmptyKey(s.to_string()));
    }
    let (key, modifier_parts) = parts
        .split_last()
        .expect("split('+') on a non-empty string yields >=1 part");
    let mut modifiers = Vec::new();
    for part in modifier_parts {
        let modifier = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Modifier::Ctrl,
            "shift" => Modifier::Shift,
            "alt" => Modifier::Alt,
            "meta" | "super" | "win" => Modifier::Meta,
            _ => return Err(NotationError::UnrecognizedModifierOrEmptyKey(s.to_string())),
        };
        modifiers.push(modifier);
    }
    Ok(KeyCombo {
        modifiers,
        key: key.to_string(),
    })
}

fn parse_delay_ms(s: &str) -> Result<u32, NotationError> {
    s.parse::<u32>()
        .map_err(|_| NotationError::InvalidDelay(s.to_string()))
}

fn parse_position(s: &str) -> Result<ClickPosition, NotationError> {
    if s == "cursor" {
        return Ok(ClickPosition::AtCursor);
    }
    let (x_str, y_str) = s
        .split_once(',')
        .ok_or_else(|| NotationError::InvalidCoordinates(s.to_string()))?;
    let x: i32 = x_str
        .parse()
        .map_err(|_| NotationError::InvalidCoordinates(s.to_string()))?;
    let y: i32 = y_str
        .parse()
        .map_err(|_| NotationError::InvalidCoordinates(s.to_string()))?;
    Ok(ClickPosition::Fixed { x, y })
}

fn parse_dx_dy(s: &str) -> Result<(i32, i32), NotationError> {
    let (dx_str, dy_str) = s
        .split_once(',')
        .ok_or_else(|| NotationError::InvalidCoordinates(s.to_string()))?;
    let dx: i32 = dx_str
        .parse()
        .map_err(|_| NotationError::InvalidCoordinates(s.to_string()))?;
    let dy: i32 = dy_str
        .parse()
        .map_err(|_| NotationError::InvalidCoordinates(s.to_string()))?;
    Ok((dx, dy))
}

fn parse_button(s: &str) -> Result<MouseButton, NotationError> {
    match s.to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        _ => Err(NotationError::InvalidButton(s.to_string())),
    }
}

pub fn parse_macro_step(s: &str) -> Result<MacroStep, NotationError> {
    let fields: Vec<&str> = s.split(':').collect();
    let kind = *fields
        .first()
        .ok_or_else(|| NotationError::UnknownStepKind(s.to_string()))?;
    match kind {
        "key" => {
            if fields.len() != 3 {
                return Err(NotationError::WrongFieldCount {
                    kind: "key".to_string(),
                    expected: 3,
                    got: fields.len(),
                });
            }
            Ok(MacroStep::KeyPress {
                combo: parse_combo(fields[1])?,
                delay_ms: parse_delay_ms(fields[2])?,
            })
        }
        "click" => {
            if fields.len() != 4 {
                return Err(NotationError::WrongFieldCount {
                    kind: "click".to_string(),
                    expected: 4,
                    got: fields.len(),
                });
            }
            Ok(MacroStep::MouseClick {
                delay_ms: parse_delay_ms(fields[3])?,
                button: parse_button(fields[1])?,
                position: parse_position(fields[2])?,
            })
        }
        "drag" => {
            if fields.len() != 4 {
                return Err(NotationError::WrongFieldCount {
                    kind: "drag".to_string(),
                    expected: 4,
                    got: fields.len(),
                });
            }
            Ok(MacroStep::Drag {
                delay_ms: parse_delay_ms(fields[3])?,
                from: parse_position(fields[1])?,
                to: parse_position(fields[2])?,
            })
        }
        "scroll" => {
            if fields.len() != 3 {
                return Err(NotationError::WrongFieldCount {
                    kind: "scroll".to_string(),
                    expected: 3,
                    got: fields.len(),
                });
            }
            let (dx, dy) = parse_dx_dy(fields[1])?;
            Ok(MacroStep::Scroll {
                delay_ms: parse_delay_ms(fields[2])?,
                dx,
                dy,
            })
        }
        other => Err(NotationError::UnknownStepKind(other.to_string())),
    }
}

pub fn parse_repeat_input(s: &str) -> Result<InputEvent, NotationError> {
    let fields: Vec<&str> = s.split(':').collect();
    let kind = *fields
        .first()
        .ok_or_else(|| NotationError::UnknownStepKind(s.to_string()))?;
    match kind {
        "key" => {
            if fields.len() != 2 {
                return Err(NotationError::WrongFieldCount {
                    kind: "key".to_string(),
                    expected: 2,
                    got: fields.len(),
                });
            }
            Ok(InputEvent::KeyPress(parse_combo(fields[1])?))
        }
        "scroll" => {
            if fields.len() != 2 {
                return Err(NotationError::WrongFieldCount {
                    kind: "scroll".to_string(),
                    expected: 2,
                    got: fields.len(),
                });
            }
            let (dx, dy) = parse_dx_dy(fields[1])?;
            Ok(InputEvent::Scroll { dx, dy })
        }
        "click" => Err(NotationError::NotAllowedInRepeatMode("click".to_string())),
        "drag" => Err(NotationError::NotAllowedInRepeatMode("drag".to_string())),
        other => Err(NotationError::UnknownStepKind(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_combo ---------------------------------------------------

    #[test]
    fn parse_combo_plain_key() {
        let combo = parse_combo("A").unwrap();
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![],
                key: "A".to_string()
            }
        );
    }

    #[test]
    fn parse_combo_single_modifier() {
        let combo = parse_combo("Ctrl+C").unwrap();
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "C".to_string()
            }
        );
    }

    #[test]
    fn parse_combo_multiple_modifiers_case_insensitive() {
        let combo = parse_combo("ctrl+ALT+F5").unwrap();
        assert_eq!(
            combo,
            KeyCombo {
                modifiers: vec![Modifier::Ctrl, Modifier::Alt],
                key: "F5".to_string()
            }
        );
    }

    #[test]
    fn parse_combo_meta_aliases() {
        for alias in ["Meta", "Super", "Win"] {
            let combo = parse_combo(&format!("{alias}+F9")).unwrap();
            assert_eq!(
                combo.modifiers,
                vec![Modifier::Meta],
                "alias {alias} should map to Modifier::Meta"
            );
        }
    }

    #[test]
    fn parse_combo_empty_string_is_an_error() {
        assert!(parse_combo("").is_err());
    }

    #[test]
    fn parse_combo_trailing_plus_is_an_error() {
        assert!(parse_combo("Ctrl+").is_err());
    }

    // -- parse_macro_step: key -----------------------------------------

    #[test]
    fn macro_step_key() {
        let step = parse_macro_step("key:Ctrl+Alt+F5:50").unwrap();
        assert_eq!(
            step,
            MacroStep::KeyPress {
                combo: KeyCombo {
                    modifiers: vec![Modifier::Ctrl, Modifier::Alt],
                    key: "F5".to_string()
                },
                delay_ms: 50,
            }
        );
    }

    #[test]
    fn macro_step_key_wrong_field_count() {
        assert!(matches!(
            parse_macro_step("key:Ctrl+C"),
            Err(NotationError::WrongFieldCount { .. })
        ));
    }

    // -- parse_macro_step: click ----------------------------------------

    #[test]
    fn macro_step_click_fixed_position() {
        let step = parse_macro_step("click:left:100,200:30").unwrap();
        assert_eq!(
            step,
            MacroStep::MouseClick {
                delay_ms: 30,
                button: MouseButton::Left,
                position: ClickPosition::Fixed { x: 100, y: 200 },
            }
        );
    }

    #[test]
    fn macro_step_click_at_cursor() {
        let step = parse_macro_step("click:right:cursor:10").unwrap();
        assert_eq!(
            step,
            MacroStep::MouseClick {
                delay_ms: 10,
                button: MouseButton::Right,
                position: ClickPosition::AtCursor
            }
        );
    }

    #[test]
    fn macro_step_click_invalid_button() {
        assert!(matches!(
            parse_macro_step("click:nope:cursor:10"),
            Err(NotationError::InvalidButton(_))
        ));
    }

    #[test]
    fn macro_step_click_invalid_coordinates() {
        assert!(matches!(
            parse_macro_step("click:left:not-a-position:10"),
            Err(NotationError::InvalidCoordinates(_))
        ));
    }

    // -- parse_macro_step: drag ------------------------------------------

    #[test]
    fn macro_step_drag_mixed_positions() {
        let step = parse_macro_step("drag:cursor:300,400:50").unwrap();
        assert_eq!(
            step,
            MacroStep::Drag {
                delay_ms: 50,
                from: ClickPosition::AtCursor,
                to: ClickPosition::Fixed { x: 300, y: 400 }
            }
        );
    }

    // -- parse_macro_step: scroll -----------------------------------------

    #[test]
    fn macro_step_scroll() {
        let step = parse_macro_step("scroll:0,-3:20").unwrap();
        assert_eq!(
            step,
            MacroStep::Scroll {
                delay_ms: 20,
                dx: 0,
                dy: -3
            }
        );
    }

    // -- parse_macro_step: unknown kind -----------------------------------

    #[test]
    fn macro_step_unknown_kind() {
        assert!(matches!(
            parse_macro_step("teleport:1,2:5"),
            Err(NotationError::UnknownStepKind(_))
        ));
    }

    // -- parse_repeat_input: key/scroll allowed, click/drag rejected -------

    #[test]
    fn repeat_input_key() {
        let event = parse_repeat_input("key:W").unwrap();
        assert_eq!(
            event,
            InputEvent::KeyPress(KeyCombo {
                modifiers: vec![],
                key: "W".to_string()
            })
        );
    }

    #[test]
    fn repeat_input_scroll() {
        let event = parse_repeat_input("scroll:0,1").unwrap();
        assert_eq!(event, InputEvent::Scroll { dx: 0, dy: 1 });
    }

    #[test]
    fn repeat_input_click_is_rejected() {
        let err = parse_repeat_input("click:left:cursor").unwrap_err();
        assert!(matches!(err, NotationError::NotAllowedInRepeatMode(_)));
    }

    #[test]
    fn repeat_input_drag_is_rejected() {
        let err = parse_repeat_input("drag:cursor:100,100").unwrap_err();
        assert!(matches!(err, NotationError::NotAllowedInRepeatMode(_)));
    }
}
