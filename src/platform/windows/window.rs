use super::*;
impl WindowBackend for WindowsPlatform {
    fn begin_activation(&self) {
        self.activation_fallback_foreground
            .store(0, Ordering::SeqCst);
    }
    fn diagnostics(&self, id: Option<&WindowId>) -> Result<serde_json::Value> {
        diagnostics(id)
    }
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
                "Explicit observations and the first wait sample activate the bound window while running; later wait samples and after-images remain passive; activation may use one Alt press/release if Windows refuses the initial request. No background focus keeper".into(),
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
            let (activation, input_surface) = surface_roles(hwnd)?;
            Ok(WindowSnapshot {
                identity: info.identity,
                activation: Some(activation),
                input_surface: Some(input_surface),
                client_screen: client,
                content_screen: content,
                dpi: GetDpiForWindow(hwnd),
            })
        }
    }
    fn focus(&self, identity: &WindowIdentity, cancel: &Cancellation) -> Result<()> {
        cancel.check_running()?;
        interactive_desktop()?;
        self.keyboard_anchor.store(0, Ordering::SeqCst);
        unsafe {
            let bound = hwnd(&identity.id)?;
            if info(bound)?.identity != *identity {
                return Err(DesktopError::new("WINDOW_LOST", "Window identity changed"));
            }
            let (activation, input) = surface_roles(bound)?;
            let hwnd = hwnd_from_identity(&activation)?;
            let input = hwnd_from_identity(&input)?;
            cancel.check()?;
            if foreground_matches(hwnd, input) {
                self.activation_fallback_foreground
                    .store(0, Ordering::SeqCst);
                return Ok(());
            }
            let previous = GetForegroundWindow();
            SetForegroundWindow(hwnd);
            if wait_for_foreground(hwnd, input, cancel)? {
                self.activation_fallback_foreground
                    .store(0, Ordering::SeqCst);
                return Ok(());
            }
            // One Alt press/release can release the foreground lock. Never send
            // task text/clicks to the previous application, alter global settings,
            // or attach input queues. Do not repeat Alt while waiting on the same
            // foreground window, or if the user switched windows during activation.
            if !previous.is_null()
                && GetForegroundWindow() == previous
                && self.activation_fallback_foreground.load(Ordering::SeqCst) != previous as usize
            {
                cancel.check_running()?;
                modifiers_released()?;
                if [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
                    .iter()
                    .any(|key| GetAsyncKeyState(i32::from(*key)) < 0)
                {
                    return Err(DesktopError::new(
                        "FOREGROUND_REQUIRED",
                        "Release the mouse before window activation",
                    ));
                }
                if info(bound)?.identity != *identity || info(hwnd)?.identity != activation {
                    return Err(DesktopError::new("WINDOW_LOST", "Window identity changed"));
                }
                self.activation_fallback_foreground
                    .store(previous as usize, Ordering::SeqCst);
                let (events, releases) = activation_alt_events();
                let mut sent = 0;
                send(&events, &releases, &mut sent).map_err(|error| error.with_sent(sent))?;
                thread::sleep(std::time::Duration::from_millis(20));
                cancel.check_running()?;
                let current = GetForegroundWindow();
                if current != previous && current != hwnd && current != input {
                    return Err(foreground_error(
                        &identity.id,
                        "Foreground changed during activation; observe again",
                    ));
                }
                SetForegroundWindow(hwnd);
                if wait_for_foreground(hwnd, input, cancel)? {
                    self.activation_fallback_foreground
                        .store(0, Ordering::SeqCst);
                    return Ok(());
                }
            }
            Err(foreground_error(
                &identity.id,
                "Automatic activation failed. Tell the user to select the remote window WITHOUT replying in the terminal, then call desktop_focus with timeout_ms=30000",
            ))
        }
    }
    fn prepare_observation(
        &self,
        identity: &WindowIdentity,
        cancel: &Cancellation,
    ) -> Result<bool> {
        let (activation, input) = surface_roles(hwnd(&identity.id)?)?;
        if foreground_matches(
            hwnd_from_identity(&activation)?,
            hwnd_from_identity(&input)?,
        ) {
            self.activation_fallback_foreground
                .store(0, Ordering::SeqCst);
            return Ok(false);
        }
        self.focus(identity, cancel)?;
        Ok(true)
    }
}

/// Read-only diagnostics: no controller lease, hotkeys, activation, or input.
pub fn diagnostics(id: Option<&WindowId>) -> Result<serde_json::Value> {
    unsafe {
        let previous_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        struct DpiContext(DPI_AWARENESS_CONTEXT);
        impl Drop for DpiContext {
            fn drop(&mut self) {
                unsafe {
                    if !self.0.is_null() {
                        SetThreadDpiAwarenessContext(self.0);
                    }
                }
            }
        }
        let _dpi = DpiContext(previous_dpi);
        let foreground = GetForegroundWindow();
        let mut gui: GUITHREADINFO = zeroed();
        gui.cbSize = size_of::<GUITHREADINFO>() as u32;
        let focused = if GetGUIThreadInfo(0, &mut gui) != 0 {
            gui.hwndFocus
        } else {
            null_mut()
        };
        Ok(serde_json::json!({
            "target":id.map(|id| hwnd(id).map(|window| window_diagnostics(window))).transpose()?,
            "foreground":window_diagnostics(foreground),
            "focused":window_diagnostics(focused),
            "virtual_screen":virtual_desktop()?,
        }))
    }
}

unsafe fn window_diagnostics(window: HWND) -> serde_json::Value {
    if window.is_null() || IsWindow(window) == 0 {
        return serde_json::Value::Null;
    }
    let token = |handle: HWND| {
        if handle.is_null() {
            None
        } else {
            Some(format!("win:{:x}", handle as usize))
        }
    };
    let mut rect: RECT = zeroed();
    let bounds = if GetWindowRect(window, &mut rect) != 0 {
        Some(
            serde_json::json!({"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top}),
        )
    } else {
        None
    };
    let mut origin = POINT { x: 0, y: 0 };
    let client = if GetClientRect(window, &mut rect) != 0
        && ClientToScreen(window, &mut origin) != 0
    {
        Some(
            serde_json::json!({"x":origin.x,"y":origin.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top}),
        )
    } else {
        None
    };
    let roles = surface_roles(window).map(|(activation, input)| serde_json::json!({"capture":token(window),"activation":activation,"input":input}));
    let information = info(window).ok();
    serde_json::json!({"id":token(window),"client_profile":information.as_ref().map(crate::platform::clients::profile),"info":information,"roles":roles.unwrap_or_else(|error| serde_json::json!({"error":error})),
        "parent_id":token(GetAncestor(window, GA_PARENT)),"owner_id":token(GetWindow(window, GW_OWNER)),
        "root_id":token(GetAncestor(window, GA_ROOT)),"root_owner_id":token(GetAncestor(window, GA_ROOTOWNER)),
        "visible":IsWindowVisible(window)!=0,"minimized":IsIconic(window)!=0,
        "window_screen":bounds,"client_screen":client})
}

pub(super) fn foreground_matches(activation: HWND, input: HWND) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    foreground == activation || foreground == input
}

pub(super) fn hwnd_from_identity(identity: &WindowIdentity) -> Result<HWND> {
    let window = hwnd(&identity.id)?;
    if unsafe { info(window)? }.identity != *identity {
        return Err(DesktopError::new(
            "WINDOW_LOST",
            "A bound activation/input surface was replaced",
        ));
    }
    Ok(window)
}

unsafe fn client_rect(window: HWND) -> Result<Rect> {
    let mut rect: RECT = zeroed();
    let mut origin = POINT { x: 0, y: 0 };
    if GetClientRect(window, &mut rect) == 0 || ClientToScreen(window, &mut origin) == 0 {
        return Err(native_error(
            "GEOMETRY_UNAVAILABLE",
            "Cannot inspect client surface",
        ));
    }
    Rect {
        x: origin.x,
        y: origin.y,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    }
    .validate()
}

pub(super) fn surface_roles(target: HWND) -> Result<(WindowIdentity, WindowIdentity)> {
    unsafe {
        let target_info = info(target)?;
        let root = GetAncestor(target, GA_ROOT);
        let root_info = info(root)?;
        if root_info.identity.process_id != target_info.identity.process_id
            || root_info.identity.process_started != target_info.identity.process_started
        {
            return Err(DesktopError::new(
                "WINDOW_UNSUPPORTED",
                "Cross-process embedded windows are not supported",
            ));
        }
        if crate::platform::clients::awesun_expected_class(&target_info).is_none() {
            return Ok((root_info.identity, target_info.identity));
        }
        struct Candidates {
            process_id: u32,
            windows: Vec<(WindowInfo, Rect)>,
        }
        unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
            let candidates = &mut *(data as *mut Candidates);
            let mut process_id = 0;
            GetWindowThreadProcessId(window, &mut process_id);
            if process_id == candidates.process_id
                && IsWindowVisible(window) != 0
                && IsIconic(window) == 0
            {
                if let (Ok(info), Ok(rect)) = (info(window), client_rect(window)) {
                    candidates.windows.push((info, rect));
                }
            }
            1
        }
        let mut candidates = Candidates {
            process_id: target_info.identity.process_id,
            windows: Vec::new(),
        };
        if EnumWindows(Some(collect), &mut candidates as *mut _ as LPARAM) == 0 {
            return Err(native_error(
                "ENUMERATION_FAILED",
                "Cannot resolve native surfaces",
            ));
        }
        if let Some(counterpart) =
            awesun_counterpart(&target_info, client_rect(target)?, &candidates.windows)?
        {
            if target_info.identity.class_name == "FlutterMultiWindow" {
                Ok((target_info.identity, counterpart))
            } else {
                Ok((counterpart, target_info.identity))
            }
        } else {
            Ok((root_info.identity, target_info.identity))
        }
    }
}

pub(super) fn snapshot_surfaces(snapshot: &WindowSnapshot) -> Result<(HWND, HWND)> {
    let activation =
        hwnd_from_identity(snapshot.activation.as_ref().unwrap_or(&snapshot.identity))?;
    let input = hwnd_from_identity(
        snapshot
            .input_surface
            .as_ref()
            .unwrap_or(&snapshot.identity),
    )?;
    let target = hwnd_from_identity(&snapshot.identity)?;
    unsafe {
        for surface in [activation, input] {
            if IsWindowVisible(surface) == 0 || IsIconic(surface) != 0 {
                return Err(DesktopError::new(
                    "WINDOW_HIDDEN",
                    "An activation/input surface is hidden or minimized",
                ));
            }
        }
        if (activation != target
            && IsChild(activation, target) == 0
            && client_rect(activation)? != snapshot.client_screen)
            || (input != target
                && IsChild(target, input) == 0
                && client_rect(input)? != snapshot.client_screen)
        {
            return Err(DesktopError::new(
                "GEOMETRY_CHANGED",
                "Paired surface geometry changed; observe again",
            ));
        }
    }
    Ok((activation, input))
}

pub(super) fn foreground_error(target: &WindowId, message: &str) -> DesktopError {
    DesktopError::new("FOREGROUND_REQUIRED", message).with_details(
        diagnostics(Some(target))
            .unwrap_or_else(|error| serde_json::json!({"diagnostics_error":error})),
    )
}

fn wait_for_foreground(target: HWND, input: HWND, cancel: &Cancellation) -> Result<bool> {
    for _ in 0..10 {
        cancel.check_running()?;
        if foreground_matches(target, input) {
            return Ok(true);
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
    cancel.check_running()?;
    Ok(foreground_matches(target, input))
}

pub(super) fn activation_alt_events() -> ([INPUT; 2], [INPUT; 1]) {
    let release = keyboard(VK_MENU, 0, KEYEVENTF_KEYUP);
    ([keyboard(VK_MENU, 0, 0), release], [release])
}
