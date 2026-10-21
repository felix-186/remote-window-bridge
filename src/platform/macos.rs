//! Persistent native helper: ScreenCaptureKit, Accessibility and Quartz events.
//! JSON headers and binary RGB payloads avoid per-frame subprocesses/base64 copies.
use super::{CaptureBackend, InputBackend, WindowBackend};
use crate::{
    control::{Cancellation, Control},
    error::{DesktopError, Result},
    geometry::Rect,
    image::RgbImage,
    types::*,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

struct Reply {
    value: Value,
    pixels: Vec<u8>,
}
struct Bridge {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Result<Reply>>,
    failed: bool,
}
fn unavailable(error: impl std::fmt::Display) -> DesktopError {
    DesktopError::new("MACOS_BRIDGE_UNAVAILABLE", error.to_string())
}
#[cfg(target_os = "macos")]
fn helper_path() -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/macos-bridge"));
    let hash = format!("{:x}", Sha256::digest(HELPER));
    let home = std::env::var_os("HOME").ok_or_else(|| unavailable("HOME is unavailable"))?;
    let directory = PathBuf::from(home)
        .join("Library/Caches/remote-window-bridge")
        .join(hash);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(unavailable)?;
    let path = directory.join("macos-bridge");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&path)
    {
        Ok(mut file) => file.write_all(HELPER).map_err(unavailable)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path).map_err(unavailable)? != HELPER {
                return Err(unavailable(
                    "Native helper cache differs from embedded executable",
                ));
            }
        }
        Err(e) => return Err(unavailable(e)),
    }
    Ok(path)
}
#[cfg(not(target_os = "macos"))]
fn helper_path() -> Result<PathBuf> {
    Err(unavailable(
        "macos-api-check only checks types; run on macOS 14+",
    ))
}
impl Bridge {
    fn start() -> Result<Self> {
        let mut child = Command::new(helper_path()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(unavailable)?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let reply = read_reply(&mut reader);
                let failed = reply.is_err();
                if tx.send(reply).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input,
            replies,
            failed: false,
        })
    }
    fn call(&mut self, request: Value) -> Result<Reply> {
        if self.failed {
            return Err(unavailable("Native helper stopped; restart the bridge"));
        }
        let result = (|| {
            serde_json::to_writer(&mut self.input, &request).map_err(unavailable)?;
            self.input
                .write_all(b"\n")
                .and_then(|_| self.input.flush())
                .map_err(unavailable)?;
            self.replies
                .recv_timeout(Duration::from_secs(15))
                .map_err(unavailable)?
        })();
        match result {
            Ok(reply) => {
                if let Some(error) = reply.value.get("error") {
                    return Err(serde_json::from_value(error.clone()).map_err(unavailable)?);
                }
                Ok(reply)
            }
            Err(error) => {
                // Never replay a request whose input may already have been posted.
                self.failed = true;
                let _ = self.child.kill();
                Err(error)
            }
        }
    }
}
fn read_reply(reader: &mut impl BufRead) -> Result<Reply> {
    let mut header = Vec::new();
    reader
        .take(64 * 1024)
        .read_until(b'\n', &mut header)
        .map_err(unavailable)?;
    if header.last() != Some(&b'\n') {
        return Err(unavailable("Missing/oversized native reply"));
    }
    let value: Value = serde_json::from_slice(&header).map_err(unavailable)?;
    let size = value
        .get("payload_len")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if size > 64 * 1024 * 1024 {
        return Err(unavailable("Native frame exceeds 64 MiB"));
    }
    let mut pixels = vec![0; size as usize];
    reader.read_exact(&mut pixels).map_err(unavailable)?;
    Ok(Reply { value, pixels })
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct MacPlatform {
    bridge: Mutex<Bridge>,
    _control: Arc<Control>,
}
impl MacPlatform {
    pub fn new(control: Arc<Control>) -> Result<Self> {
        let platform = Self {
            bridge: Mutex::new(Bridge::start()?),
            _control: control,
        };
        platform.call(json!({"op":"lease"}))?;
        Ok(platform)
    }
    fn call(&self, request: Value) -> Result<Reply> {
        self.bridge.lock().unwrap().call(request)
    }
    fn component(
        &self,
        snapshot: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
        sent: &mut u32,
    ) -> Result<()> {
        cancel.check_running()?;
        let reply = self.call(json!({"op":"inject", "snapshot":snapshot, "action":action}))
            .map_err(|error| {
                if error.code == "MACOS_BRIDGE_UNAVAILABLE" {
                    DesktopError::new("INPUT_OUTCOME_UNKNOWN", "Native reply lost; input may have been posted. Restart the bridge and inspect before continuing")
                        .with_details(json!({"transport_error":error}))
                } else { error }
            })?;
        *sent += reply.value["sent_events"].as_u64().unwrap_or(0) as u32;
        Ok(())
    }
}
impl WindowBackend for MacPlatform {
    fn capabilities(&self) -> Capabilities {
        Capabilities { platform: "macos".into(), window_capture: true,
            capture_backend: "screencapturekit_window".into(), requires_visible: true,
            input: true, ocr: false, global_pause_hotkey: false,
            notes: vec!["macOS 14+; Screen Recording and Accessibility permission required for the launching application/native helper".into(),
                "Window bounds include local title bars; calibrate a viewport before input".into(),
                "Foreground, unoccluded, unlocked desktop required; mixed-scale display spans are rejected".into(),
                "No global pause hotkey or physical-input takeover detection; use desktop_pause or stop the process locally".into()] }
    }
    fn permissions(&self) -> PermissionStatus {
        match self.call(json!({"op":"permissions"})) {
            Ok(reply) => PermissionStatus {
                screen_capture: if reply.value["screen_capture"] == true {
                    PermissionState::Granted
                } else {
                    PermissionState::Required
                },
                input_control: if reply.value["input_control"] == true {
                    PermissionState::Granted
                } else {
                    PermissionState::Required
                },
                global_hotkeys: PermissionState::Unavailable,
                notes: vec![],
            },
            Err(error) => PermissionStatus {
                screen_capture: PermissionState::Unknown,
                input_control: PermissionState::Unknown,
                global_hotkeys: PermissionState::Unavailable,
                notes: vec![error.to_string()],
            },
        }
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        serde_json::from_value(self.call(json!({"op":"windows"}))?.value["windows"].clone())
            .map_err(unavailable)
    }
    fn inspect(&self, id: &WindowId, viewport: Option<Rect>) -> Result<WindowSnapshot> {
        let mut snapshot: WindowSnapshot = serde_json::from_value(
            self.call(json!({"op":"inspect", "id":id}))?.value["snapshot"].clone(),
        )
        .map_err(unavailable)?;
        snapshot.client_screen.validate()?;
        snapshot.content_screen = match viewport {
            Some(v) => snapshot.client_screen.crop(v)?,
            None => snapshot.client_screen,
        };
        Ok(snapshot)
    }
    fn focus(&self, identity: &WindowIdentity, cancel: &Cancellation) -> Result<()> {
        cancel.check_running()?;
        self.call(json!({"op":"focus", "identity":identity}))?;
        for _ in 0..25 {
            cancel.check_running()?;
            if self
                .call(json!({"op":"foreground", "identity":identity}))?
                .value["foreground"]
                == true
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Err(DesktopError::new(
            "FOREGROUND_REQUIRED",
            "macOS refused window activation",
        ))
    }
    fn prepare_observation(
        &self,
        identity: &WindowIdentity,
        cancel: &Cancellation,
    ) -> Result<bool> {
        if self
            .call(json!({"op":"foreground", "identity":identity}))?
            .value["foreground"]
            == true
        {
            return Ok(false);
        }
        self.focus(identity, cancel)?;
        Ok(true)
    }
    fn diagnostics(&self, id: Option<&WindowId>) -> Result<Value> {
        Ok(self.call(json!({"op":"diagnostics", "id":id}))?.value)
    }
}
impl CaptureBackend for MacPlatform {
    fn capture(&self, snapshot: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage> {
        cancel.check()?;
        let reply = self.call(json!({"op":"capture", "snapshot":snapshot}))?;
        cancel.check()?;
        let image = RgbImage {
            width: snapshot.content_screen.width,
            height: snapshot.content_screen.height,
            pixels: reply.pixels,
        };
        image.validate()?;
        Ok(image)
    }
}
impl InputBackend for MacPlatform {
    fn inject(
        &self,
        snapshot: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
    ) -> Result<InjectionReport> {
        let mut sent = 0;
        let result = (|| {
            match action {
                ResolvedAction::Fill {
                    point,
                    text,
                    input_mode,
                } => {
                    validate_text(text, *input_mode)?;
                    self.component(
                        snapshot,
                        &ResolvedAction::Click {
                            point: *point,
                            button: MouseButton::Left,
                            count: 1,
                        },
                        cancel,
                        &mut sent,
                    )?;
                    std::thread::sleep(Duration::from_millis(20));
                    let report = self.inject(
                        snapshot,
                        &ResolvedAction::TypeText {
                            text: text.clone(),
                            input_mode: *input_mode,
                        },
                        cancel,
                    )?;
                    sent += report.sent_events;
                }
                ResolvedAction::KeyAt { point, keys } => {
                    // Validate all keys before the first click.
                    self.call(json!({"op":"validate_keys", "keys":keys}))?;
                    self.component(
                        snapshot,
                        &ResolvedAction::Click {
                            point: *point,
                            button: MouseButton::Left,
                            count: 1,
                        },
                        cancel,
                        &mut sent,
                    )?;
                    std::thread::sleep(Duration::from_millis(20));
                    self.component(
                        snapshot,
                        &ResolvedAction::PressKey { keys: keys.clone() },
                        cancel,
                        &mut sent,
                    )?;
                }
                ResolvedAction::TypeText { text, input_mode } => {
                    validate_text(text, *input_mode)?;
                    for ch in text.chars() {
                        self.component(
                            snapshot,
                            &ResolvedAction::TypeText {
                                text: ch.to_string(),
                                input_mode: *input_mode,
                            },
                            cancel,
                            &mut sent,
                        )?;
                    }
                }
                _ => self.component(snapshot, action, cancel, &mut sent)?,
            }
            Ok(InjectionReport { sent_events: sent })
        })();
        result.map_err(|e: DesktopError| {
            let partial = e.sent_events.unwrap_or(0);
            e.with_sent(sent + partial)
        })
    }
}
fn validate_text(text: &str, mode: TextInputMode) -> Result<()> {
    if text.chars().any(char::is_control)
        || mode == TextInputMode::Auto
        || (mode == TextInputMode::ScanCode && !text.bytes().all(|b| (b' '..=b'~').contains(&b)))
    {
        return Err(DesktopError::invalid(
            "Resolve text mode; text must be literal and scan_code supports printable ASCII only",
        ));
    }
    Ok(())
}
pub fn diagnostics(id: Option<&WindowId>) -> Result<Value> {
    Ok(Bridge::start()?
        .call(json!({"op":"diagnostics", "id":id}))?
        .value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_reply_preserves_next_header_and_rejects_truncation() {
        let mut stream = std::io::Cursor::new(b"{\"payload_len\":3}\n\x00\x0a\xff{}\n".to_vec());
        assert_eq!(read_reply(&mut stream).unwrap().pixels, [0, 10, 255]);
        assert!(read_reply(&mut stream).unwrap().pixels.is_empty());
        assert!(read_reply(&mut stream).is_err());
        assert!(read_reply(&mut std::io::Cursor::new(b"{\"payload_len\":3}\nx")).is_err());
        assert!(read_reply(&mut std::io::Cursor::new(b"{\"payload_len\":67108865}\n")).is_err());
    }
    #[test]
    fn validates_entire_text_before_input() {
        assert!(validate_text("hello\n", TextInputMode::Unicode).is_err());
        assert!(validate_text("hello中", TextInputMode::ScanCode).is_err());
        assert!(validate_text("hello", TextInputMode::Auto).is_err());
        assert!(validate_text("中文🙂", TextInputMode::Unicode).is_ok());
    }
}
