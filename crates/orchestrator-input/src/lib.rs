pub mod error;
pub mod trait_def;
pub mod vocab;

pub use error::*;
pub use trait_def::InputInjector;
pub use vocab::*;

#[cfg(feature = "macos")]
pub mod macos;

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_hotkey::{KeyCombo, Modifier};

    #[test]
    fn input_event_key_press_serde_round_trip() {
        let event = InputEvent::KeyPress(KeyCombo {
            modifiers: vec![Modifier::Ctrl],
            key: "F9".to_string(),
        });
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn input_event_mouse_move_serde_round_trip() {
        let event = InputEvent::MouseMoveAbsolute { x: 10, y: 20 };
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn input_event_scroll_serde_round_trip() {
        let event = InputEvent::Scroll { dx: -1, dy: 3 };
        let json = serde_json::to_string(&event).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn mouse_button_serde_round_trip() {
        let button = MouseButton::Middle;
        let json = serde_json::to_string(&button).unwrap();
        let back: MouseButton = serde_json::from_str(&json).unwrap();
        assert_eq!(button, back);
    }

    #[cfg(feature = "macos")]
    mod macos_stub {
        use super::*;
        use crate::macos::MacosInputInjector;

        #[tokio::test]
        async fn connect_reports_backend_unavailable() {
            let mut backend = MacosInputInjector;
            let result = backend.connect().await;
            assert!(matches!(result, Err(InjectError::BackendUnavailable(_))));
        }

        #[tokio::test]
        async fn inject_reports_backend_unavailable() {
            let backend = MacosInputInjector;
            let event = InputEvent::Scroll { dx: 0, dy: 1 };
            let result = backend.inject(&event).await;
            assert!(matches!(result, Err(InjectError::BackendUnavailable(_))));
        }

        #[test]
        fn does_not_require_focus_steal() {
            let backend = MacosInputInjector;
            assert!(!backend.requires_focus_steal_for_window_targeting());
        }

        #[test]
        fn backend_name_is_macos_stub() {
            let backend = MacosInputInjector;
            assert_eq!(backend.backend_name(), "macos-stub");
        }
    }
}
