use crate::{
    audit::Audit,
    control::Control,
    error::{DesktopError, Result},
    geometry::{ImageTransform, Rect},
    image::RgbImage,
    platform::DesktopPlatform,
    types::*,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Frame {
    pub metadata: FrameMetadata,
    pub png: Vec<u8>,
    pub image: RgbImage,
    created: Instant,
    snapshot: WindowSnapshot,
}
struct Session {
    id: String,
    identity: WindowIdentity,
    viewport: Option<Rect>,
    keyboard_generation: Option<u64>,
}

pub struct DesktopService {
    platform: Box<dyn DesktopPlatform>,
    pub control: Arc<Control>,
    audit: Audit,
    data_dir: PathBuf,
    session: Option<Session>,
    pub frame: Option<Frame>,
    geometry_version: u64,
    sequence: u64,
    instance_id: String,
    last_geometry: Option<WindowSnapshot>,
    pub frame_ttl: Duration,
    pub change_threshold: f64,
}

impl DesktopService {
    pub fn new(
        platform: Box<dyn DesktopPlatform>,
        control: Arc<Control>,
        data_dir: PathBuf,
    ) -> Result<Self> {
        let audit = Audit::open(&data_dir.join("audit.jsonl"))?;
        std::fs::create_dir_all(data_dir.join("frames")).map_err(storage_error)?;
        Ok(Self {
            platform,
            control,
            audit,
            data_dir,
            session: None,
            frame: None,
            geometry_version: 0,
            sequence: 0,
            instance_id: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            last_geometry: None,
            frame_ttl: Duration::from_secs(120),
            change_threshold: 0.003,
        })
    }
    pub fn capabilities(&self) -> Capabilities {
        self.platform.capabilities()
    }
    pub fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        self.platform.list_windows()
    }
    pub fn status(&self) -> Value {
        json!({"capabilities":self.capabilities(), "permissions":self.platform.permissions(), "pause_reason":self.control.reason(),
            "control_generation":self.control.generation(), "session_id":self.session.as_ref().map(|s| &s.id),
            "geometry_version":self.geometry_version, "frame":self.frame.as_ref().map(|f| &f.metadata),
            "control_model":"exclusive_process_per_desktop", "frame_ttl_seconds":self.frame_ttl.as_secs()})
    }
    pub fn attach(&mut self, window_id: WindowId, viewport: Option<Rect>) -> Result<Value> {
        let snapshot = self.platform.inspect(&window_id, viewport)?;
        self.control.cancel_current();
        self.frame = None;
        self.last_geometry = None;
        self.geometry_version += 1;
        self.sequence += 1;
        self.session = Some(Session {
            id: format!("session-{}-{}", self.instance_id, self.sequence),
            identity: snapshot.identity,
            viewport,
            keyboard_generation: None,
        });
        Ok(self.status())
    }
    pub fn detach(&mut self) -> Value {
        self.control.cancel_current();
        self.session = None;
        self.frame = None;
        self.status()
    }
    fn snapshot(&self) -> Result<WindowSnapshot> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopError::new("NOT_ATTACHED", "Select and attach a window first"))?;
        let snapshot = self
            .platform
            .inspect(&session.identity.id, session.viewport)?;
        if snapshot.identity != session.identity {
            return Err(DesktopError::new(
                "WINDOW_LOST",
                "Window identity changed; attach explicitly again",
            ));
        }
        Ok(snapshot)
    }
    pub fn focus(&mut self) -> Result<FrameMetadata> {
        let token = self.control.token(false);
        token.check()?;
        let snapshot = self.snapshot()?;
        self.frame = None;
        if let Some(s) = &mut self.session {
            s.keyboard_generation = None;
        }
        self.platform.focus(&snapshot.identity, &token)?;
        token.check()?;
        self.control.cancel_current();
        self.observe()
    }
    pub fn observe(&mut self) -> Result<FrameMetadata> {
        self.frame = None;
        let result = self.observe_inner();
        if result.is_err() {
            if let Some(session) = &mut self.session {
                session.keyboard_generation = None;
            }
        }
        result
    }
    fn observe_inner(&mut self) -> Result<FrameMetadata> {
        let cancel = self.control.token(true);
        cancel.check()?;
        let before = self.snapshot()?;
        let image = self.platform.capture(&before, &cancel)?;
        image.validate()?;
        let after = self.snapshot()?;
        if before != after {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Window moved during capture",
            ));
        }
        cancel.check()?;
        if image.width != before.content_screen.width
            || image.height != before.content_screen.height
        {
            return Err(DesktopError::new(
                "INVALID_FRAME",
                "Capture size does not match physical viewport",
            ));
        }
        if self.last_geometry.as_ref().is_some_and(|s| *s != before) {
            self.geometry_version += 1;
            self.session.as_mut().unwrap().keyboard_generation = None;
        }
        self.last_geometry = Some(before.clone());
        self.sequence += 1;
        let metadata = FrameMetadata {
            session_id: self.session.as_ref().unwrap().id.clone(),
            frame_id: format!("frame-{}-{}", self.instance_id, self.sequence),
            control_generation: cancel.generation(),
            geometry_version: self.geometry_version,
            captured_at_ms: now_ms(),
            transform: ImageTransform {
                image_width: image.width,
                image_height: image.height,
                content_screen: before.content_screen,
            },
            capture_backend: self.capabilities().capture_backend,
        };
        let png = image.png()?;
        if png.len() > 8 * 1024 * 1024 {
            return Err(DesktopError::new(
                "FRAME_TOO_LARGE",
                "Encoded screenshot exceeds 8 MiB; calibrate a smaller viewport",
            ));
        }
        std::fs::write(
            self.data_dir
                .join("frames")
                .join(format!("{}.png", metadata.frame_id)),
            &png,
        )
        .map_err(storage_error)?;
        self.frame = Some(Frame {
            metadata: metadata.clone(),
            png,
            image,
            created: Instant::now(),
            snapshot: before,
        });
        Ok(metadata)
    }
    pub fn get_action(&self, id: &str) -> Result<ActionReceipt> {
        self.audit.get(id)
    }
    pub fn perform(&mut self, request: ActionRequest) -> Result<ActionReceipt> {
        let result = self.perform_inner(request);
        if result.is_err() {
            self.frame = None;
            if let Some(session) = &mut self.session {
                session.keyboard_generation = None;
            }
        }
        result
    }
    fn perform_inner(&mut self, request: ActionRequest) -> Result<ActionReceipt> {
        if request.action_id.is_empty() || request.action_id.len() > 128 {
            return Err(DesktopError::invalid(
                "action_id must contain 1..=128 bytes",
            ));
        }
        if let Some(receipt) = self.audit.lookup(&request)? {
            return Ok(receipt);
        }
        let cancel = self.control.token(false);
        cancel.check()?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| DesktopError::new("NOT_ATTACHED", "No session"))?;
        if request.session_id != session.id {
            return Err(DesktopError::new(
                "SESSION_CHANGED",
                "session_id does not match active binding",
            ));
        }
        let frame = self
            .frame
            .as_ref()
            .ok_or_else(|| DesktopError::new("STALE_FRAME", "Observe before sending input"))?;
        if request.frame_id != frame.metadata.frame_id
            || frame.metadata.control_generation != self.control.generation()
            || frame.created.elapsed() > self.frame_ttl
        {
            return Err(DesktopError::new(
                "STALE_FRAME",
                "Frame expired, was consumed, or predates a pause; observe again",
            ));
        }
        let current = self.snapshot()?;
        if current != frame.snapshot {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Observe after geometry changes",
            ));
        }
        let action = resolve(&request.action, frame.metadata.transform)?;
        if matches!(
            action,
            ResolvedAction::TypeText { .. } | ResolvedAction::PressKey { .. }
        ) && session.keyboard_generation != Some(self.control.generation())
        {
            return Err(DesktopError::new(
                "CONTENT_FOCUS_REQUIRED",
                "Click the remote content/input field before typing or sending keys",
            ));
        }
        let fresh = self.platform.capture(&current, &cancel)?;
        fresh.validate()?;
        if self.snapshot()? != current {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Window changed during preflight",
            ));
        }
        if frame.image.difference(&fresh) > self.change_threshold {
            return Err(DesktopError::new(
                "STALE_FRAME",
                "Visible scene changed; observe again before acting",
            ));
        }
        cancel.check()?;
        self.audit.intent(&request)?;
        // Consume the observation even if injection fails: never reuse ambiguous coordinates.
        self.frame = None;
        let outcome = cancel
            .check()
            .and_then(|_| self.platform.inject(&current, &action, &cancel));
        let mut receipt = match outcome {
            Ok(report) => {
                if matches!(action, ResolvedAction::Click { .. }) {
                    self.session.as_mut().unwrap().keyboard_generation = Some(cancel.generation());
                }
                ActionReceipt {
                    action_id: request.action_id,
                    execution_status: "injected".into(),
                    verification_status: "unverified".into(),
                    sent_events: report.sent_events,
                    next_frame_id: None,
                    error: None,
                }
            }
            Err(error) => {
                self.session.as_mut().unwrap().keyboard_generation = None;
                ActionReceipt {
                    action_id: request.action_id,
                    execution_status: if error.sent_events.unwrap_or(0) > 0 {
                        "uncertain"
                    } else {
                        "rejected"
                    }
                    .into(),
                    verification_status: "uncertain".into(),
                    sent_events: error.sent_events.unwrap_or(0),
                    next_frame_id: None,
                    error: Some(error),
                }
            }
        };
        if receipt.execution_status == "uncertain" {
            self.control.pause_agent();
        }
        // Persist the native outcome BEFORE waiting for an after-frame.
        if let Err(error) = self.audit.outcome(&receipt) {
            self.control.pause_agent();
            return Err(error);
        }
        if receipt.execution_status == "injected" {
            for _ in 0..6 {
                if cancel.check().is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if cancel.check().is_ok() {
                match self.observe() {
                    Ok(frame) => receipt.next_frame_id = Some(frame.frame_id),
                    Err(e) => {
                        receipt.error =
                            Some(DesktopError::new("POST_CAPTURE_FAILED", e.to_string()))
                    }
                }
            }
            if let Err(error) = self.audit.outcome(&receipt) {
                self.control.pause_agent();
                return Err(error);
            }
        }
        Ok(receipt)
    }
    pub fn wait(&mut self, mode: &str, timeout_ms: u64) -> Result<Value> {
        if !matches!(mode, "changed" | "stable" | "duration") || timeout_ms > 60_000 {
            return Err(DesktopError::invalid(
                "mode must be changed/stable/duration; timeout_ms <= 60000",
            ));
        }
        let cancel = self.control.token(false);
        cancel.check()?;
        let start = Instant::now();
        let baseline = self
            .frame
            .as_ref()
            .filter(|frame| {
                frame.metadata.control_generation == cancel.generation()
                    && frame.created.elapsed() <= self.frame_ttl
            })
            .map(|frame| frame.image.clone());
        self.observe()?;
        let first = self.frame.as_ref().unwrap();
        let baseline = baseline.unwrap_or_else(|| first.image.clone());
        if mode == "changed" && baseline.difference(&first.image) > self.change_threshold {
            return Ok(json!({"condition_met":true,"timed_out":false,"frame":first.metadata}));
        }
        let mut previous = first.image.clone();
        let mut last_sample = Instant::now();
        let mut stable = 0;
        loop {
            cancel.check()?;
            let remaining = Duration::from_millis(timeout_ms).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(
                    json!({"condition_met": mode == "duration", "timed_out": mode != "duration", "frame":self.frame.as_ref().map(|f| &f.metadata)}),
                );
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
            if last_sample.elapsed() < Duration::from_millis(150) {
                continue;
            }
            last_sample = Instant::now();
            self.observe()?;
            let current = &self.frame.as_ref().unwrap().image;
            let changed = baseline.difference(current) > self.change_threshold;
            if previous.difference(current) <= self.change_threshold {
                stable += 1;
            } else {
                stable = 0;
            }
            previous = current.clone();
            if (mode == "changed" && changed)
                || (mode == "stable"
                    && stable >= 3
                    && start.elapsed() >= Duration::from_millis(450))
            {
                return Ok(
                    json!({"condition_met":true,"timed_out":false,"frame":self.frame.as_ref().map(|f| &f.metadata)}),
                );
            }
        }
    }
}

fn resolve(action: &Action, transform: ImageTransform) -> Result<ResolvedAction> {
    Ok(match action {
        Action::Click {
            point,
            button,
            count,
        } => {
            if !(1..=2).contains(count) {
                return Err(DesktopError::invalid("click count must be 1 or 2"));
            }
            ResolvedAction::Click {
                point: transform.map(*point)?,
                button: *button,
                count: *count,
            }
        }
        Action::Move { point } => ResolvedAction::Move {
            point: transform.map(*point)?,
        },
        Action::Scroll {
            point,
            ticks,
            horizontal,
        } => {
            if *ticks == 0 || ticks.unsigned_abs() > 100 {
                return Err(DesktopError::invalid(
                    "ticks must be nonzero and within -100..100",
                ));
            }
            ResolvedAction::Scroll {
                point: transform.map(*point)?,
                ticks: *ticks,
                horizontal: *horizontal,
            }
        }
        Action::TypeText { text, .. } => {
            if text.len() > 65536 {
                return Err(DesktopError::invalid(
                    "Split text into requests of at most 65536 UTF-8 bytes",
                ));
            }
            ResolvedAction::TypeText { text: text.clone() }
        }
        Action::PressKey { keys } => {
            if keys.is_empty() || keys.len() > 8 {
                return Err(DesktopError::invalid("Specify 1..=8 simultaneous keys"));
            }
            ResolvedAction::PressKey { keys: keys.clone() }
        }
    })
}
fn storage_error(e: std::io::Error) -> DesktopError {
    DesktopError::new("STORAGE_UNAVAILABLE", e.to_string())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
