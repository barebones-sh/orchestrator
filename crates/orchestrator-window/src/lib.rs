pub mod error;
pub mod trait_def;
pub mod vocab;

pub use error::*;
pub use trait_def::WindowLocator;
pub use vocab::*;

#[cfg(feature = "macos")]
pub mod macos;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_handle_serde_round_trip() {
        let handle = WindowHandle("kwin-uuid-1234".to_string());
        let json = serde_json::to_string(&handle).unwrap();
        let back: WindowHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(handle, back);
    }

    #[test]
    fn window_info_serde_round_trip() {
        let info = WindowInfo {
            handle: WindowHandle("kwin-uuid-1234".to_string()),
            pid: Some(4242),
            process_name: Some("firefox".to_string()),
            title: "Mozilla Firefox".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: WindowInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, back);
    }

    #[cfg(feature = "macos")]
    mod macos_stub {
        use super::*;
        use crate::macos::MacosWindowLocator;

        #[tokio::test]
        async fn list_windows_reports_backend_unavailable() {
            let backend = MacosWindowLocator;
            let result = backend.list_windows().await;
            assert!(matches!(result, Err(WindowError::BackendUnavailable(_))));
        }

        #[tokio::test]
        async fn focused_window_reports_backend_unavailable() {
            let backend = MacosWindowLocator;
            let result = backend.focused_window().await;
            assert!(matches!(result, Err(WindowError::BackendUnavailable(_))));
        }

        #[tokio::test]
        async fn activate_window_reports_backend_unavailable() {
            let backend = MacosWindowLocator;
            let handle = WindowHandle("some-handle".to_string());
            let result = backend.activate_window(&handle).await;
            assert!(matches!(result, Err(WindowError::BackendUnavailable(_))));
        }
    }
}
