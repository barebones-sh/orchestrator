pub mod error;
pub mod trait_def;
pub mod vocab;

pub use error::*;
pub use trait_def::HotkeyBackend;
pub use vocab::*;

#[cfg(feature = "macos")]
pub mod macos;

#[cfg(feature = "kde")]
pub mod kde_portal_shortcuts;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_combo_serde_round_trip() {
        let combo = KeyCombo {
            modifiers: vec![Modifier::Ctrl, Modifier::Shift],
            key: "F9".to_string(),
        };
        let json = serde_json::to_string(&combo).unwrap();
        let back: KeyCombo = serde_json::from_str(&json).unwrap();
        assert_eq!(combo, back);
    }

    #[test]
    fn modifier_serde_round_trip() {
        let m = Modifier::Alt;
        let json = serde_json::to_string(&m).unwrap();
        let back: Modifier = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn hotkey_id_serde_round_trip() {
        let id = HotkeyId(42);
        let json = serde_json::to_string(&id).unwrap();
        let back: HotkeyId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[cfg(feature = "macos")]
    mod macos_stub {
        use super::*;
        use crate::macos::MacosHotkeyBackend;

        #[tokio::test]
        async fn register_reports_backend_unavailable() {
            let backend = MacosHotkeyBackend;
            let combo = KeyCombo {
                modifiers: vec![],
                key: "F9".to_string(),
            };
            let result = backend.register("test-action", Some(&combo)).await;
            assert!(matches!(result, Err(HotkeyError::BackendUnavailable(_))));
        }

        #[tokio::test]
        async fn unregister_reports_backend_unavailable() {
            let backend = MacosHotkeyBackend;
            let result = backend.unregister(HotkeyId(1)).await;
            assert!(matches!(result, Err(HotkeyError::BackendUnavailable(_))));
        }

        #[test]
        fn subscribe_returns_closed_receiver() {
            let backend = MacosHotkeyBackend;
            let rx = backend.subscribe();
            assert!(rx.recv().is_err());
        }

        #[test]
        fn backend_name_is_macos_stub() {
            let backend = MacosHotkeyBackend;
            assert_eq!(backend.backend_name(), "macos-stub");
        }
    }
}
