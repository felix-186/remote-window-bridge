use remote_window_bridge::{
    audit::Audit,
    control::{Cancellation, Control},
    error::{DesktopError, Result},
    geometry::*,
    image::RgbImage,
    mcp,
    platform::{CaptureBackend, InputBackend, WindowBackend},
    service::DesktopService,
    types::*,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "remote-window-bridge-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct State {
    snapshot: WindowSnapshot,
    image: RgbImage,
    injections: u32,
    failure: Option<DesktopError>,
    capture_failure: bool,
    focus_failures: u32,
    focus_attempts: u32,
    foreground: bool,
    lose_foreground_after_capture: bool,
    last_text_input_mode: Option<TextInputMode>,
    capture_foreground_failures: u32,
    activation_batches: u32,
}
struct Fake(Arc<Mutex<State>>);
impl WindowBackend for Fake {
    fn begin_activation(&self) {
        self.0.lock().unwrap().activation_batches += 1;
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "test".into(),
            window_capture: true,
            capture_backend: "fake".into(),
            requires_visible: true,
            input: true,
            ocr: false,
            global_pause_hotkey: false,
            notes: vec![],
        }
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(vec![WindowInfo {
            identity: self.0.lock().unwrap().snapshot.identity.clone(),
            title: "TEST ONLY".into(),
            executable: "fake".into(),
        }])
    }
    fn inspect(&self, _: &WindowId, viewport: Option<Rect>) -> Result<WindowSnapshot> {
        let mut s = self.0.lock().unwrap().snapshot.clone();
        if let Some(v) = viewport {
            s.content_screen = s.client_screen.crop(v)?;
        }
        Ok(s)
    }
    fn focus(&self, _: &WindowIdentity, cancel: &Cancellation) -> Result<()> {
        cancel.check()?;
        let mut state = self.0.lock().unwrap();
        state.focus_attempts += 1;
        if state.focus_failures > 0 {
            state.focus_failures -= 1;
            return Err(DesktopError::new(
                "FOREGROUND_REQUIRED",
                "test foreground refusal",
            ));
        }
        state.foreground = true;
        Ok(())
    }
    fn prepare_observation(
        &self,
        identity: &WindowIdentity,
        cancel: &Cancellation,
    ) -> Result<bool> {
        if self.0.lock().unwrap().foreground {
            return Ok(false);
        }
        cancel.check_running()?;
        self.focus(identity, cancel)?;
        Ok(true)
    }
}
impl CaptureBackend for Fake {
    fn capture(&self, _: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage> {
        cancel.check()?;
        let mut state = self.0.lock().unwrap();
        if !state.foreground {
            return Err(DesktopError::new(
                "FOREGROUND_REQUIRED",
                "test lost foreground",
            ));
        }
        if state.capture_foreground_failures > 0 {
            state.capture_foreground_failures -= 1;
            state.foreground = false;
            return Err(DesktopError::new(
                "FOREGROUND_REQUIRED",
                "test activation lost before capture",
            ));
        }
        if state.capture_failure {
            return Err(DesktopError::new(
                "CAPTURE_UNAVAILABLE",
                "test capture failure",
            ));
        }
        let image = state.image.clone();
        if state.lose_foreground_after_capture {
            state.foreground = false;
        }
        Ok(image)
    }
}
impl InputBackend for Fake {
    fn inject(
        &self,
        _: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
    ) -> Result<InjectionReport> {
        cancel.check()?;
        let mut state = self.0.lock().unwrap();
        if let Some(e) = state.failure.clone() {
            return Err(e);
        }
        state.injections += 1;
        if let ResolvedAction::TypeText { input_mode, .. }
        | ResolvedAction::Fill { input_mode, .. } = action
        {
            state.last_text_input_mode = Some(*input_mode);
        }
        Ok(InjectionReport { sent_events: 3 })
    }
}
struct Fixture {
    service: DesktopService,
    state: Arc<Mutex<State>>,
    _temp: Temp,
}
impl Fixture {
    fn new() -> Self {
        let temp = Temp::new();
        let state = Arc::new(Mutex::new(State {
            snapshot: WindowSnapshot {
                activation: None,
                input_surface: None,
                identity: WindowIdentity {
                    id: WindowId("test:1".into()),
                    process_id: 4,
                    process_started: 42,
                    class_name: "fake".into(),
                },
                client_screen: Rect {
                    x: -1200,
                    y: 100,
                    width: 32,
                    height: 24,
                },
                content_screen: Rect {
                    x: -1200,
                    y: 100,
                    width: 32,
                    height: 24,
                },
                dpi: 144,
            },
            image: RgbImage {
                width: 32,
                height: 24,
                pixels: vec![100; 32 * 24 * 3],
            },
            injections: 0,
            failure: None,
            capture_failure: false,
            focus_failures: 0,
            focus_attempts: 0,
            foreground: true,
            lose_foreground_after_capture: false,
            last_text_input_mode: None,
            capture_foreground_failures: 0,
            activation_batches: 0,
        }));
        let mut service = DesktopService::new(
            Box::new(Fake(state.clone())),
            Arc::new(Control::default()),
            temp.0.clone(),
        )
        .unwrap();
        service.attach(WindowId("test:1".into()), None).unwrap();
        Self {
            service,
            state,
            _temp: temp,
        }
    }
    fn click(&mut self, id: &str) -> ActionRequest {
        let frame = self.service.observe().unwrap();
        ActionRequest {
            session_id: frame.session_id,
            frame_id: frame.frame_id,
            action_id: id.into(),
            action: Action::Click {
                point: Point {
                    x: 10.0,
                    y: 10.0,
                    coordinate_space: CoordinateSpace::ImagePixels,
                },
                button: MouseButton::Left,
                count: 1,
            },
        }
    }
}

#[test]
fn wait_does_not_keep_reclaiming_foreground_after_the_first_observation() {
    let mut f = Fixture::new();
    {
        let mut state = f.state.lock().unwrap();
        state.foreground = false;
        state.lose_foreground_after_capture = true;
    }
    assert_eq!(
        f.service.wait("stable", 1000).unwrap_err().code,
        "FOREGROUND_REQUIRED"
    );
    assert_eq!(f.state.lock().unwrap().focus_attempts, 1);
    assert!(!f.state.lock().unwrap().foreground);
    assert!(f.service.frame.is_none());
}

#[test]
fn wait_publishes_only_one_frame_for_all_intermediate_samples() {
    let mut f = Fixture::new();
    let result = f.service.wait("stable", 1500).unwrap();
    assert_eq!(result["condition_met"], true);
    assert_eq!(
        std::fs::read_dir(f._temp.0.join("frames")).unwrap().count(),
        1
    );
    assert!(f
        .service
        .frame
        .as_ref()
        .unwrap()
        .metadata
        .image_path
        .is_file());
}

#[test]
fn small_target_change_cannot_hide_inside_a_large_viewport() {
    let mut f = Fixture::new();
    {
        let mut state = f.state.lock().unwrap();
        state.snapshot.client_screen.width = 960;
        state.snapshot.client_screen.height = 600;
        state.snapshot.content_screen = state.snapshot.client_screen;
        state.image = RgbImage {
            width: 960,
            height: 600,
            pixels: vec![100; 960 * 600 * 3],
        };
    }
    let mut request = f.click("changed-target");
    if let Action::Click { point, .. } = &mut request.action {
        point.x = 480.0;
        point.y = 300.0;
    }
    {
        let mut state = f.state.lock().unwrap();
        for y in 290..310 {
            for x in 470..490 {
                let offset = (y * 960 + x) * 3;
                state.image.pixels[offset..offset + 3].fill(240);
            }
        }
    }
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn stage_timings_include_sampling_and_only_one_publication() {
    let mut f = Fixture::new();
    f.service.performance.begin("test_wait", 12);
    f.service.wait("stable", 1500).unwrap();
    f.service.performance.finish();
    let report = f.service.performance.snapshot();
    let report = &report["recent"][0];
    assert_eq!(report["stages_us"]["queue"], 12);
    assert!(report["calls"]["capture"].as_u64().unwrap() >= 4);
    assert_eq!(report["calls"]["encode_png"], 1);
    assert_eq!(report["calls"]["frame_write"], 1);
    for _ in 0..40 {
        f.service.performance.begin("bounded", 0);
        f.service.performance.finish();
    }
    assert_eq!(
        f.service.performance.snapshot()["recent"]
            .as_array()
            .unwrap()
            .len(),
        32
    );
}

#[test]
fn screenshot_retention_keeps_current_and_unrelated_files() {
    use remote_window_bridge::storage::FrameStore;
    let temp = Temp::new();
    let mut store = FrameStore::new(temp.0.join("frames")).unwrap();
    store.retention.max_files = 2;
    store.retention.grace_secs = 0;
    let keep = store.path("frame-1-1-5");
    for n in 1..=5 {
        std::fs::write(store.path(&format!("frame-1-1-{n}")), [1; 10]).unwrap();
    }
    let foreign = temp.0.join("frames/user-image.png");
    std::fs::write(&foreign, [2; 10]).unwrap();
    store.prune(&keep).unwrap();
    assert!(keep.is_file());
    assert!(foreign.is_file());
    assert_eq!(std::fs::read_dir(temp.0.join("frames")).unwrap().count(), 3);
    // Grace prevents quota cleanup from removing screenshots still in use.
    store.retention.grace_secs = 120;
    store.retention.max_files = 0;
    store.prune(&keep).unwrap();
    assert_eq!(std::fs::read_dir(temp.0.join("frames")).unwrap().count(), 3);
    store.retention.grace_secs = 0;
    store.retention.max_files = 100;
    store.retention.max_bytes = 15;
    store.prune(&keep).unwrap();
    assert_eq!(std::fs::read_dir(temp.0.join("frames")).unwrap().count(), 2);
    let aged = store.path("frame-1-1-6");
    std::fs::write(&aged, [3; 10]).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&aged)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
        .unwrap();
    store.retention.max_bytes = 1000;
    store.prune(&keep).unwrap();
    assert!(!aged.exists());
    assert!(keep.is_file());
    assert!(foreign.is_file());
}

#[test]
fn segmented_audit_preserves_evicted_actions_conflicts_and_uncertainty() {
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    let request = |n: usize| ActionRequest {
        action_id: format!("archive-{n}"),
        session_id: "s".into(),
        frame_id: "f".into(),
        action: Action::PressKey {
            keys: vec!["Enter".into()],
        },
    };
    {
        let mut audit = Audit::open_with_segment_limit(&path, 4096).unwrap();
        for n in 0..520 {
            audit.intent(&request(n)).unwrap();
            if n == 0 {
                let mut receipt = audit.get("archive-0").unwrap();
                receipt.execution_status = "injected".into();
                receipt.sent_events = 2;
                receipt.error = None;
                audit.outcome(&receipt).unwrap();
            }
        }
        assert_eq!(audit.status()["cached_actions"], 512);
        assert_eq!(
            audit.lookup(&request(0)).unwrap().unwrap().execution_status,
            "injected"
        );
        assert_eq!(
            audit.get("archive-1").unwrap().execution_status,
            "uncertain"
        );
        let mut conflict = request(0);
        conflict.frame_id = "different".into();
        assert_eq!(
            audit.lookup(&conflict).unwrap_err().code,
            "ACTION_ID_CONFLICT"
        );
        // Updating an evicted intent must still find its fingerprint on disk.
        let mut receipt = audit.get("archive-1").unwrap();
        receipt.sent_events = 1;
        audit.outcome(&receipt).unwrap();
    }
    let audit = Audit::open(&path).unwrap();
    assert_eq!(audit.get("archive-0").unwrap().execution_status, "injected");
    assert_eq!(audit.get("archive-1").unwrap().sent_events, 1);
    assert_eq!(
        audit.get("archive-2").unwrap().execution_status,
        "uncertain"
    );
    assert!(audit.lookup(&request(999)).unwrap().is_none());
    drop(audit);
    let first = temp
        .0
        .join("audit.jsonl.segment-00000000000000000001.jsonl");
    std::fs::remove_file(first).unwrap();
    assert_eq!(Audit::open(&path).err().unwrap().code, "AUDIT_CORRUPT");
}

#[test]
fn truncated_archived_audit_blocks_recovery_too() {
    use std::io::Write;
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    {
        let mut audit = Audit::open_with_segment_limit(&path, 1).unwrap();
        for n in 0..2 {
            audit
                .intent(&ActionRequest {
                    session_id: "s".into(),
                    frame_id: "f".into(),
                    action_id: format!("a-{n}"),
                    action: Action::PressKey {
                        keys: vec!["Enter".into()],
                    },
                })
                .unwrap();
        }
    }
    let archive = temp
        .0
        .join("audit.jsonl.segment-00000000000000000001.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(archive)
        .unwrap()
        .write_all(b"{")
        .unwrap();
    assert_eq!(Audit::open(&path).err().unwrap().code, "AUDIT_CORRUPT");
}

#[test]
fn relative_audit_path_reopens_its_archived_segments() {
    let relative = PathBuf::from("target").join(format!(
        "audit-relative-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::SeqCst)
    ));
    let directory = std::env::current_dir().unwrap().join(&relative);
    std::fs::create_dir_all(&directory).unwrap();
    let _cleanup = Temp(directory);
    let path = relative.join("audit.jsonl");
    {
        let mut audit = Audit::open_with_segment_limit(&path, 1).unwrap();
        for n in 0..2 {
            audit
                .intent(&ActionRequest {
                    session_id: "s".into(),
                    frame_id: "f".into(),
                    action_id: format!("relative-{n}"),
                    action: Action::PressKey {
                        keys: vec!["Enter".into()],
                    },
                })
                .unwrap();
        }
    }
    let audit = Audit::open(&path).unwrap();
    assert_eq!(
        audit.get("relative-0").unwrap().execution_status,
        "uncertain"
    );
}

#[test]
fn after_image_failure_does_not_reclaim_foreground_or_repeat_the_action() {
    let mut f = Fixture::new();
    let request = f.click("post-capture-passive");
    // Lose foreground after the preflight image; the fake injector reports an
    // accepted action, but the after-image must remain passive and fail.
    f.state.lock().unwrap().lose_foreground_after_capture = true;
    let receipt = f.service.perform(request.clone()).unwrap();
    assert_eq!(receipt.execution_status, "injected");
    assert_eq!(receipt.error.unwrap().code, "POST_CAPTURE_FAILED");
    assert!(receipt.next_frame_id.is_none());
    assert_eq!(f.state.lock().unwrap().focus_attempts, 0);
    assert_eq!(f.state.lock().unwrap().injections, 1);
    assert!(f.service.frame.is_none());
    f.service.perform(request).unwrap();
    assert_eq!(f.state.lock().unwrap().injections, 1);
}

#[test]
fn observation_recovers_foreground_and_requires_a_new_content_click_for_typing() {
    let mut f = Fixture::new();
    let click = f.click("content-click");
    f.service.perform(click).unwrap();
    f.state.lock().unwrap().foreground = false;
    let frame = f.service.observe().unwrap();
    assert_eq!(f.state.lock().unwrap().focus_attempts, 1);
    assert!(f.state.lock().unwrap().foreground);
    let request = ActionRequest {
        session_id: frame.session_id,
        frame_id: frame.frame_id,
        action_id: "text-after-recovery".into(),
        action: Action::TypeText {
            text: "ls".into(),
            sensitive: false,
            input_mode: TextInputMode::ScanCode,
        },
    };
    assert_eq!(
        f.service.perform(request).unwrap_err().code,
        "CONTENT_FOCUS_REQUIRED"
    );
    assert_eq!(f.state.lock().unwrap().injections, 1);
}

#[test]
fn paused_observation_does_not_reclaim_foreground_but_can_capture_the_active_target() {
    let mut f = Fixture::new();
    f.service.control.pause_user();
    assert!(f.service.observe().is_ok());
    f.state.lock().unwrap().foreground = false;
    assert_eq!(f.service.observe().unwrap_err().code, "PAUSED");
    assert_eq!(f.state.lock().unwrap().focus_attempts, 0);
    assert!(!f.state.lock().unwrap().foreground);
}

#[test]
fn lost_foreground_during_input_preflight_does_not_activate_or_inject() {
    let mut f = Fixture::new();
    let request = f.click("do-not-reclaim-mid-input");
    f.state.lock().unwrap().foreground = false;
    assert_eq!(
        f.service.perform(request).unwrap_err().code,
        "FOREGROUND_REQUIRED"
    );
    assert_eq!(f.state.lock().unwrap().focus_attempts, 0);
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn composite_fill_and_key_at_recover_focus_and_deduplicate_as_single_actions() {
    let mut f = Fixture::new();
    let frame = f.service.observe().unwrap();
    f.state.lock().unwrap().foreground = false;
    let args = json!({"session_id":frame.session_id,"frame_id":frame.frame_id,
        "action_id":"fill-command","x":10,"y":10,"text":"hostname -I","settle_ms":0});
    let (fill, image) = mcp::call(&mut f.service, "desktop_fill", args.clone()).unwrap();
    assert!(image);
    assert_eq!(fill["execution_status"], "injected");
    assert_eq!(fill["text_input_mode"], "scan_code");
    mcp::call(&mut f.service, "desktop_fill", args).unwrap();
    assert_eq!(f.state.lock().unwrap().focus_attempts, 1);
    assert_eq!(f.state.lock().unwrap().injections, 1);
    f.state.lock().unwrap().foreground = false;
    let key = json!({"session_id":frame.session_id,"frame_id":fill["next_frame_id"],
        "action_id":"execute-command","x":10,"y":10,"keys":["Enter"],"settle_ms":0});
    let (receipt, _) = mcp::call(&mut f.service, "desktop_key_at", key.clone()).unwrap();
    assert_eq!(receipt["execution_status"], "injected");
    mcp::call(&mut f.service, "desktop_key_at", key).unwrap();
    assert_eq!(f.state.lock().unwrap().focus_attempts, 2);
    assert_eq!(f.state.lock().unwrap().injections, 2);
    let log = std::fs::read_to_string(f._temp.0.join("audit.jsonl")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["event"] == "intent")
            .count(),
        2
    );
}

#[test]
fn composite_actions_reject_stale_scenes_and_control_text_before_injection() {
    let mut f = Fixture::new();
    let frame = f.service.observe().unwrap();
    let args = json!({"session_id":frame.session_id,"frame_id":frame.frame_id,"action_id":"invalid",
        "x":10,"y":10,"text":"hostname -I\n","settle_ms":0});
    assert!(mcp::call(&mut f.service, "desktop_fill", args).is_err());
    assert_eq!(f.state.lock().unwrap().injections, 0);
    assert_eq!(f.state.lock().unwrap().focus_attempts, 0);
    let frame = f.service.observe().unwrap();
    let args = json!({"session_id":frame.session_id,"frame_id":frame.frame_id,"action_id":"stale",
        "x":10,"y":10,"keys":["Enter"],"settle_ms":0});
    f.state.lock().unwrap().image.pixels.fill(0);
    assert_eq!(
        mcp::call(&mut f.service, "desktop_key_at", args)
            .unwrap_err()
            .code,
        "STALE_FRAME"
    );
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn partial_composite_input_pauses_and_sensitive_fill_is_redacted() {
    let mut f = Fixture::new();
    let frame = f.service.observe().unwrap();
    let args = json!({"session_id":frame.session_id,"frame_id":frame.frame_id,"action_id":"partial-fill",
        "x":10,"y":10,"text":"COMPOSITE_SECRET","sensitive":true,"settle_ms":0});
    f.state.lock().unwrap().failure = Some(
        DesktopError::new("INPUT_PARTIAL", "test click accepted before text failure").with_sent(4),
    );
    let (receipt, image) = mcp::call(&mut f.service, "desktop_fill", args.clone()).unwrap();
    assert!(!image);
    assert_eq!(receipt["execution_status"], "uncertain");
    assert_eq!(receipt["sent_events"], 4);
    assert_eq!(f.service.control.reason(), "agent");
    mcp::call(&mut f.service, "desktop_fill", args).unwrap();
    assert_eq!(f.state.lock().unwrap().focus_attempts, 1);
    assert!(!std::fs::read_to_string(f._temp.0.join("audit.jsonl"))
        .unwrap()
        .contains("COMPOSITE_SECRET"));
}

#[test]
fn changing_a_bound_surface_mapping_rejects_old_frame_input() {
    let mut f = Fixture::new();
    let request = f.click("old-surface");
    let mut state = f.state.lock().unwrap();
    let mut replacement = state.snapshot.identity.clone();
    replacement.id = WindowId("test:replacement".into());
    state.snapshot.input_surface = Some(replacement);
    drop(state);
    assert_eq!(
        f.service.perform(request).unwrap_err().code,
        "GEOMETRY_CHANGED"
    );
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn focus_wait_can_be_paused_after_activation_but_before_a_successful_capture() {
    let mut f = Fixture::new();
    f.state.lock().unwrap().capture_foreground_failures = u32::MAX;
    let control = f.service.control.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(75));
        control.pause_user();
    });
    assert_eq!(
        f.service.focus_with_timeout(60_000).unwrap_err().code,
        "PAUSED"
    );
    worker.join().unwrap();
    assert!(f.service.frame.is_none());
}

#[test]
fn focus_wait_retries_capture_failures_with_one_activation_batch() {
    let mut f = Fixture::new();
    f.service.observe().unwrap();
    let batches = f.state.lock().unwrap().activation_batches;
    f.state.lock().unwrap().capture_foreground_failures = 2;
    let frame = f.service.focus_with_timeout(1000).unwrap();
    assert!(frame.image_path.is_file());
    let state = f.state.lock().unwrap();
    assert_eq!(state.focus_attempts, 3);
    assert_eq!(state.activation_batches, batches + 1);
    drop(state);
    f.service.focus().unwrap();
    assert_eq!(f.state.lock().unwrap().activation_batches, batches + 2);
}

#[test]
fn focus_capture_failure_obeys_timeout_and_keeps_old_frames_invalid() {
    let mut f = Fixture::new();
    f.service.observe().unwrap();
    f.state.lock().unwrap().capture_foreground_failures = u32::MAX;
    let start = std::time::Instant::now();
    assert_eq!(
        f.service.focus_with_timeout(100).unwrap_err().code,
        "FOREGROUND_REQUIRED"
    );
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(f.service.frame.is_none());
    let attempts = f.state.lock().unwrap().focus_attempts;
    assert!(attempts >= 2);
    assert_eq!(f.service.focus().unwrap_err().code, "FOREGROUND_REQUIRED");
    assert_eq!(f.state.lock().unwrap().focus_attempts, attempts + 1);
}

#[test]
fn focus_wait_recovers_after_refusal_and_returns_a_new_image_file() {
    let mut f = Fixture::new();
    let old = f.service.observe().unwrap();
    f.state.lock().unwrap().focus_failures = 1;
    let frame = f.service.focus_with_timeout(1000).unwrap();
    assert_ne!(frame.frame_id, old.frame_id);
    assert_eq!(f.state.lock().unwrap().focus_attempts, 2);
    assert!(frame.image_path.is_absolute());
    assert_eq!(
        std::fs::read(&frame.image_path).unwrap(),
        f.service.frame.as_ref().unwrap().png
    );
}

#[test]
fn focus_refusal_discards_old_frame_and_wait_can_be_cancelled() {
    let mut f = Fixture::new();
    f.service.observe().unwrap();
    f.state.lock().unwrap().focus_failures = u32::MAX;
    assert_eq!(f.service.focus().unwrap_err().code, "FOREGROUND_REQUIRED");
    assert!(f.service.frame.is_none());
    assert_eq!(f.state.lock().unwrap().focus_attempts, 1);
    let control = f.service.control.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(75));
        control.pause_user();
    });
    let start = std::time::Instant::now();
    assert_eq!(
        f.service.focus_with_timeout(60_000).unwrap_err().code,
        "PAUSED"
    );
    worker.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(f.service.focus_with_timeout(60_001).is_err());
}

#[test]
fn text_mode_preserves_legacy_audit_fingerprints_and_detects_mode_conflicts() {
    let legacy = json!({"session_id":"s","frame_id":"f","action_id":"text",
        "action":{"kind":"type_text","text":"ls","sensitive":false}});
    let request: ActionRequest = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), legacy);
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    {
        let mut audit = Audit::open(&path).unwrap();
        audit.intent(&request).unwrap();
    }
    let audit = Audit::open(&path).unwrap();
    assert!(audit.lookup(&request).unwrap().is_some());
    let mut scan = legacy;
    scan["action"]["input_mode"] = json!("scan_code");
    let scan: ActionRequest = serde_json::from_value(scan).unwrap();
    assert_eq!(audit.lookup(&scan).unwrap_err().code, "ACTION_ID_CONFLICT");
}

#[test]
fn mcp_default_text_mode_uses_scancodes_for_ascii_and_reports_the_selected_channel() {
    for (text, mode, expected) in [
        ("dir /b", None, TextInputMode::ScanCode),
        (
            "echo %USERNAME% & cd",
            Some("auto"),
            TextInputMode::ScanCode,
        ),
        ("中文", None, TextInputMode::Unicode),
        ("echo 中文", None, TextInputMode::Unicode),
        ("ls\n", None, TextInputMode::Unicode),
        ("dir", Some("unicode"), TextInputMode::Unicode),
        ("dir", Some("scan_code"), TextInputMode::ScanCode),
    ] {
        let mut f = Fixture::new();
        assert_eq!(
            f.service.status()["server"]["default_text_input_mode"],
            "auto"
        );
        let click = f.click("content");
        let receipt = f.service.perform(click).unwrap();
        let mut args = json!({"session_id":f.service.status()["session_id"],
            "frame_id":receipt.next_frame_id.unwrap(),"action_id":"type-command","text":text});
        if let Some(mode) = mode {
            args["input_mode"] = json!(mode);
        }
        let (data, _) = mcp::call(&mut f.service, "desktop_type_text", args.clone()).unwrap();
        assert_eq!(f.state.lock().unwrap().last_text_input_mode, Some(expected));
        assert_eq!(
            data["text_input_mode"],
            serde_json::to_value(expected).unwrap()
        );
        let audit = Audit::open(&f._temp.0.join("audit.jsonl")).unwrap();
        assert_eq!(
            audit.get("type-command").unwrap().text_input_mode,
            Some(expected)
        );
        mcp::call(&mut f.service, "desktop_type_text", args).unwrap();
        assert_eq!(f.state.lock().unwrap().injections, 2);
    }
    let definition = mcp::tools()
        .into_iter()
        .find(|tool| tool["name"] == "desktop_type_text")
        .unwrap();
    assert_eq!(
        definition["inputSchema"]["properties"]["input_mode"]["default"],
        "auto"
    );
}

#[test]
fn old_action_receipts_without_text_mode_remain_readable() {
    let legacy = json!({"action_id":"old","execution_status":"injected",
        "verification_status":"unverified","sent_events":12,"next_frame_id":null,"error":null});
    let receipt: ActionReceipt = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(receipt.text_input_mode, None);
    assert_eq!(serde_json::to_value(receipt).unwrap(), legacy);
}

#[test]
fn mcp_routes_focus_timeout_and_scan_code_mode_and_rejects_unknown_modes() {
    let mut f = Fixture::new();
    let session = f.service.status()["session_id"].clone();
    f.state.lock().unwrap().focus_failures = 1;
    let (data, image) = mcp::call(
        &mut f.service,
        "desktop_focus",
        json!({"session_id":session,"timeout_ms":1000}),
    )
    .unwrap();
    assert!(image);
    assert!(data["frame"]["image_path"].is_string());
    assert_eq!(f.state.lock().unwrap().focus_attempts, 2);
    let click = f.click("focus-content");
    let receipt = f.service.perform(click).unwrap();
    let args = json!({"session_id":session,"frame_id":receipt.next_frame_id.unwrap(),
        "action_id":"scan-text","text":"ls","input_mode":"scan_code"});
    let (receipt, image) = mcp::call(&mut f.service, "desktop_type_text", args.clone()).unwrap();
    assert!(image);
    assert_eq!(receipt["execution_status"], "injected");
    let records = std::fs::read_to_string(f._temp.0.join("audit.jsonl")).unwrap();
    let intent: Value = records
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["action_id"] == "scan-text")
        .unwrap();
    assert_eq!(intent["request"]["action"]["input_mode"], "scan_code");
    let mut invalid = args;
    invalid["input_mode"] = json!("clipboard");
    assert!(mcp::call(&mut f.service, "desktop_type_text", invalid).is_err());
    assert!(mcp::call(
        &mut f.service,
        "desktop_focus",
        json!({"session_id":session,"timeout_ms":60001})
    )
    .is_err());
}

#[test]
fn failed_observation_discards_the_previous_frame() {
    let mut f = Fixture::new();
    let request = f.click("old");
    f.state.lock().unwrap().capture_failure = true;
    assert!(f.service.observe().is_err());
    assert!(f.service.frame.is_none());
    f.state.lock().unwrap().capture_failure = false;
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn wait_detects_a_change_that_happened_since_the_last_observation() {
    let mut f = Fixture::new();
    f.service.observe().unwrap();
    f.state.lock().unwrap().image.pixels.fill(0);
    let result = f.service.wait("changed", 0).unwrap();
    assert_eq!(result["condition_met"], true);
    assert_eq!(result["timed_out"], false);
}

#[test]
fn image_validation_rejects_overflow_dimensions_without_panicking() {
    let image = RgbImage {
        width: u32::MAX,
        height: u32::MAX,
        pixels: vec![],
    };
    assert!(image.validate().is_err());
}

#[test]
fn partial_injection_pauses_further_actions() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.state.lock().unwrap().failure = Some(DesktopError::new("INPUT_PARTIAL", "test").with_sent(1));
    assert_eq!(
        f.service.perform(request).unwrap().execution_status,
        "uncertain"
    );
    assert_eq!(f.service.control.reason(), "agent");
}

#[test]
fn lost_native_reply_is_uncertain_even_without_a_confirmed_event_count() {
    let mut f = Fixture::new();
    let request = f.click("lost-reply");
    f.state.lock().unwrap().failure = Some(DesktopError::new(
        "INPUT_OUTCOME_UNKNOWN",
        "native process stopped before acknowledging input",
    ));
    let receipt = f.service.perform(request.clone()).unwrap();
    assert_eq!(receipt.execution_status, "uncertain");
    assert_eq!(receipt.sent_events, 0);
    assert_eq!(f.service.control.reason(), "agent");
    assert!(f.service.frame.is_none());
    f.state.lock().unwrap().failure = None;
    assert_eq!(
        f.service.perform(request).unwrap().execution_status,
        "uncertain"
    );
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn audit_cannot_append_an_outcome_without_intent() {
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    let mut audit = Audit::open(&path).unwrap();
    let receipt = ActionReceipt {
        action_id: "missing".into(),
        execution_status: "injected".into(),
        verification_status: "unverified".into(),
        text_input_mode: None,
        sent_events: 1,
        next_frame_id: None,
        error: None,
    };
    assert!(audit.outcome(&receipt).is_err());
    drop(audit);
    assert!(Audit::open(&path).is_ok());
}

#[test]
fn failed_input_preflight_discards_the_previous_frame() {
    let mut f = Fixture::new();
    let request = f.click("old");
    f.state.lock().unwrap().capture_failure = true;
    assert_eq!(
        f.service.perform(request.clone()).unwrap_err().code,
        "CAPTURE_UNAVAILABLE"
    );
    assert!(f.service.frame.is_none());
    f.state.lock().unwrap().capture_failure = false;
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}

#[test]
fn duplicate_audit_intents_are_not_allowed_to_replace_prior_state() {
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    let request = ActionRequest {
        session_id: "s".into(),
        frame_id: "f".into(),
        action_id: "a".into(),
        action: Action::TypeText {
            text: "hello".into(),
            sensitive: false,
            input_mode: TextInputMode::Unicode,
        },
    };
    let mut audit = Audit::open(&path).unwrap();
    audit.intent(&request).unwrap();
    assert_eq!(
        audit.intent(&request).unwrap_err().code,
        "ACTION_ID_CONFLICT"
    );
    drop(audit);
    let line = std::fs::read(&path).unwrap();
    let mut duplicates = line.clone();
    duplicates.extend(line);
    std::fs::write(&path, duplicates).unwrap();
    assert_eq!(Audit::open(&path).err().unwrap().code, "AUDIT_CORRUPT");
}

#[test]
fn screenshot_coordinates_map_to_negative_monitor_and_viewport() {
    let t = ImageTransform {
        image_width: 1280,
        image_height: 720,
        content_screen: Rect {
            x: -1700,
            y: 140,
            width: 1280,
            height: 720,
        },
    };
    assert_eq!(
        t.map(Point {
            x: 640.,
            y: 360.,
            coordinate_space: CoordinateSpace::ImagePixels
        })
        .unwrap(),
        ScreenPoint { x: -1060, y: 500 }
    );
    let end = t
        .map(Point {
            x: 1.,
            y: 1.,
            coordinate_space: CoordinateSpace::Normalized,
        })
        .unwrap();
    assert_eq!(end, ScreenPoint { x: -421, y: 859 });
    let desktop = Rect {
        x: -1920,
        y: 0,
        width: 3840,
        height: 1080,
    };
    assert_eq!(
        absolute_mouse(ScreenPoint { x: -1920, y: 0 }, desktop).unwrap(),
        (0, 0)
    );
    assert_eq!(
        absolute_mouse(ScreenPoint { x: 1919, y: 1079 }, desktop).unwrap(),
        (65535, 65535)
    );
}
#[test]
fn scaling_keeps_every_legal_point_inside_content() {
    for (iw, cw) in [(1, 1), (100, 125), (100, 150), (100, 200), (200, 100)] {
        let t = ImageTransform {
            image_width: iw,
            image_height: iw,
            content_screen: Rect {
                x: -600,
                y: 100,
                width: cw,
                height: cw,
            },
        };
        for x in 0..iw {
            let p = t
                .map(Point {
                    x: x as f64,
                    y: x as f64,
                    coordinate_space: CoordinateSpace::ImagePixels,
                })
                .unwrap();
            assert!(t.content_screen.contains(p.x, p.y));
        }
    }
}
#[test]
fn invalid_points_and_viewports_never_clamp_into_actions() {
    let t = ImageTransform {
        image_width: 10,
        image_height: 10,
        content_screen: Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        },
    };
    for x in [-1., 10., f64::NAN, f64::INFINITY] {
        assert!(t
            .map(Point {
                x,
                y: 0.,
                coordinate_space: CoordinateSpace::ImagePixels
            })
            .is_err());
    }
    assert!(t
        .content_screen
        .crop(Rect {
            x: 9,
            y: 0,
            width: 2,
            height: 1
        })
        .is_err());
}
#[test]
fn png_has_valid_chunks_and_decodable_rgb_scanlines() {
    use std::io::Read;
    let image = RgbImage {
        width: 2,
        height: 1,
        pixels: vec![255, 0, 0, 0, 128, 255],
    };
    let png = image.png().unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let mut at = 8;
    let mut idat = Vec::new();
    while at < png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let name = &png[at + 4..at + 8];
        let data = &png[at + 8..at + 8 + len];
        let expected = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
        assert_eq!(crc32fast::hash(&png[at + 4..at + 8 + len]), expected);
        if name == b"IDAT" {
            idat.extend_from_slice(data);
        }
        at += 12 + len;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&idat[..])
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(raw, vec![0, 255, 0, 0, 0, 128, 255]);
}
#[test]
fn duplicate_action_is_not_reinjected_even_after_frame_changes() {
    let mut f = Fixture::new();
    let request = f.click("one");
    let first = f.service.perform(request.clone()).unwrap();
    assert_eq!(first.execution_status, "injected");
    f.service.observe().unwrap();
    let second = f.service.perform(request).unwrap();
    assert_eq!(first.next_frame_id, second.next_frame_id);
    assert_eq!(f.state.lock().unwrap().injections, 1);
}
#[test]
fn action_id_conflict_is_rejected() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.service.perform(request.clone()).unwrap();
    let mut other = request;
    other.action = Action::Move {
        point: Point {
            x: 1.,
            y: 1.,
            coordinate_space: CoordinateSpace::ImagePixels,
        },
    };
    assert_eq!(
        f.service.perform(other).unwrap_err().code,
        "ACTION_ID_CONFLICT"
    );
    assert_eq!(f.state.lock().unwrap().injections, 1);
}
#[test]
fn window_move_or_reused_handle_blocks_input() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.state.lock().unwrap().snapshot.client_screen.x += 1;
    assert_eq!(
        f.service.perform(request).unwrap_err().code,
        "GEOMETRY_CHANGED"
    );
    f.state.lock().unwrap().snapshot.identity.process_started += 1;
    assert_eq!(f.service.observe().unwrap_err().code, "WINDOW_LOST");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}
#[test]
fn changed_scene_and_expired_frames_block_input() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.state.lock().unwrap().image.pixels.fill(0);
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    let request = f.click("two");
    f.service.frame_ttl = Duration::ZERO;
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}
#[test]
fn manual_pause_cannot_be_resumed_by_agent_and_invalidates_old_frames() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.service.control.pause_user();
    assert_eq!(
        f.service.control.resume_agent().unwrap_err().code,
        "PAUSED_BY_USER"
    );
    assert_eq!(
        f.service.perform(request.clone()).unwrap_err().code,
        "PAUSED"
    );
    f.service.control.resume_user();
    assert_eq!(f.service.perform(request).unwrap_err().code, "STALE_FRAME");
    assert_eq!(f.state.lock().unwrap().injections, 0);
}
#[test]
fn resumed_keyboard_requires_a_new_remote_content_click() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.service.perform(request).unwrap();
    f.service.control.pause_user();
    f.service.control.resume_user();
    let frame = f.service.observe().unwrap();
    let request = ActionRequest {
        session_id: frame.session_id,
        frame_id: frame.frame_id,
        action_id: "text".into(),
        action: Action::TypeText {
            text: "hello".into(),
            sensitive: false,
            input_mode: TextInputMode::Unicode,
        },
    };
    assert_eq!(
        f.service.perform(request).unwrap_err().code,
        "CONTENT_FOCUS_REQUIRED"
    );
}
#[test]
fn partial_native_result_stays_uncertain_and_is_never_retried() {
    let mut f = Fixture::new();
    let request = f.click("one");
    f.state.lock().unwrap().failure = Some(DesktopError::new("INPUT_PARTIAL", "test").with_sent(1));
    let result = f.service.perform(request.clone()).unwrap();
    assert_eq!(result.execution_status, "uncertain");
    assert_eq!(result.sent_events, 1);
    assert!(f.service.frame.is_none());
    assert_eq!(
        f.service.perform(request).unwrap().execution_status,
        "uncertain"
    );
}
#[test]
fn audit_recovers_unfinished_intent_without_replaying_and_redacts_text() {
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    let request = ActionRequest {
        session_id: "s".into(),
        frame_id: "f".into(),
        action_id: "secret".into(),
        action: Action::TypeText {
            text: "TOP_SECRET_TEXT".into(),
            sensitive: true,
            input_mode: TextInputMode::Unicode,
        },
    };
    {
        let mut audit = Audit::open(&path).unwrap();
        audit.intent(&request).unwrap();
    }
    let audit = Audit::open(&path).unwrap();
    assert_eq!(
        audit.lookup(&request).unwrap().unwrap().execution_status,
        "uncertain"
    );
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("TOP_SECRET_TEXT"));
}
#[test]
fn audit_outcomes_survive_restart_and_corrupt_tails_are_rejected() {
    let mut f = Fixture::new();
    let request = f.click("one");
    let receipt = f.service.perform(request.clone()).unwrap();
    let path = f._temp.0.join("audit.jsonl");
    let audit = Audit::open(&path).unwrap();
    assert_eq!(
        audit.lookup(&request).unwrap().unwrap().execution_status,
        receipt.execution_status
    );
    let temp = Temp::new();
    let bad = temp.0.join("bad.jsonl");
    std::fs::write(&bad, b"{\"event\":").unwrap();
    assert_eq!(Audit::open(&bad).err().unwrap().code, "AUDIT_CORRUPT");
}
#[test]
fn cancellation_invalidates_existing_tokens_and_disconnect_stops_work() {
    let control = Arc::new(Control::default());
    let token = control.token(false);
    control.cancel_current();
    assert_eq!(token.check().unwrap_err().code, "CANCELLED");
    let token = control.token(true);
    control.shutdown();
    assert_eq!(token.check().unwrap_err().code, "SHUTTING_DOWN");
}
#[test]
fn mcp_negotiation_tools_and_inline_images_work_without_vendor_code() {
    let mut f = Fixture::new();
    let mut rpc = mcp::RpcSession::default();
    let early = rpc
        .dispatch(
            &mut f.service,
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
        )
        .unwrap();
    assert_eq!(early["error"]["code"], -32002);
    let init=rpc.dispatch(&mut f.service,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"test","version":"1"},"capabilities":{}}})).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert!(rpc
        .dispatch(
            &mut f.service,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
    let list = rpc
        .dispatch(
            &mut f.service,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
    let advertised = list["result"]["tools"].as_array().unwrap();
    assert!(advertised
        .iter()
        .any(|tool| tool["name"] == "desktop_observe"));
    assert!(advertised
        .iter()
        .any(|tool| tool["name"] == "desktop_click"));
    let session = f.service.status()["session_id"].clone();
    let image=rpc.dispatch(&mut f.service,json!({"jsonrpc":"2.0","id":"image","method":"tools/call","params":{"name":"desktop_observe","arguments":{"session_id":session}}})).unwrap();
    assert_eq!(image["result"]["content"][1]["mimeType"], "image/png");
    assert!(image["result"]["structuredContent"]["frame"]["frame_id"].is_string());
}
#[test]
fn tool_arguments_reject_unknown_fields_null_and_wrong_types() {
    let mut f = Fixture::new();
    for args in [
        json!({"window_id":"test:1","viewport":null}),
        json!({"window_id":1}),
        json!({"window_id":"test:1","extra":true}),
    ] {
        assert_eq!(
            mcp::call(&mut f.service, "desktop_attach", args)
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }
    let args =
        json!({"session_id":"s","frame_id":"f","action_id":"a","x":0,"y":0,"button":"invalid"});
    assert_eq!(
        mcp::call(&mut f.service, "desktop_click", args)
            .unwrap_err()
            .code,
        "INVALID_ARGUMENT"
    );
}
#[test]
fn malformed_rpc_and_unknown_tools_return_protocol_errors() {
    let mut f = Fixture::new();
    let mut rpc = mcp::RpcSession::default();
    for req in [
        json!([]),
        json!({"jsonrpc":"1.0","id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
    ] {
        assert_eq!(
            rpc.dispatch(&mut f.service, req).unwrap()["error"]["code"],
            -32600
        );
    }
}
#[test]
fn old_protocol_omits_new_structured_content_field() {
    let mut f = Fixture::new();
    let mut rpc = mcp::RpcSession::default();
    rpc.dispatch(&mut f.service,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{},"capabilities":{}}}));
    rpc.dispatch(
        &mut f.service,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    let result=rpc.dispatch(&mut f.service,json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"desktop_get_status"}})).unwrap();
    assert!(result["result"].get("structuredContent").is_none());
    assert_eq!(result["result"]["isError"], false);
}
#[test]
fn wait_is_bounded_and_does_not_claim_static_scene_changed() {
    let mut f = Fixture::new();
    let result = f.service.wait("changed", 1).unwrap();
    assert_eq!(result["timed_out"], true);
    assert_eq!(result["condition_met"], false);
    assert_eq!(
        f.service.wait("duration", 0).unwrap()["condition_met"],
        true
    );
}

#[test]
fn user_pause_interrupts_a_long_wait_without_waiting_for_timeout() {
    let mut f = Fixture::new();
    let control = f.service.control.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        control.pause_user();
    });
    let start = std::time::Instant::now();
    assert!(f.service.wait("changed", 60000).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    thread.join().unwrap();
}
#[test]
fn platform_types_remain_handle_free_serializable_data() {
    let f = Fixture::new();
    let value: Value =
        serde_json::to_value(f.state.lock().unwrap().snapshot.identity.clone()).unwrap();
    assert_eq!(value["id"], "test:1");
    assert!(value.get("hwnd").is_none());
}
