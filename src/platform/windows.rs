//! Win32 implementation. Native pointers are confined to this module.
//! v0.1 deliberately uses visible screen-region capture, not unverified PrintWindow output.
use super::{
    keyboard::scan_code_text, surfaces::awesun_counterpart, CaptureBackend, InputBackend,
    WindowBackend,
};
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

mod capture;
mod input;
mod window;
use input::{focused_child, keyboard, modifiers_released, send};
#[cfg(test)]
use window::activation_alt_events;
pub use window::diagnostics;
use window::{foreground_error, foreground_matches, snapshot_surfaces};
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
    activation_fallback_foreground: AtomicUsize,
}
impl WindowsPlatform {
    fn inject_component(
        &self,
        snapshot: &WindowSnapshot,
        action: &ResolvedAction,
        cancel: &Cancellation,
        total: &mut u32,
    ) -> Result<()> {
        match self.inject(snapshot, action, cancel) {
            Ok(report) => {
                *total += report.sent_events;
                Ok(())
            }
            Err(error) => {
                *total += error.sent_events.unwrap_or(0);
                Err(error)
            }
        }
    }
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
                activation_fallback_foreground: AtomicUsize::new(0),
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
        let (activation, input) = snapshot_surfaces(snapshot)?;
        if !foreground_matches(activation, input) {
            return Err(foreground_error(
                &snapshot.identity.id,
                "Remote surfaces are not foreground; activate then observe again",
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
            ).with_details(serde_json::json!({"content_screen":content,"virtual_screen":desktop,"client_screen":snapshot.client_screen})));
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
            if above != activation
                && above != input
                && IsWindowVisible(above) != 0
                && IsIconic(above) == 0
                && cloaked == 0
            {
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
        let (activation, input) = snapshot_surfaces(snapshot)?;
        let actual = WindowFromPoint(POINT {
            x: point.x,
            y: point.y,
        });
        if actual.is_null()
            || ![activation, GetAncestor(input, GA_ROOT)].contains(&GetAncestor(actual, GA_ROOT))
        {
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
