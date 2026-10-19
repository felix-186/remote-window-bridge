#[cfg(not(windows))]
use crate::error::DesktopError;
use crate::{control::Cancellation, error::Result, geometry::Rect, image::RgbImage, types::*};

#[cfg(any(windows, feature = "windows-api-check"))]
pub mod windows;

/// The OS adapter owns permission prompts and native handles, not the Agent or core.
pub trait WindowBackend {
    fn capabilities(&self) -> Capabilities;
    fn permissions(&self) -> PermissionStatus {
        PermissionStatus {
            screen_capture: PermissionState::Unknown,
            input_control: PermissionState::Unknown,
            global_hotkeys: PermissionState::Unknown,
            notes: vec![],
        }
    }
    /// For a future LOCAL UI to invoke OS consent flows. Not an automatic privilege bypass.
    fn request_permissions(&self) -> Result<PermissionStatus> {
        Ok(self.permissions())
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>>;
    fn inspect(&self, id: &WindowId, viewport: Option<Rect>) -> Result<WindowSnapshot>;
    fn focus(&self, identity: &WindowIdentity, cancel: &Cancellation) -> Result<()>;
}
pub trait CaptureBackend {
    fn capture(&self, snapshot: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage>;
}
pub trait InputBackend {
    /// Must repeat native foreground / hit testing immediately before injection.
    fn inject(
        &self,
        snapshot: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
    ) -> Result<InjectionReport>;
}
pub trait DesktopPlatform: WindowBackend + CaptureBackend + InputBackend + Send {}
impl<T: WindowBackend + CaptureBackend + InputBackend + Send> DesktopPlatform for T {}

/// OCR is optional and can live in an isolated worker (Python, ONNX, or a native OS API).
pub trait OcrProvider: Send {
    fn recognize(&self, image: &RgbImage, cancel: &Cancellation) -> Result<Vec<TextRegion>>;
}

pub fn native(
    control: std::sync::Arc<crate::control::Control>,
) -> Result<Box<dyn DesktopPlatform>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsPlatform::new(control)?))
    }
    #[cfg(not(windows))]
    {
        let _ = control;
        Ok(Box::new(Unsupported))
    }
}

#[cfg(not(windows))]
struct Unsupported;
#[cfg(not(windows))]
fn unsupported<T>() -> Result<T> {
    Err(DesktopError::new(
        "PLATFORM_UNSUPPORTED",
        "This release implements Windows only; macOS/Linux adapters are not installed",
    ))
}
#[cfg(not(windows))]
impl WindowBackend for Unsupported {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: std::env::consts::OS.into(),
            window_capture: false,
            capture_backend: "unavailable".into(),
            requires_visible: true,
            input: false,
            ocr: false,
            global_pause_hotkey: false,
            notes: vec!["Windows backend only in v0.1".into()],
        }
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        unsupported()
    }
    fn inspect(&self, _: &WindowId, _: Option<Rect>) -> Result<WindowSnapshot> {
        unsupported()
    }
    fn focus(&self, _: &WindowIdentity, _: &Cancellation) -> Result<()> {
        unsupported()
    }
}
#[cfg(not(windows))]
impl CaptureBackend for Unsupported {
    fn capture(&self, _: &WindowSnapshot, _: &Cancellation) -> Result<RgbImage> {
        unsupported()
    }
}
#[cfg(not(windows))]
impl InputBackend for Unsupported {
    fn inject(
        &self,
        _: &WindowSnapshot,
        _: &ResolvedAction,
        _: &Cancellation,
    ) -> Result<InjectionReport> {
        unsupported()
    }
}
