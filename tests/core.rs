use airemotecontrol::{
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
            "airc-test-{}-{}",
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
}
struct Fake(Arc<Mutex<State>>);
impl WindowBackend for Fake {
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
        Ok(())
    }
}
impl CaptureBackend for Fake {
    fn capture(&self, _: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage> {
        cancel.check()?;
        let state = self.0.lock().unwrap();
        if state.capture_failure {
            return Err(DesktopError::new(
                "CAPTURE_UNAVAILABLE",
                "test capture failure",
            ));
        }
        Ok(state.image.clone())
    }
}
impl InputBackend for Fake {
    fn inject(
        &self,
        _: &WindowSnapshot,
        _: &ResolvedAction,
        cancel: &Cancellation,
    ) -> Result<InjectionReport> {
        cancel.check()?;
        let mut state = self.0.lock().unwrap();
        if let Some(e) = state.failure.clone() {
            return Err(e);
        }
        state.injections += 1;
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
fn audit_cannot_append_an_outcome_without_intent() {
    let temp = Temp::new();
    let path = temp.0.join("audit.jsonl");
    let mut audit = Audit::open(&path).unwrap();
    let receipt = ActionReceipt {
        action_id: "missing".into(),
        execution_status: "injected".into(),
        verification_status: "unverified".into(),
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
