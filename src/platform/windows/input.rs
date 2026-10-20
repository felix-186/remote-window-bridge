use super::*;
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
                ResolvedAction::Fill {
                    point,
                    text,
                    input_mode,
                } => {
                    if text.chars().any(char::is_control) {
                        return Err(DesktopError::invalid("Fill accepts literal text only; send Enter/Tab separately with desktop_key_at"));
                    }
                    if *input_mode == TextInputMode::ScanCode {
                        scan_code_text(text)?;
                    }
                    if *input_mode == TextInputMode::Auto {
                        return Err(DesktopError::invalid("Resolve text mode before injection"));
                    }
                    self.inject_component(
                        snapshot,
                        &ResolvedAction::Click {
                            point: *point,
                            button: MouseButton::Left,
                            count: 1,
                        },
                        cancel,
                        &mut sent,
                    )?;
                    thread::sleep(std::time::Duration::from_millis(20));
                    cancel.check()?;
                    ready(snapshot)?;
                    self.keyboard_anchor
                        .store(focused_child(snapshot)?, Ordering::SeqCst);
                    self.inject_component(
                        snapshot,
                        &ResolvedAction::TypeText {
                            text: text.clone(),
                            input_mode: *input_mode,
                        },
                        cancel,
                        &mut sent,
                    )?;
                }
                ResolvedAction::KeyAt { point, keys } => {
                    let mut codes = keys
                        .iter()
                        .map(|key| key_code(key))
                        .collect::<Result<Vec<_>>>()?;
                    codes.sort_unstable();
                    codes.dedup();
                    if codes.len() != keys.len() {
                        return Err(DesktopError::invalid("Duplicate keys in chord"));
                    }
                    self.inject_component(
                        snapshot,
                        &ResolvedAction::Click {
                            point: *point,
                            button: MouseButton::Left,
                            count: 1,
                        },
                        cancel,
                        &mut sent,
                    )?;
                    thread::sleep(std::time::Duration::from_millis(20));
                    cancel.check()?;
                    ready(snapshot)?;
                    self.keyboard_anchor
                        .store(focused_child(snapshot)?, Ordering::SeqCst);
                    self.inject_component(
                        snapshot,
                        &ResolvedAction::PressKey { keys: keys.clone() },
                        cancel,
                        &mut sent,
                    )?;
                }
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
                ResolvedAction::TypeText {
                    input_mode: TextInputMode::Auto,
                    ..
                } => {
                    return Err(DesktopError::invalid(
                        "Resolve automatic text mode before injection",
                    ))
                }
                ResolvedAction::TypeText {
                    text,
                    input_mode: TextInputMode::ScanCode,
                } => {
                    let keys = scan_code_text(text)?;
                    for key in keys {
                        cancel.check()?;
                        ready(snapshot)?;
                        modifiers_released()?;
                        self.check_keyboard_focus(snapshot)?;
                        let (events, releases) = scan_key_events(key.scan, key.shift);
                        send(&events, &releases, &mut sent)?;
                    }
                }
                ResolvedAction::TypeText {
                    text,
                    input_mode: TextInputMode::Unicode,
                } => {
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
                        .map(|vk| chord_key_event(*vk, true))
                        .collect();
                    let mut events: Vec<INPUT> =
                        codes.iter().map(|vk| chord_key_event(*vk, false)).collect();
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

fn chord_key_event(vk: u16, up: bool) -> INPUT {
    let mapped = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    let prefix = mapped >> 8;
    let release = if up { KEYEVENTF_KEYUP } else { 0 };
    // Pause has an E1 sequence; PrintScreen has special system semantics.
    // Preserve virtual-key handling for those and unmapped keys.
    if mapped & 0xff != 0 && matches!(prefix, 0 | 0xe0) && ![VK_PAUSE, VK_SNAPSHOT].contains(&vk) {
        let extension = if prefix == 0xe0 {
            KEYEVENTF_EXTENDEDKEY
        } else {
            extended(vk)
        };
        keyboard(
            0,
            (mapped & 0xff) as u16,
            KEYEVENTF_SCANCODE | extension | release,
        )
    } else {
        keyboard(vk, 0, extended(vk) | release)
    }
}

fn scan_key_events(scan: u16, shift: bool) -> (Vec<INPUT>, Vec<INPUT>) {
    let up = keyboard(0, scan, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);
    let shift_up = keyboard(0, 0x2a, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);
    let mut events = Vec::with_capacity(4);
    if shift {
        events.push(keyboard(0, 0x2a, KEYEVENTF_SCANCODE));
    }
    events.push(keyboard(0, scan, KEYEVENTF_SCANCODE));
    events.push(up);
    let mut releases = vec![up];
    if shift {
        events.push(shift_up);
        releases.push(shift_up);
    }
    (events, releases)
}

pub(super) fn focused_child(snapshot: &WindowSnapshot) -> Result<usize> {
    unsafe {
        let (activation, input) = snapshot_surfaces(snapshot)?;
        if !foreground_matches(activation, input) {
            return Err(foreground_error(
                &snapshot.identity.id,
                "Remote surfaces are not foreground",
            ));
        }
        let thread_id = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(thread_id, &mut info) == 0
            || info.hwndFocus.is_null()
            || ![activation, GetAncestor(input, GA_ROOT)]
                .contains(&GetAncestor(info.hwndFocus, GA_ROOT))
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
pub(super) fn send(events: &[INPUT], releases: &[INPUT], total: &mut u32) -> Result<()> {
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
pub(super) fn keyboard(vk: u16, scan: u16, flags: u32) -> INPUT {
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
pub(super) fn modifiers_released() -> Result<()> {
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

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn chords_supply_physical_scans_for_remote_clients() {
        for (vk, scan, extended) in [
            (VK_RETURN, 0x1c, 0),
            (VK_TAB, 0x0f, 0),
            (VK_LCONTROL, 0x1d, 0),
            (VK_LEFT, 0x4b, KEYEVENTF_EXTENDEDKEY),
            (VK_RCONTROL, 0x1d, KEYEVENTF_EXTENDEDKEY),
        ] {
            for up in [false, true] {
                let event = chord_key_event(vk, up);
                let key = unsafe { event.Anonymous.ki };
                assert_eq!(key.wVk, 0);
                assert_eq!(key.wScan, scan);
                assert_eq!(
                    key.dwFlags,
                    KEYEVENTF_SCANCODE | extended | if up { KEYEVENTF_KEYUP } else { 0 }
                );
            }
        }
        for vk in [VK_PAUSE, VK_SNAPSHOT] {
            let event = chord_key_event(vk, false);
            let key = unsafe { event.Anonymous.ki };
            assert_eq!(key.wVk, vk);
            assert_eq!(key.dwFlags & KEYEVENTF_SCANCODE, 0);
        }
    }

    #[test]
    fn activation_fallback_is_only_an_alt_press_and_release_with_cleanup() {
        let (events, releases) = activation_alt_events();
        assert_eq!(events.len(), 2);
        assert_eq!(releases.len(), 1);
        for (event, flags) in [
            (&events[0], 0),
            (&events[1], KEYEVENTF_KEYUP),
            (&releases[0], KEYEVENTF_KEYUP),
        ] {
            assert_eq!(event.r#type, INPUT_KEYBOARD);
            let key = unsafe { event.Anonymous.ki };
            assert_eq!(key.wVk, VK_MENU);
            assert_eq!(key.wScan, 0);
            assert_eq!(key.dwFlags, flags);
            assert_eq!(key.dwExtraInfo, INPUT_MARKER);
        }
    }

    #[test]
    fn physical_text_events_use_scancodes_and_release_shift_after_the_key() {
        for shift in [false, true] {
            let (events, releases) = scan_key_events(0x26, shift);
            let read = |event: &INPUT| unsafe {
                assert_eq!(event.r#type, INPUT_KEYBOARD);
                let key = event.Anonymous.ki;
                (key.wVk, key.wScan, key.dwFlags)
            };
            let mut expected = Vec::new();
            if shift {
                expected.push((0, 0x2a, KEYEVENTF_SCANCODE));
            }
            expected.push((0, 0x26, KEYEVENTF_SCANCODE));
            expected.push((0, 0x26, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP));
            if shift {
                expected.push((0, 0x2a, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP));
            }
            assert_eq!(events.iter().map(read).collect::<Vec<_>>(), expected);
            assert_eq!(
                releases.iter().map(read).collect::<Vec<_>>(),
                expected
                    .into_iter()
                    .filter(|(_, _, flags)| flags & KEYEVENTF_KEYUP != 0)
                    .collect::<Vec<_>>()
            );
        }
    }
}
