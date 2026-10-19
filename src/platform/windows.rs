//! Win32 implementation. Native pointers are confined to this module.
//! v0.1 deliberately uses visible screen-region capture, not unverified PrintWindow output.
use super::{CaptureBackend, InputBackend, WindowBackend};
use crate::{
    control::{Cancellation, Control},
    error::{DesktopError, Result},
    geometry::{absolute_mouse, Rect, ScreenPoint},
    image::RgbImage,
    types::*,
};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{RemoteDesktop::*, StationsAndDesktops::*, Threading::*},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const INPUT_MARKER: usize = 0x41495243;

struct Handle(HANDLE);
// Kernel handles are process-wide; this owner closes it exactly once.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                CloseHandle(self.0);
            }
        }
    }
}

pub struct WindowsPlatform {
    _hotkeys: Hotkeys,
    _lock: Handle,
    keyboard_anchor: AtomicUsize,
}
impl WindowsPlatform {
    pub fn new(control: Arc<Control>) -> Result<Self> {
        unsafe {
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0
                && AreDpiAwarenessContextsEqual(
                    GetThreadDpiAwarenessContext(),
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                ) == 0
            {
                return Err(native_error(
                    "DPI_UNAVAILABLE",
                    "Per-Monitor V2 awareness is required",
                ));
            }
            let name = wide("Local\\remote-window-bridge.Input.v1");
            let lock = CreateMutexW(null(), 0, name.as_ptr());
            if lock.is_null() {
                return Err(native_error(
                    "DESKTOP_LOCK_FAILED",
                    "Cannot create desktop input lock",
                ));
            }
            let exists = GetLastError() == ERROR_ALREADY_EXISTS;
            let lock = Handle(lock);
            if exists {
                return Err(DesktopError::new("LEASE_CONFLICT", "Another remote-window-bridge process owns this Windows session; stop it before starting another Agent"));
            }
            let hotkeys = Hotkeys::start(control)?;
            Ok(Self {
                _lock: lock,
                _hotkeys: hotkeys,
                keyboard_anchor: AtomicUsize::new(0),
            })
        }
    }
    fn check_keyboard_focus(&self, snapshot: &WindowSnapshot) -> Result<()> {
        let anchor = self.keyboard_anchor.load(Ordering::SeqCst);
        if anchor == 0 || focused_child(snapshot)? != anchor {
            return Err(DesktopError::new(
                "CONTENT_FOCUS_REQUIRED",
                "Keyboard focus changed since the last content click; click the input field again",
            ));
        }
        Ok(())
    }
}

impl WindowBackend for WindowsPlatform {
    fn permissions(&self) -> PermissionStatus {
        PermissionStatus { screen_capture: PermissionState::NotRequired, input_control: PermissionState::Unknown,
            global_hotkeys: PermissionState::Granted, notes: vec!["Input availability is checked at execution (interactive desktop, foreground, UIPI). No automatic elevation.".into()] }
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { platform: "windows".into(), window_capture: true,
            capture_backend: "visible_screen_bitblt".into(), requires_visible: true, input: true,
            ocr: false, global_pause_hotkey: true, notes: vec![
                "Select an already-connected client; client viewport must exclude local toolbars/letterboxing".into(),
                "Requires a foreground, non-minimized, unobstructed window on an unlocked desktop".into(),
                "Ctrl+Alt+F10 pauses; Ctrl+Alt+F11 resumes locally. Same-window physical-input takeover detection is not implemented".into(),
                "Single process owns input for this Windows session; shared Broker/IPC is a future extension".into(),
            ] }
    }
    fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
            if IsWindowVisible(hwnd) == 0 || GetWindowTextLengthW(hwnd) == 0 {
                return 1;
            }
            if let Ok(info) = info(hwnd) {
                (*(data as *mut Vec<WindowInfo>)).push(info);
            }
            1
        }
        let mut windows = Vec::new();
        unsafe {
            if EnumWindows(Some(collect), &mut windows as *mut _ as LPARAM) == 0 {
                return Err(native_error("ENUMERATION_FAILED", "EnumWindows"));
            }
        }
        Ok(windows)
    }
    fn inspect(&self, id: &WindowId, viewport: Option<Rect>) -> Result<WindowSnapshot> {
        unsafe {
            let hwnd = hwnd(id)?;
            if IsWindow(hwnd) == 0 {
                return Err(DesktopError::new("WINDOW_LOST", "Window no longer exists"));
            }
            if IsWindowVisible(hwnd) == 0 {
                return Err(DesktopError::new(
                    "WINDOW_HIDDEN",
                    "Target window is hidden",
                ));
            }
            if IsIconic(hwnd) != 0 {
                return Err(DesktopError::new(
                    "WINDOW_MINIMIZED",
                    "Restore the remote desktop window",
                ));
            }
            let info = info(hwnd)?;
            let mut rect: RECT = zeroed();
            let mut origin = POINT { x: 0, y: 0 };
            if GetClientRect(hwnd, &mut rect) == 0 || ClientToScreen(hwnd, &mut origin) == 0 {
                return Err(native_error(
                    "GEOMETRY_UNAVAILABLE",
                    "Cannot read physical client rectangle",
                ));
            }
            if rect.right <= rect.left || rect.bottom <= rect.top {
                return Err(DesktopError::new(
                    "INVALID_GEOMETRY",
                    "Empty client rectangle",
                ));
            }
            let client = Rect {
                x: origin.x,
                y: origin.y,
                width: (rect.right - rect.left) as u32,
                height: (rect.bottom - rect.top) as u32,
            }
            .validate()?;
            let content = match viewport {
                Some(v) => client.crop(v)?,
                None => client,
            };
            Ok(WindowSnapshot {
                identity: info.identity,
                client_screen: client,
                content_screen: content,
                dpi: GetDpiForWindow(hwnd),
            })
        }
    }
    fn focus(&self, identity: &WindowIdentity, cancel: &Cancellation) -> Result<()> {
        cancel.check()?;
        interactive_desktop()?;
        self.keyboard_anchor.store(0, Ordering::SeqCst);
        unsafe {
            let hwnd = hwnd(&identity.id)?;
            if info(hwnd)?.identity != *identity {
                return Err(DesktopError::new("WINDOW_LOST", "Window identity changed"));
            }
            cancel.check()?;
            if SetForegroundWindow(hwnd) == 0 {
                return Err(DesktopError::new(
                    "FOREGROUND_REQUIRED",
                    "Windows refused activation; select the remote window manually",
                ));
            }
            // Activation can be asynchronous. Do not use an unchecked click to force focus.
            for _ in 0..10 {
                cancel.check()?;
                if GetForegroundWindow() == hwnd {
                    return Ok(());
                }
                thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(DesktopError::new(
                "FOREGROUND_REQUIRED",
                "Remote window did not become foreground",
            ))
        }
    }
}

impl CaptureBackend for WindowsPlatform {
    fn capture(&self, snapshot: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage> {
        cancel.check()?;
        ready(snapshot)?;
        unsafe {
            let area = snapshot.content_screen;
            let len = area.width as u64 * area.height as u64 * 4;
            if len > 64 * 1024 * 1024 {
                return Err(DesktopError::new(
                    "FRAME_TOO_LARGE",
                    "Calibrate a viewport smaller than 16 million pixels",
                ));
            }
            let mut resources = CaptureResources {
                screen: GetDC(null_mut()),
                memory: null_mut(),
                bitmap: null_mut(),
                previous: null_mut(),
            };
            if resources.screen.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "GetDC"));
            }
            resources.memory = CreateCompatibleDC(resources.screen);
            if resources.memory.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "CreateCompatibleDC"));
            }
            let mut bmi: BITMAPINFO = zeroed();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = area.width as i32;
            bmi.bmiHeader.biHeight = -(area.height as i32); // top-down BGRA
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut pixels = null_mut();
            resources.bitmap = CreateDIBSection(
                resources.screen,
                &bmi,
                DIB_RGB_COLORS,
                &mut pixels,
                null_mut(),
                0,
            );
            if resources.bitmap.is_null() || pixels.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "CreateDIBSection"));
            }
            resources.previous = SelectObject(resources.memory, resources.bitmap);
            if resources.previous.is_null() || resources.previous as isize == -1 {
                return Err(native_error("CAPTURE_UNAVAILABLE", "SelectObject"));
            }
            if BitBlt(
                resources.memory,
                0,
                0,
                area.width as i32,
                area.height as i32,
                resources.screen,
                area.x,
                area.y,
                SRCCOPY | CAPTUREBLT,
            ) == 0
            {
                return Err(native_error("CAPTURE_UNAVAILABLE", "BitBlt"));
            }
            if GdiFlush() == 0 {
                return Err(native_error("CAPTURE_UNAVAILABLE", "GdiFlush"));
            }
            let raw = std::slice::from_raw_parts(pixels as *const u8, len as usize);
            let mut rgb = Vec::with_capacity(area.width as usize * area.height as usize * 3);
            for pixel in raw.chunks_exact(4) {
                rgb.extend([pixel[2], pixel[1], pixel[0]]);
            }
            cancel.check()?;
            ready(snapshot)?;
            Ok(RgbImage {
                width: area.width,
                height: area.height,
                pixels: rgb,
            })
        }
    }
}

struct CaptureResources {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for CaptureResources {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() && self.previous as isize != -1 && !self.memory.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}

impl InputBackend for WindowsPlatform {
    fn inject(
        &self,
        snapshot: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
    ) -> Result<InjectionReport> {
        let mut sent = 0;
        let result = (|| {
            cancel.check()?;
            ready(snapshot)?;
            modifiers_released()?;
            match action {
                ResolvedAction::Move { point } => move_to(snapshot, *point, cancel, &mut sent)?,
                ResolvedAction::Click {
                    point,
                    button,
                    count,
                } => {
                    move_to(snapshot, *point, cancel, &mut sent)?;
                    let (down, up) = match button {
                        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                    };
                    for _ in 0..*count {
                        cancel.check()?;
                        ready(snapshot)?;
                        hit(snapshot, *point)?;
                        send(
                            &[
                                absolute_event(*point)?,
                                mouse(0, 0, 0, down),
                                mouse(0, 0, 0, up),
                            ],
                            &[mouse(0, 0, 0, up)],
                            &mut sent,
                        )?;
                    }
                    self.keyboard_anchor
                        .store(focused_child(snapshot).unwrap_or(0), Ordering::SeqCst);
                }
                ResolvedAction::Scroll {
                    point,
                    ticks,
                    horizontal,
                } => {
                    move_to(snapshot, *point, cancel, &mut sent)?;
                    cancel.check()?;
                    ready(snapshot)?;
                    hit(snapshot, *point)?;
                    let flag = if *horizontal {
                        MOUSEEVENTF_HWHEEL
                    } else {
                        MOUSEEVENTF_WHEEL
                    };
                    send(
                        &[
                            absolute_event(*point)?,
                            mouse(0, 0, (*ticks * WHEEL_DELTA as i32) as u32, flag),
                        ],
                        &[],
                        &mut sent,
                    )?;
                }
                ResolvedAction::TypeText { text } => {
                    for ch in text.chars() {
                        cancel.check()?;
                        ready(snapshot)?;
                        modifiers_released()?;
                        self.check_keyboard_focus(snapshot)?;
                        let mut units = [0u16; 2];
                        let mut events = Vec::new();
                        let mut releases = Vec::new();
                        for unit in ch.encode_utf16(&mut units).iter() {
                            let up = keyboard(0, *unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
                            events.push(keyboard(0, *unit, KEYEVENTF_UNICODE));
                            events.push(up);
                            releases.push(up);
                        }
                        send(&events, &releases, &mut sent)?;
                    }
                }
                ResolvedAction::PressKey { keys } => {
                    let codes: Vec<u16> = keys
                        .iter()
                        .map(|key| key_code(key))
                        .collect::<Result<_>>()?;
                    let mut unique = codes.clone();
                    unique.sort();
                    unique.dedup();
                    if unique.len() != codes.len() {
                        return Err(DesktopError::invalid("Duplicate keys in chord"));
                    }
                    let releases: Vec<INPUT> = codes
                        .iter()
                        .rev()
                        .map(|vk| keyboard(*vk, 0, extended(*vk) | KEYEVENTF_KEYUP))
                        .collect();
                    let mut events: Vec<INPUT> = codes
                        .iter()
                        .map(|vk| keyboard(*vk, 0, extended(*vk)))
                        .collect();
                    events.extend(releases.iter().copied());
                    cancel.check()?;
                    ready(snapshot)?;
                    self.check_keyboard_focus(snapshot)?;
                    send(&events, &releases, &mut sent)?;
                }
            }
            Ok(InjectionReport { sent_events: sent })
        })();
        result.map_err(|error: DesktopError| error.with_sent(sent))
    }
}

fn focused_child(snapshot: &WindowSnapshot) -> Result<usize> {
    unsafe {
        let target = hwnd(&snapshot.identity.id)?;
        let thread_id = GetWindowThreadProcessId(target, null_mut());
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(thread_id, &mut info) == 0
            || info.hwndFocus.is_null()
            || GetAncestor(info.hwndFocus, GA_ROOT) != target
        {
            return Err(DesktopError::new(
                "CONTENT_FOCUS_REQUIRED",
                "Cannot verify keyboard focus inside the bound window",
            ));
        }
        Ok(info.hwndFocus as usize)
    }
}

fn move_to(
    snapshot: &WindowSnapshot,
    point: ScreenPoint,
    cancel: &Cancellation,
    sent: &mut u32,
) -> Result<()> {
    cancel.check()?;
    ready(snapshot)?;
    hit(snapshot, point)?;
    send(&[absolute_event(point)?], &[], sent)?;
    unsafe {
        let mut actual: POINT = zeroed();
        if GetCursorPos(&mut actual) == 0
            || actual.x.abs_diff(point.x) > 1
            || actual.y.abs_diff(point.y) > 1
        {
            return Err(DesktopError::new(
                "POINTER_MISMATCH",
                "Pointer did not reach the mapped point; no click was sent",
            ));
        }
    }
    cancel.check()?;
    hit(snapshot, point)
}

fn absolute_event(point: ScreenPoint) -> Result<INPUT> {
    let (x, y) = absolute_mouse(point, virtual_desktop()?)?;
    Ok(mouse(
        x,
        y,
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    ))
}
fn send(events: &[INPUT], releases: &[INPUT], total: &mut u32) -> Result<()> {
    unsafe {
        let count = SendInput(
            events.len() as u32,
            events.as_ptr(),
            size_of::<INPUT>() as i32,
        );
        *total += count;
        if count != events.len() as u32 {
            let error = native_error(
                "INPUT_PARTIAL",
                "SendInput rejected some/all events (UIPI or another desktop may be involved)",
            );
            if count > 0 && !releases.is_empty() {
                *total += SendInput(
                    releases.len() as u32,
                    releases.as_ptr(),
                    size_of::<INPUT>() as i32,
                );
            }
            return Err(error);
        }
        Ok(())
    }
}
fn mouse(dx: i32, dy: i32, data: u32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INPUT_MARKER,
            },
        },
    }
}
fn keyboard(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INPUT_MARKER,
            },
        },
    }
}
fn modifiers_released() -> Result<()> {
    unsafe {
        for vk in [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN] {
            if GetAsyncKeyState(vk as i32) < 0 {
                return Err(DesktopError::new(
                    "MODIFIER_HELD",
                    "Release physical modifiers before Agent input",
                ));
            }
        }
    }
    Ok(())
}
fn key_code(key: &str) -> Result<u16> {
    let key = key.to_ascii_uppercase();
    if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() {
        return Ok(key.as_bytes()[0] as u16);
    }
    if let Some(value) = key.strip_prefix('F').and_then(|s| s.parse::<u16>().ok()) {
        if (1..=24).contains(&value) {
            return Ok(VK_F1 + value - 1);
        }
    }
    if let Some(value) = key
        .strip_prefix("0X")
        .and_then(|s| u16::from_str_radix(s, 16).ok())
    {
        if (1..=254).contains(&value) {
            return Ok(value);
        }
    }
    Ok(match key.as_str() {
        "CTRL" | "CONTROL" => VK_CONTROL,
        "ALT" => VK_MENU,
        "SHIFT" => VK_SHIFT,
        "WIN" | "META" => VK_LWIN,
        "ENTER" | "RETURN" => VK_RETURN,
        "ESC" | "ESCAPE" => VK_ESCAPE,
        "TAB" => VK_TAB,
        "SPACE" => VK_SPACE,
        "BACKSPACE" => VK_BACK,
        "DELETE" => VK_DELETE,
        "INSERT" => VK_INSERT,
        "HOME" => VK_HOME,
        "END" => VK_END,
        "PAGEUP" => VK_PRIOR,
        "PAGEDOWN" => VK_NEXT,
        "LEFT" => VK_LEFT,
        "RIGHT" => VK_RIGHT,
        "UP" => VK_UP,
        "DOWN" => VK_DOWN,
        "CAPSLOCK" => VK_CAPITAL,
        "PRINTSCREEN" => VK_SNAPSHOT,
        "PAUSE" => VK_PAUSE,
        _ => {
            return Err(DesktopError::invalid(format!(
                "Unknown key {key}; use named key, A-Z, 0-9, F1-F24 or 0xNN virtual-key code"
            )))
        }
    })
}
fn extended(vk: u16) -> u32 {
    if [
        VK_INSERT,
        VK_DELETE,
        VK_HOME,
        VK_END,
        VK_PRIOR,
        VK_NEXT,
        VK_LEFT,
        VK_RIGHT,
        VK_UP,
        VK_DOWN,
        VK_LWIN,
        VK_RWIN,
        VK_RCONTROL,
        VK_RMENU,
        VK_DIVIDE,
    ]
    .contains(&vk)
    {
        KEYEVENTF_EXTENDEDKEY
    } else {
        0
    }
}

fn ready(snapshot: &WindowSnapshot) -> Result<()> {
    interactive_desktop()?;
    unsafe {
        let target = hwnd(&snapshot.identity.id)?;
        if info(target)?.identity != snapshot.identity {
            return Err(DesktopError::new("WINDOW_LOST", "Window identity changed"));
        }
        if IsIconic(target) != 0 || IsWindowVisible(target) == 0 {
            return Err(DesktopError::new("WINDOW_MINIMIZED", "Target not visible"));
        }
        if GetForegroundWindow() != target {
            return Err(DesktopError::new(
                "FOREGROUND_REQUIRED",
                "Remote window is not foreground; activate it then observe again",
            ));
        }
        let mut r: RECT = zeroed();
        let mut origin = POINT { x: 0, y: 0 };
        if GetClientRect(target, &mut r) == 0
            || ClientToScreen(target, &mut origin) == 0
            || origin.x != snapshot.client_screen.x
            || origin.y != snapshot.client_screen.y
            || r.right - r.left != snapshot.client_screen.width as i32
            || r.bottom - r.top != snapshot.client_screen.height as i32
            || GetDpiForWindow(target) != snapshot.dpi
        {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Window changed since observation",
            ));
        }
        let desktop = virtual_desktop()?;
        let content = snapshot.content_screen;
        if !desktop.contains(content.x, content.y)
            || !desktop.contains((content.right() - 1) as i32, (content.bottom() - 1) as i32)
        {
            return Err(DesktopError::new(
                "POINT_OCCLUDED",
                "Part of viewport is outside the desktop",
            ));
        }
        // Conservatively reject ANY visible top-level rectangle above the target intersecting the viewport.
        // This can reject transparent windows, but never deliberately captures an overlapping application.
        let mut above = GetWindow(target, GW_HWNDPREV);
        let mut visited = 0;
        while !above.is_null() {
            visited += 1;
            if visited > 10000 {
                return Err(DesktopError::new(
                    "POINT_OCCLUDED",
                    "Unstable window z-order",
                ));
            }
            let mut cloaked: u32 = 0;
            DwmGetWindowAttribute(
                above,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as _,
                size_of::<u32>() as u32,
            );
            if IsWindowVisible(above) != 0 && IsIconic(above) == 0 && cloaked == 0 {
                let mut rect: RECT = zeroed();
                if GetWindowRect(above, &mut rect) == 0 {
                    return Err(native_error(
                        "POINT_OCCLUDED",
                        "Cannot inspect an overlapping window; observe again",
                    ));
                }
                if rect.right > rect.left && rect.bottom > rect.top {
                    let area = Rect {
                        x: rect.left,
                        y: rect.top,
                        width: (rect.right - rect.left) as u32,
                        height: (rect.bottom - rect.top) as u32,
                    };
                    if area.intersects(content) {
                        return Err(DesktopError::new(
                            "POINT_OCCLUDED",
                            "Another local window overlaps the remote viewport",
                        ));
                    }
                }
            }
            above = GetWindow(above, GW_HWNDPREV);
        }
        Ok(())
    }
}

fn interactive_desktop() -> Result<()> {
    // Foreground HWND alone is insufficient: it can belong to our inactive desktop.
    unsafe {
        let mut session_id = 0;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) == 0 {
            return Err(native_error(
                "NO_INTERACTIVE_DESKTOP",
                "Cannot query current Windows session",
            ));
        }
        let mut buffer = null_mut();
        let mut bytes = 0;
        let queried = WTSQuerySessionInformationW(
            null_mut(),
            session_id,
            WTSConnectState,
            &mut buffer,
            &mut bytes,
        );
        let active = queried != 0
            && !buffer.is_null()
            && bytes >= size_of::<i32>() as u32
            && (buffer as *const i32).read_unaligned() == WTSActive;
        if !buffer.is_null() {
            WTSFreeMemory(buffer as _);
        }
        if !active {
            return Err(DesktopError::new(
                "NO_INTERACTIVE_DESKTOP",
                "Current Windows session is disconnected or unavailable",
            ));
        }
        let input = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if input.is_null() {
            return Err(native_error(
                "NO_INTERACTIVE_DESKTOP",
                "Input desktop is locked or inaccessible",
            ));
        }
        struct Desktop(HDESK);
        impl Drop for Desktop {
            fn drop(&mut self) {
                unsafe {
                    CloseDesktop(self.0);
                }
            }
        }
        let input = Desktop(input);
        let thread_desktop = GetThreadDesktop(GetCurrentThreadId());
        if thread_desktop.is_null() || desktop_name(input.0)? != desktop_name(thread_desktop)? {
            return Err(DesktopError::new(
                "NO_INTERACTIVE_DESKTOP",
                "Input is on another desktop (lock screen or secure prompt)",
            ));
        }
        Ok(())
    }
}
unsafe fn desktop_name(desktop: HDESK) -> Result<Vec<u16>> {
    let mut name = [0u16; 512];
    let mut bytes = 0;
    if GetUserObjectInformationW(
        desktop as _,
        UOI_NAME,
        name.as_mut_ptr() as _,
        size_of_val(&name) as u32,
        &mut bytes,
    ) == 0
        || bytes == 0
        || bytes as usize > size_of_val(&name)
        || bytes % 2 != 0
    {
        return Err(native_error(
            "NO_INTERACTIVE_DESKTOP",
            "Cannot identify input desktop",
        ));
    }
    Ok(name[..bytes as usize / 2].to_vec())
}
fn hit(snapshot: &WindowSnapshot, point: ScreenPoint) -> Result<()> {
    if !snapshot.content_screen.contains(point.x, point.y) {
        return Err(DesktopError::invalid("Point outside calibrated content"));
    }
    unsafe {
        let target = hwnd(&snapshot.identity.id)?;
        let actual = WindowFromPoint(POINT {
            x: point.x,
            y: point.y,
        });
        if actual.is_null() || GetAncestor(actual, GA_ROOT) != target {
            return Err(DesktopError::new(
                "POINT_OCCLUDED",
                "Point does not hit the bound remote desktop window",
            ));
        }
    }
    Ok(())
}
fn virtual_desktop() -> Result<Rect> {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32,
        }
        .validate()
    }
}
unsafe fn info(hwnd: HWND) -> Result<WindowInfo> {
    if IsWindow(hwnd) == 0 {
        return Err(DesktopError::new("WINDOW_LOST", "Window no longer exists"));
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let process = Handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid));
    if process.0.is_null() {
        return Err(native_error(
            "PROCESS_UNAVAILABLE",
            "Cannot inspect window process",
        ));
    }
    let mut creation: FILETIME = zeroed();
    let mut exit = zeroed();
    let mut kernel = zeroed();
    let mut user = zeroed();
    if GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
        return Err(native_error("PROCESS_UNAVAILABLE", "GetProcessTimes"));
    }
    let mut title = vec![0u16; (GetWindowTextLengthW(hwnd).clamp(0, 32767) + 1) as usize];
    let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
    let mut class = [0u16; 256];
    let c = GetClassNameW(hwnd, class.as_mut_ptr(), 256).max(0) as usize;
    let mut exe = vec![0u16; 32768];
    let mut len = exe.len() as u32;
    let executable = if QueryFullProcessImageNameW(process.0, 0, exe.as_mut_ptr(), &mut len) != 0 {
        String::from_utf16_lossy(&exe[..len as usize])
    } else {
        String::new()
    };
    Ok(WindowInfo {
        identity: WindowIdentity {
            id: WindowId(format!("win:{:x}", hwnd as usize)),
            process_id: pid,
            process_started: ((creation.dwHighDateTime as u64) << 32)
                | creation.dwLowDateTime as u64,
            class_name: String::from_utf16_lossy(&class[..c]),
        },
        title: String::from_utf16_lossy(&title[..n]),
        executable,
    })
}
fn hwnd(id: &WindowId) -> Result<HWND> {
    id.0.strip_prefix("win:")
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .filter(|v| *v != 0)
        .map(|v| v as HWND)
        .ok_or_else(|| {
            DesktopError::invalid(
                "Expected opaque Windows window_id returned by desktop_list_windows",
            )
        })
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn native_error(code: &str, context: &str) -> DesktopError {
    DesktopError::new(
        code,
        format!("{context}: {}", std::io::Error::last_os_error()),
    )
}

struct Hotkeys {
    thread_id: u32,
    join: Option<thread::JoinHandle<()>>,
}
impl Hotkeys {
    fn start(control: Arc<Control>) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(1);
        let join = thread::spawn(move || unsafe {
            let thread_id = GetCurrentThreadId();
            let mut msg: MSG = zeroed();
            PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
            let modifiers = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
            if RegisterHotKey(null_mut(), 1, modifiers, VK_F10 as u32) == 0 {
                let _ = tx.send(Err(native_error(
                    "HOTKEY_UNAVAILABLE",
                    "Cannot register Ctrl+Alt+F10",
                )));
                return;
            }
            if RegisterHotKey(null_mut(), 2, modifiers, VK_F11 as u32) == 0 {
                let error = native_error("HOTKEY_UNAVAILABLE", "Cannot register Ctrl+Alt+F11");
                UnregisterHotKey(null_mut(), 1);
                let _ = tx.send(Err(error));
                return;
            }
            let _ = tx.send(Ok(thread_id));
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                if msg.message == WM_HOTKEY {
                    if msg.wParam == 1 {
                        control.pause_user();
                    }
                    if msg.wParam == 2 {
                        control.resume_user();
                    }
                }
            }
            control.pause_user(); // A dead emergency-hotkey worker must not leave input enabled.
            UnregisterHotKey(null_mut(), 1);
            UnregisterHotKey(null_mut(), 2);
        });
        match rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(DesktopError::new(
                    "HOTKEY_UNAVAILABLE",
                    "Hotkey worker failed",
                ))
            }
        }
    }
}
impl Drop for Hotkeys {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
