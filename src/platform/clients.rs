//! Pure client policy. No native handles, activation, or speculative input retries.
use crate::types::{TextInputMode, WindowInfo};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Generic,
    AweSun,
    RustDesk,
}
#[derive(Serialize)]
pub struct ClientProfile {
    pub kind: ClientKind,
    pub paired_surfaces: bool,
    pub ascii_input: TextInputMode,
    pub ordinary_keys: &'static str,
    pub settle_hint_ms: u64,
}
pub fn profile(window: &WindowInfo) -> ClientProfile {
    let executable = window.executable.rsplit(['\\', '/']).next().unwrap_or("");
    let class = window.identity.class_name.as_str();
    let kind = if executable.eq_ignore_ascii_case("AweSun.exe")
        && matches!(class, "FlutterMultiWindow" | "ORAY_FLUTTER_VIEW_CONTAINER")
    {
        ClientKind::AweSun
    } else if executable.eq_ignore_ascii_case("rustdesk.exe") && class == "RustdeskMultiWindow" {
        ClientKind::RustDesk
    } else {
        ClientKind::Generic
    };
    ClientProfile {
        kind,
        paired_surfaces: kind == ClientKind::AweSun,
        ascii_input: TextInputMode::ScanCode,
        ordinary_keys: "scan_code_with_special_key_fallback",
        settle_hint_ms: if kind == ClientKind::Generic {
            250
        } else {
            1000
        },
    }
}
pub(super) fn awesun_expected_class(window: &WindowInfo) -> Option<&'static str> {
    if profile(window).kind != ClientKind::AweSun || window.title.is_empty() {
        return None;
    }
    match window.identity.class_name.as_str() {
        "FlutterMultiWindow" => Some("ORAY_FLUTTER_VIEW_CONTAINER"),
        "ORAY_FLUTTER_VIEW_CONTAINER" => Some("FlutterMultiWindow"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WindowId, WindowIdentity};
    #[test]
    fn profiles_require_executable_and_remote_window_class() {
        let mut window = WindowInfo {
            title: "remote".into(),
            executable: "C:\\RustDesk\\rustdesk.exe".into(),
            identity: WindowIdentity {
                id: WindowId("win:1".into()),
                process_id: 1,
                process_started: 1,
                class_name: "RustdeskMultiWindow".into(),
            },
        };
        assert_eq!(profile(&window).kind, ClientKind::RustDesk);
        window.identity.class_name = "FLUTTER_RUNNER_WIN32_WINDOW".into();
        assert_eq!(profile(&window).kind, ClientKind::Generic);
        window.identity.class_name = "FlutterMultiWindow".into();
        assert!(awesun_expected_class(&window).is_none());
        window.executable = "C:\\AweSun.exe".into();
        assert_eq!(
            awesun_expected_class(&window),
            Some("ORAY_FLUTTER_VIEW_CONTAINER")
        );
        window.title.clear();
        assert!(awesun_expected_class(&window).is_none());
    }
}
