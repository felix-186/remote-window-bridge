use crate::geometry::{ImageTransform, Point, Rect, ScreenPoint};
use serde::{Deserialize, Serialize};

/// Opaque platform token. Public APIs must not assume this contains a Win32 HWND.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowIdentity {
    pub id: WindowId,
    pub process_id: u32,
    pub process_started: u64,
    pub class_name: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct WindowInfo {
    pub identity: WindowIdentity,
    pub title: String,
    pub executable: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowSnapshot {
    pub identity: WindowIdentity,
    pub activation: Option<WindowIdentity>,
    pub input_surface: Option<WindowIdentity>,
    pub client_screen: Rect,
    pub content_screen: Rect,
    pub dpi: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub platform: String,
    pub window_capture: bool,
    pub capture_backend: String,
    pub requires_visible: bool,
    pub input: bool,
    pub ocr: bool,
    pub global_pause_hotkey: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Required,
    Denied,
    NotRequired,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionStatus {
    pub screen_capture: PermissionState,
    pub input_control: PermissionState,
    pub global_hotkeys: PermissionState,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameMetadata {
    pub session_id: String,
    pub frame_id: String,
    pub control_generation: u64,
    pub geometry_version: u64,
    pub captured_at_ms: u64,
    pub transform: ImageTransform,
    pub capture_backend: String,
    pub image_path: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextInputMode {
    Auto,
    #[default]
    Unicode,
    ScanCode,
}
impl TextInputMode {
    fn is_unicode(&self) -> bool {
        *self == Self::Unicode
    }
    pub fn resolved(self, text: &str) -> Self {
        match self {
            Self::Auto if text.bytes().all(|byte| (b' '..=b'~').contains(&byte)) => Self::ScanCode,
            Self::Auto => Self::Unicode,
            mode => mode,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Click {
        point: Point,
        button: MouseButton,
        #[serde(default = "one")]
        count: u8,
    },
    Move {
        point: Point,
    },
    Scroll {
        point: Point,
        ticks: i32,
        #[serde(default)]
        horizontal: bool,
    },
    TypeText {
        text: String,
        #[serde(default)]
        sensitive: bool,
        // Preserve fingerprints of existing Unicode actions in the audit log.
        #[serde(default, skip_serializing_if = "TextInputMode::is_unicode")]
        input_mode: TextInputMode,
    },
    Fill {
        point: Point,
        text: String,
        #[serde(default)]
        sensitive: bool,
        input_mode: TextInputMode,
        settle_ms: u64,
    },
    KeyAt {
        point: Point,
        keys: Vec<String>,
        settle_ms: u64,
    },
    PressKey {
        keys: Vec<String>,
    },
}
fn one() -> u8 {
    1
}

#[derive(Debug, Clone)]
pub enum ResolvedAction {
    Click {
        point: ScreenPoint,
        button: MouseButton,
        count: u8,
    },
    Move {
        point: ScreenPoint,
    },
    Scroll {
        point: ScreenPoint,
        ticks: i32,
        horizontal: bool,
    },
    TypeText {
        text: String,
        input_mode: TextInputMode,
    },
    Fill {
        point: ScreenPoint,
        text: String,
        input_mode: TextInputMode,
    },
    KeyAt {
        point: ScreenPoint,
        keys: Vec<String>,
    },
    PressKey {
        keys: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub session_id: String,
    pub frame_id: String,
    pub action_id: String,
    pub action: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub action_id: String,
    pub execution_status: String,
    pub verification_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_input_mode: Option<TextInputMode>,
    pub sent_events: u32,
    pub next_frame_id: Option<String>,
    pub error: Option<crate::error::DesktopError>,
}

#[derive(Debug, Clone)]
pub struct InjectionReport {
    pub sent_events: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRegion {
    pub text: String,
    pub confidence: f32,
    pub bounds: Rect,
}
