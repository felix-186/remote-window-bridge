use crate::{
    audit::Audit,
    control::Control,
    error::{DesktopError, Result},
    geometry::{ImageTransform, Rect},
    image::RgbImage,
    performance::Performance,
    platform::DesktopPlatform,
    storage::FrameStore,
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
/// A validated native sample. Intermediate wait samples never encode or touch disk.
struct Sample {
    image: RgbImage,
    snapshot: WindowSnapshot,
    generation: u64,
    captured_at_ms: u64,
    created: Instant,
}
struct Session {
    id: String,
    identity: WindowIdentity,
    viewport: Option<Rect>,
    keyboard_generation: Option<u64>,
}

pub struct DesktopService {
    pub performance: Arc<Performance>,
    platform: Box<dyn DesktopPlatform>,
    pub control: Arc<Control>,
    audit: Audit,
    pub frame_store: FrameStore,
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
        let data_dir = if data_dir.is_absolute() {
            data_dir
        } else {
            std::env::current_dir()
                .map_err(storage_error)?
                .join(data_dir)
        };
        let audit = Audit::open(&data_dir.join("audit.jsonl"))?;
        let frame_store = FrameStore::new(data_dir.join("frames"))?;
        Ok(Self {
            performance: Arc::new(Performance::default()),
            platform,
            control,
            audit,
            frame_store,
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
    pub fn check_session(&self, id: &str) -> Result<()> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == id)
        {
            Ok(())
        } else {
            Err(DesktopError::new(
                "SESSION_CHANGED",
                "Use the session_id returned by attach",
            ))
        }
    }
    pub fn status(&self) -> Value {
        json!({"server":{"name":env!("CARGO_PKG_NAME"),"version":env!("CARGO_PKG_VERSION"),"process_id":std::process::id(),"default_text_input_mode":"auto"},
            "performance":self.performance.snapshot(),
            "storage":{"frames":self.frame_store.retention,"cleanup_error":self.frame_store.cleanup_error,"audit":self.audit.status()},
            "window_diagnostics":self.platform.diagnostics(self.session.as_ref().map(|session| &session.identity.id)).unwrap_or_else(|error| json!({"error":error})),
            "binding":self.session.as_ref().map(|session| json!({"identity":session.identity,"viewport":session.viewport})),
            "capabilities":self.capabilities(), "permissions":self.platform.permissions(), "pause_reason":self.control.reason(),
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
        let _timing = self.performance.stage("inspect");
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
        self.focus_with_timeout(0)
    }
    pub fn focus_with_timeout(&mut self, timeout_ms: u64) -> Result<FrameMetadata> {
        if timeout_ms > 60_000 {
            return Err(DesktopError::invalid("timeout_ms must be <= 60000"));
        }
        let token = self.control.token(false);
        token.check()?;
        self.frame = None;
        if let Some(s) = &mut self.session {
            s.keyboard_generation = None;
        }
        self.platform.begin_activation();
        let start = Instant::now();
        loop {
            token.check()?;
            let snapshot = self.snapshot()?;
            let focused = {
                let _timing = self.performance.stage("activate");
                self.platform.focus(&snapshot.identity, &token)
            };
            let result = focused.and_then(|()| {
                token.check()?;
                let frame = self.observe_with_activation(false)?;
                token.check()?;
                Ok(frame)
            });
            match result {
                Ok(frame) => return Ok(frame),
                Err(error) if error.code == "FOREGROUND_REQUIRED" => {
                    let remaining =
                        Duration::from_millis(timeout_ms).saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        return Err(error);
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(50)));
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn observe(&mut self) -> Result<FrameMetadata> {
        self.platform.begin_activation();
        self.observe_with_activation(true)
    }
    fn observe_with_activation(&mut self, activate: bool) -> Result<FrameMetadata> {
        self.frame = None;
        let result = self.observe_inner(activate);
        if result.is_err() {
            if let Some(session) = &mut self.session {
                session.keyboard_generation = None;
            }
        }
        result
    }
    fn observe_inner(&mut self, activate: bool) -> Result<FrameMetadata> {
        let sample = self.sample(activate)?;
        self.publish(sample)
    }
    fn sample(&mut self, activate: bool) -> Result<Sample> {
        let cancel = self.control.token(true);
        cancel.check()?;
        let before = self.snapshot()?;
        let activated = if activate {
            let _timing = self.performance.stage("activate");
            self.platform
                .prepare_observation(&before.identity, &cancel)?
        } else {
            false
        };
        if activated {
            self.session.as_mut().unwrap().keyboard_generation = None;
        }
        let image = {
            let _timing = self.performance.stage("capture");
            self.platform.capture(&before, &cancel)?
        };
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
        Ok(Sample {
            image,
            snapshot: before,
            generation: cancel.generation(),
            captured_at_ms: now_ms(),
            created: Instant::now(),
        })
    }
    fn publish(&mut self, sample: Sample) -> Result<FrameMetadata> {
        let Sample {
            image,
            snapshot: before,
            generation,
            captured_at_ms,
            created,
        } = sample;
        let cancel = self.control.token(true);
        cancel.check()?;
        if generation != cancel.generation() {
            return Err(DesktopError::new(
                "CANCELLED",
                "Sample predates control change",
            ));
        }
        if self.last_geometry.as_ref().is_some_and(|s| *s != before) {
            self.geometry_version += 1;
            self.session.as_mut().unwrap().keyboard_generation = None;
        }
        self.last_geometry = Some(before.clone());
        self.sequence += 1;
        let frame_id = format!("frame-{}-{}", self.instance_id, self.sequence);
        let metadata = FrameMetadata {
            session_id: self.session.as_ref().unwrap().id.clone(),
            image_path: self.frame_store.path(&frame_id),
            frame_id,
            control_generation: generation,
            geometry_version: self.geometry_version,
            captured_at_ms,
            transform: ImageTransform {
                image_width: image.width,
                image_height: image.height,
                content_screen: before.content_screen,
            },
            capture_backend: self.capabilities().capture_backend,
        };
        let png = {
            let _timing = self.performance.stage("encode_png");
            image.png()?
        };
        cancel.check()?;
        if png.len() > 8 * 1024 * 1024 {
            return Err(DesktopError::new(
                "FRAME_TOO_LARGE",
                "Encoded screenshot exceeds 8 MiB; calibrate a smaller viewport",
            ));
        }
        {
            let _timing = self.performance.stage("frame_write");
            self.frame_store.write(&metadata.image_path, &png)?;
        }
        cancel.check()?;
        self.frame = Some(Frame {
            metadata: metadata.clone(),
            png,
            image,
            created,
            snapshot: before,
        });
        Ok(metadata)
    }
    pub fn get_action(&self, id: &str) -> Result<ActionReceipt> {
        self.audit.get(id)
    }
    pub fn perform(&mut self, request: ActionRequest) -> Result<ActionReceipt> {
        let _timing = self.performance.stage("action_total");
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
        let settle_ms = match &request.action {
            Action::Fill { settle_ms, .. } | Action::KeyAt { settle_ms, .. } => *settle_ms,
            _ => 150,
        };
        if settle_ms > 5000 {
            return Err(DesktopError::invalid("settle_ms must be <= 5000"));
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
        // These explicit composite actions own their focus recovery, but still
        // validate the original frame/geometry and fresh pixels before injection.
        if matches!(
            action,
            ResolvedAction::Fill { .. } | ResolvedAction::KeyAt { .. }
        ) {
            self.platform.begin_activation();
            let _timing = self.performance.stage("activate");
            self.platform.focus(&current.identity, &cancel)?;
        }
        let fresh = {
            let _timing = self.performance.stage("capture");
            self.platform.capture(&current, &cancel)?
        };
        fresh.validate()?;
        if self.snapshot()? != current {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Window changed during preflight",
            ));
        }
        let scene_changed = {
            let _timing = self.performance.stage("scene_check");
            let point = match &action {
                ResolvedAction::Click { point, .. }
                | ResolvedAction::Fill { point, .. }
                | ResolvedAction::KeyAt { point, .. }
                | ResolvedAction::Move { point }
                | ResolvedAction::Scroll { point, .. } => Some(*point),
                _ => None,
            };
            frame.image.difference(&fresh) > self.change_threshold
                || point.is_some_and(|point| {
                    frame.image.difference_near(
                        &fresh,
                        i64::from(point.x) - i64::from(current.content_screen.x),
                        i64::from(point.y) - i64::from(current.content_screen.y),
                    ) > 0.04
                })
        };
        if scene_changed {
            return Err(DesktopError::new(
                "STALE_FRAME",
                "Visible scene or target neighborhood changed; observe again before acting",
            ));
        }
        cancel.check()?;
        {
            let _timing = self.performance.stage("audit");
            self.audit.intent(&request)?;
        }
        // Consume the observation even if injection fails: never reuse ambiguous coordinates.
        self.frame = None;
        let injection_timing = self.performance.stage("inject");
        let outcome = cancel
            .check()
            .and_then(|_| self.platform.inject(&current, &action, &cancel));
        drop(injection_timing);
        let mut receipt = match outcome {
            Ok(report) => {
                if matches!(
                    action,
                    ResolvedAction::Click { .. }
                        | ResolvedAction::Fill { .. }
                        | ResolvedAction::KeyAt { .. }
                ) {
                    self.session.as_mut().unwrap().keyboard_generation = Some(cancel.generation());
                }
                ActionReceipt {
                    action_id: request.action_id,
                    execution_status: "injected".into(),
                    verification_status: "unverified".into(),
                    text_input_mode: None,
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
                    text_input_mode: None,
                    sent_events: error.sent_events.unwrap_or(0),
                    next_frame_id: None,
                    error: Some(error),
                }
            }
        };
        if let ResolvedAction::TypeText { input_mode, .. }
        | ResolvedAction::Fill { input_mode, .. } = action
        {
            receipt.text_input_mode = Some(input_mode);
        }
        if receipt.execution_status == "uncertain" {
            self.control.pause_agent();
        }
        // Persist the native outcome BEFORE waiting for an after-frame.
        let audit_result = {
            let _timing = self.performance.stage("audit");
            self.audit.outcome(&receipt)
        };
        if let Err(error) = audit_result {
            self.control.pause_agent();
            return Err(error);
        }
        if receipt.execution_status == "injected" {
            let settle_timing = self.performance.stage("settle");
            let settle_start = Instant::now();
            while settle_start.elapsed() < Duration::from_millis(settle_ms) {
                if cancel.check().is_err() {
                    break;
                }
                let remaining =
                    Duration::from_millis(settle_ms).saturating_sub(settle_start.elapsed());
                std::thread::sleep(remaining.min(Duration::from_millis(25)));
            }
            drop(settle_timing);
            if cancel.check().is_ok() {
                match self.observe_with_activation(false) {
                    Ok(frame) => receipt.next_frame_id = Some(frame.frame_id),
                    Err(e) => {
                        receipt.error =
                            Some(DesktopError::new("POST_CAPTURE_FAILED", e.to_string()))
                    }
                }
            }
            let audit_result = {
                let _timing = self.performance.stage("audit");
                self.audit.outcome(&receipt)
            };
            if let Err(error) = audit_result {
                self.control.pause_agent();
                return Err(error);
            }
        }
        Ok(receipt)
    }
    pub fn wait(&mut self, mode: &str, timeout_ms: u64) -> Result<Value> {
        let result = self.wait_inner(mode, timeout_ms);
        if result.is_err() {
            self.frame = None;
            if let Some(session) = &mut self.session {
                session.keyboard_generation = None;
            }
        }
        result
    }
    fn wait_inner(&mut self, mode: &str, timeout_ms: u64) -> Result<Value> {
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
        self.frame = None;
        self.platform.begin_activation();
        let mut sample = self.sample(true)?;
        let baseline = baseline.unwrap_or_else(|| sample.image.clone());
        if mode == "changed" && baseline.difference(&sample.image) > self.change_threshold {
            let frame = self.publish(sample)?;
            return Ok(json!({"condition_met":true,"timed_out":false,"frame":frame}));
        }
        let mut last_sample = Instant::now();
        let mut stable = 0;
        loop {
            cancel.check()?;
            let remaining = Duration::from_millis(timeout_ms).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                let frame = self.publish(sample)?;
                return Ok(
                    json!({"condition_met": mode == "duration", "timed_out": mode != "duration", "frame":frame}),
                );
            }
            std::thread::sleep(remaining.min(Duration::from_millis(25)));
            if last_sample.elapsed() < Duration::from_millis(150) {
                continue;
            }
            last_sample = Instant::now();
            let next = self.sample(false)?;
            if sample.snapshot != next.snapshot {
                return Err(DesktopError::new(
                    "GEOMETRY_CHANGED",
                    "Window changed while waiting",
                ));
            }
            let current = &next.image;
            let changed = baseline.difference(current) > self.change_threshold;
            if sample.image.difference(current) <= self.change_threshold {
                stable += 1;
            } else {
                stable = 0;
            }
            sample = next;
            if (mode == "changed" && changed)
                || (mode == "stable"
                    && stable >= 3
                    && start.elapsed() >= Duration::from_millis(450))
            {
                let frame = self.publish(sample)?;
                return Ok(json!({"condition_met":true,"timed_out":false,"frame":frame}));
            }
        }
    }
}

fn resolve(action: &Action, transform: ImageTransform) -> Result<ResolvedAction> {
    Ok(match action {
        Action::Fill {
            point,
            text,
            input_mode,
            ..
        } => {
            if text.len() > 65536 {
                return Err(DesktopError::invalid(
                    "Text must be at most 65536 UTF-8 bytes",
                ));
            }
            if text.chars().any(char::is_control) {
                return Err(DesktopError::invalid(
                    "Fill accepts literal text only; send Enter/Tab separately",
                ));
            }
            ResolvedAction::Fill {
                point: transform.map(*point)?,
                text: text.clone(),
                input_mode: input_mode.resolved(text),
            }
        }
        Action::KeyAt { point, keys, .. } => {
            if keys.is_empty() || keys.len() > 8 {
                return Err(DesktopError::invalid("Specify 1..=8 simultaneous keys"));
            }
            ResolvedAction::KeyAt {
                point: transform.map(*point)?,
                keys: keys.clone(),
            }
        }
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
        Action::TypeText {
            text, input_mode, ..
        } => {
            if text.len() > 65536 {
                return Err(DesktopError::invalid(
                    "Split text into requests of at most 65536 UTF-8 bytes",
                ));
            }
            ResolvedAction::TypeText {
                text: text.clone(),
                input_mode: input_mode.resolved(text),
            }
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
