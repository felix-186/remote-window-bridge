// Built by build.rs and embedded in the Rust executable. stdout is protocol only.
import AppKit
import ApplicationServices
import ScreenCaptureKit
import Darwin

struct BridgeError: Error {
    let code: String
    let message: String
    init(_ code: String, _ message: String) { self.code = code; self.message = message }
}
typealias Object = [String: Any]
func fail(_ code: String, _ message: String) throws -> Never { throw BridgeError(code, message) }
func number(_ value: Any?) -> Double { (value as? NSNumber)?.doubleValue ?? 0 }
func rect(_ value: Object) -> CGRect {
    CGRect(x: number(value["x"]), y: number(value["y"]),
           width: number(value["width"]), height: number(value["height"]))
}
func object(_ value: CGRect, scale: Double) -> Object {
    ["x": Int((value.minX * scale).rounded()), "y": Int((value.minY * scale).rounded()),
     "width": Int((value.width * scale).rounded()), "height": Int((value.height * scale).rounded())]
}
func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value
}
func axBounds(_ element: AXUIElement) -> CGRect? {
    guard let p = attribute(element, kAXPositionAttribute), let s = attribute(element, kAXSizeAttribute),
          CFGetTypeID(p) == AXValueGetTypeID(), CFGetTypeID(s) == AXValueGetTypeID() else { return nil }
    var point = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(p as! AXValue, .cgPoint, &point),
          AXValueGetValue(s as! AXValue, .cgSize, &size) else { return nil }
    return CGRect(origin: point, size: size)
}
struct Window {
    let row: Object
    let id: CGWindowID
    let app: NSRunningApplication
    let bounds: CGRect
    var identity: Object {
        ["id": "mac:\(id)", "process_id": Int(app.processIdentifier),
         "process_started": UInt64(app.launchDate!.timeIntervalSince1970 * 1_000_000),
         "class_name": app.bundleIdentifier ?? (row[kCGWindowOwnerName as String] as? String ?? "")]
    }
    var info: Object {
        ["identity": identity, "title": row[kCGWindowName as String] as? String ?? "",
         "executable": app.executableURL?.path ?? ""]
    }
    func scale() throws -> Double {
        for screen in NSScreen.screens {
            guard let id = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber else { continue }
            if CGDisplayBounds(id.uint32Value).contains(bounds) { return Double(screen.backingScaleFactor) }
        }
        try fail("GEOMETRY_UNAVAILABLE", "Window must fit entirely inside one display; move it and observe again")
    }
    func axWindow() throws -> AXUIElement {
        let application = AXUIElementCreateApplication(app.processIdentifier)
        AXUIElementSetMessagingTimeout(application, 1.0)
        guard let windows = attribute(application, kAXWindowsAttribute) as? [AXUIElement] else {
            try fail("ACCESSIBILITY_REQUIRED", "Cannot inspect application windows; grant Accessibility permission")
        }
        // Public AX does not expose CGWindowID. Require an unambiguous geometry match.
        let matches = windows.filter { axBounds($0) == bounds }
        guard matches.count == 1 else { try fail("WINDOW_UNSUPPORTED", "Cannot uniquely match Quartz window to Accessibility window") }
        return matches[0]
    }
    func foreground() throws -> Bool {
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier else { return false }
        let application = AXUIElementCreateApplication(app.processIdentifier)
        guard let focused = attribute(application, kAXFocusedWindowAttribute) else { return false }
        return CFEqual(focused, try axWindow())
    }
}
func rows() throws -> [Object] {
    guard let result = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [Object] else {
        try fail("ENUMERATION_FAILED", "Cannot enumerate current desktop windows")
    }
    return result
}
func window(_ row: Object) -> Window? {
    guard number(row[kCGWindowLayer as String]) == 0,
          number(row[kCGWindowAlpha as String]) > 0,
          let pid = row[kCGWindowOwnerPID as String] as? NSNumber,
          let app = NSRunningApplication(processIdentifier: pid.int32Value), app.launchDate != nil,
          let bounds = row[kCGWindowBounds as String] as? Object,
          let area = CGRect(dictionaryRepresentation: bounds as CFDictionary), area.width > 0, area.height > 0,
          let id = row[kCGWindowNumber as String] as? NSNumber else { return nil }
    return Window(row: row, id: id.uint32Value, app: app, bounds: area)
}
func find(_ id: String) throws -> Window {
    guard id.hasPrefix("mac:"), let token = UInt32(id.dropFirst(4)), token != 0 else {
        try fail("INVALID_ARGUMENT", "Use an opaque mac: window ID returned by desktop_list_windows")
    }
    for row in try rows() where (row[kCGWindowNumber as String] as? NSNumber)?.uint32Value == token {
        if let target = window(row) { return target }
    }
    try fail("WINDOW_LOST", "Window is hidden, minimized, closed or unavailable on the current Space")
}
func bound(_ identity: Object) throws -> Window {
    let target = try find(identity["id"] as? String ?? "")
    guard NSDictionary(dictionary: target.identity).isEqual(to: identity) else {
        try fail("WINDOW_LOST", "Window process identity changed")
    }
    return target
}
func snapshot(_ target: Window) throws -> Object {
    let scale = try target.scale()
    let bounds = object(target.bounds, scale: scale)
    return ["identity": target.identity, "activation": target.identity, "input_surface": target.identity,
            "client_screen": bounds, "content_screen": bounds, "dpi": Int((96 * scale).rounded())]
}
func unlocked() throws {
    guard let session = CGSessionCopyCurrentDictionary() as? Object,
          session["kCGSessionOnConsoleKey"] as? Bool == true,
          session["CGSSessionScreenIsLocked"] as? Bool != true else {
        try fail("NO_INTERACTIVE_DESKTOP", "An unlocked, active console session is required")
    }
}
func ready(_ saved: Object) throws -> Window {
    try unlocked()
    guard AXIsProcessTrusted() else { try fail("ACCESSIBILITY_REQUIRED", "Grant Accessibility permission in System Settings") }
    let target = try bound(saved["identity"] as? Object ?? [:])
    let current = try snapshot(target)
    guard NSDictionary(dictionary: current["client_screen"] as! Object).isEqual(to: saved["client_screen"] as? Object ?? [:]),
          number(current["dpi"]) == number(saved["dpi"]) else {
        try fail("GEOMETRY_CHANGED", "Window moved or display scale changed; observe again")
    }
    guard try target.foreground() else { try fail("FOREGROUND_REQUIRED", "Activate target window and observe again") }
    let scale = try target.scale()
    let content = rect(saved["content_screen"] as? Object ?? [:])
    guard content.width > 0, content.height > 0, content.width * content.height * 4 <= 64 * 1024 * 1024,
          rect(current["client_screen"] as! Object).contains(content) else {
        try fail("INVALID_ARGUMENT", "Viewport must fit inside the window and the 64 MiB frame limit")
    }
    let logical = CGRect(x: content.minX / scale, y: content.minY / scale,
                         width: content.width / scale, height: content.height / scale)
    for row in try rows() {
        if (row[kCGWindowNumber as String] as? NSNumber)?.uint32Value == target.id { break }
        if number(row[kCGWindowAlpha as String]) > 0,
           let b = row[kCGWindowBounds as String] as? Object,
           let area = CGRect(dictionaryRepresentation: b as CFDictionary), area.intersects(logical) {
            try fail("POINT_OCCLUDED", "Another local window overlaps the viewport")
        }
    }
    return target
}

// US physical key positions. Modifier flags are posted with each event.
let letterCodes: [UInt16] = [0,11,8,2,14,3,5,4,34,38,40,37,46,45,31,35,12,15,1,17,32,9,13,7,16,6]
let punctuation: [(Character, Character, UInt16)] = [
    ("1","!",18),("2","@",19),("3","#",20),("4","$",21),("5","%",23),
    ("6","^",22),("7","&",26),("8","*",28),("9","(",25),("0",")",29),
    ("-","_",27),("=","+",24),("[","{",33),("]","}",30),(";",":",41),
    ("'","\"",39),("`","~",50),("\\","|",42),(",","<",43),(".",">",47),("/","?",44)]
func asciiKey(_ character: Character) throws -> (UInt16, Bool) {
    if character == " " { return (49, false) }
    if let byte = character.asciiValue, (65...90).contains(byte) || (97...122).contains(byte) {
        return (letterCodes[Int((byte | 32) - 97)], byte < 97)
    }
    for (plain, shifted, key) in punctuation {
        if character == plain || character == shifted { return (key, character == shifted) }
    }
    try fail("INVALID_ARGUMENT", "scan_code accepts printable ASCII only")
}
func keyCode(_ key: String) throws -> UInt16 {
    let named: [String: UInt16] = ["CTRL":59,"CONTROL":59,"ALT":58,"OPTION":58,"SHIFT":56,
        "WIN":55,"META":55,"CMD":55,"COMMAND":55,"SUPER":55,"ENTER":36,"RETURN":36,
        "TAB":48,"SPACE":49,"ESC":53,"ESCAPE":53,"BACKSPACE":51,"DELETE":117,
        "INSERT":114,"HOME":115,"END":119,"PAGEUP":116,"PAGEDOWN":121,
        "LEFT":123,"RIGHT":124,"DOWN":125,"UP":126,
        "F1":122,"F2":120,"F3":99,"F4":118,"F5":96,"F6":97,"F7":98,"F8":100,
        "F9":101,"F10":109,"F11":103,"F12":111]
    if let value = named[key.uppercased()] { return value }
    if key.count == 1, let character = key.lowercased().first { return try asciiKey(character).0 }
    try fail("INVALID_ARGUMENT", "Unsupported macOS key: \(key)")
}
func keys(_ names: [String]) throws -> [UInt16] {
    let codes = try names.map(keyCode)
    guard !codes.isEmpty, codes.count <= 8, Set(codes).count == codes.count else {
        try fail("INVALID_ARGUMENT", "Key chord must contain 1..8 unique keys")
    }
    return codes
}
func modifier(_ key: UInt16) -> CGEventFlags {
    switch key { case 55: return .maskCommand; case 56: return .maskShift
    case 58: return .maskAlternate; case 59: return .maskControl; default: return [] }
}

@MainActor final class NativeBridge {
    var lease: Int32 = -1
    var anchor: AXUIElement?
    var anchorWindow: String?
    var captureTarget: (identity: NSDictionary, bounds: CGRect, filter: SCContentFilter)?
    deinit { if lease >= 0 { close(lease) } }
    func focusAnchor(_ target: Window) throws -> AXUIElement {
        let app = AXUIElementCreateApplication(target.app.processIdentifier)
        guard let element = attribute(app, kAXFocusedUIElementAttribute), CFGetTypeID(element) == AXUIElementGetTypeID() else {
            try fail("CONTENT_FOCUS_REQUIRED", "Click the remote input field before keyboard input")
        }
        return element as! AXUIElement
    }
    func checkAnchor(_ target: Window) throws {
        guard let saved = anchor, anchorWindow == "mac:\(target.id)", CFEqual(saved, try focusAnchor(target)) else {
            try fail("CONTENT_FOCUS_REQUIRED", "Keyboard focus changed; click the remote input field again")
        }
    }
    func inject(_ request: Object) throws -> Object {
        guard lease >= 0 else { try fail("LEASE_CONFLICT", "Native input lease not acquired") }
        let saved = request["snapshot"] as? Object ?? [:]
        let target = try ready(saved)
        let action = request["action"] as? Object ?? [:]
        let kind = action["kind"] as? String ?? ""
        let physical = rect(saved["content_screen"] as! Object)
        let pointData = action["point"] as? Object ?? [:]
        let point = CGPoint(x: number(pointData["x"]), y: number(pointData["y"]))
        let scale = try target.scale()
        let logical = CGPoint(x: point.x / scale, y: point.y / scale)
        let flags = CGEventSource.flagsState(.combinedSessionState)
        guard flags.intersection([.maskShift, .maskControl, .maskAlternate, .maskCommand]).isEmpty else {
            try fail("MODIFIERS_HELD", "Release local modifier keys before sending input")
        }
        let source = CGEventSource(stateID: .privateState)
        var events: [CGEvent] = []
        func keyboard(_ code: UInt16, _ down: Bool, _ flags: CGEventFlags = []) throws -> CGEvent {
            guard let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else {
                try fail("INPUT_UNAVAILABLE", "Cannot allocate keyboard event")
            }
            e.flags = flags
            return e
        }
        func mouse(_ type: CGEventType, _ button: CGMouseButton = .left) throws -> CGEvent {
            guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: logical, mouseButton: button) else {
                try fail("INPUT_UNAVAILABLE", "Cannot allocate mouse event")
            }
            return e
        }
        switch kind {
        case "click", "move", "scroll":
            guard physical.contains(point) else { try fail("INVALID_ARGUMENT", "Point is outside the viewport") }
            var hit: AXUIElement?
            guard AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(logical.x), Float(logical.y), &hit) == .success,
                  let element = hit else { try fail("POINT_OCCLUDED", "Cannot hit-test input point") }
            var pid: pid_t = 0
            guard AXUIElementGetPid(element, &pid) == .success, pid == target.app.processIdentifier else {
                try fail("POINT_OCCLUDED", "Input point belongs to another application")
            }
            events.append(try mouse(.mouseMoved))
            if kind == "click" {
                let name = action["button"] as? String ?? "left"
                let button: CGMouseButton = name == "right" ? .right : (name == "middle" ? .center : .left)
                let down: CGEventType = button == .right ? .rightMouseDown : (button == .center ? .otherMouseDown : .leftMouseDown)
                let up: CGEventType = button == .right ? .rightMouseUp : (button == .center ? .otherMouseUp : .leftMouseUp)
                let count = Int(number(action["count"]))
                guard (1...2).contains(count) else { try fail("INVALID_ARGUMENT", "Click count must be 1 or 2") }
                for n in 1...count {
                    let d = try mouse(down, button), u = try mouse(up, button)
                    d.setIntegerValueField(.mouseEventClickState, value: Int64(n))
                    u.setIntegerValueField(.mouseEventClickState, value: Int64(n))
                    events += [d, u]
                }
            } else if kind == "scroll" {
                let ticks = Int32(number(action["ticks"]))
                let horizontal = action["horizontal"] as? Bool ?? false
                guard let e = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2,
                    wheel1: horizontal ? 0 : ticks, wheel2: horizontal ? ticks : 0, wheel3: 0) else {
                    try fail("INPUT_UNAVAILABLE", "Cannot allocate scroll event")
                }
                e.location = logical
                events.append(e)
            }
        case "type_text":
            try checkAnchor(target)
            let text = action["text"] as? String ?? ""
            if action["input_mode"] as? String == "scan_code" {
                for character in text {
                    let (code, shift) = try asciiKey(character)
                    if shift { events.append(try keyboard(56, true, .maskShift)) }
                    events.append(try keyboard(code, true, shift ? .maskShift : []))
                    events.append(try keyboard(code, false, shift ? .maskShift : []))
                    if shift { events.append(try keyboard(56, false)) }
                }
            } else {
                let units = Array(text.utf16)
                let down = try keyboard(0, true), up = try keyboard(0, false)
                units.withUnsafeBufferPointer { buffer in
                    down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress!)
                    up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: buffer.baseAddress!)
                }
                events = [down, up]
            }
        case "press_key":
            try checkAnchor(target)
            let codes = try keys(action["keys"] as? [String] ?? [])
            var flags: CGEventFlags = []
            for code in codes { flags.formUnion(modifier(code)); events.append(try keyboard(code, true, flags)) }
            for code in codes.reversed() { flags.subtract(modifier(code)); events.append(try keyboard(code, false, flags)) }
        default: try fail("INVALID_ARGUMENT", "Unsupported native action")
        }
        // Allocate and validate the whole event group before posting any part.
        _ = try ready(saved)
        if kind == "type_text" || kind == "press_key" { try checkAnchor(target) }
        for event in events { event.post(tap: .cghidEventTap) }
        if kind == "click" {
            // Resolve after the client has processed the mouse event. Failure revokes typing.
            usleep(20_000)
            anchor = try? focusAnchor(target)
            anchorWindow = "mac:\(target.id)"
        }
        return ["sent_events": events.count]
    }
    func handle(_ request: Object) async throws -> (Object, Data) {
        switch request["op"] as? String ?? "" {
        case "lease":
            if lease < 0 {
                let path = NSHomeDirectory() + "/Library/Caches/remote-window-bridge/input.lock"
                lease = open(path, O_CREAT | O_RDWR | O_NOFOLLOW, mode_t(0o600))
                guard lease >= 0 else { try fail("LEASE_CONFLICT", "Cannot open native input lock") }
                guard flock(lease, LOCK_EX | LOCK_NB) == 0 else {
                    close(lease); lease = -1
                    try fail("LEASE_CONFLICT", "Another bridge process controls this macOS user session")
                }
            }
            return ([:], Data())
        case "permissions":
            return (["screen_capture": CGPreflightScreenCaptureAccess(), "input_control": AXIsProcessTrusted()], Data())
        case "windows":
            return (["windows": try rows().compactMap(window).map { $0.info }], Data())
        case "inspect":
            return (["snapshot": try snapshot(find(request["id"] as? String ?? ""))], Data())
        case "validate_keys":
            _ = try keys(request["keys"] as? [String] ?? [])
            return ([:], Data())
        case "foreground":
            return (["foreground": try bound(request["identity"] as? Object ?? [:]).foreground()], Data())
        case "focus":
            try unlocked()
            guard AXIsProcessTrusted() else { try fail("ACCESSIBILITY_REQUIRED", "Grant Accessibility permission") }
            let target = try bound(request["identity"] as? Object ?? [:])
            let element = try target.axWindow()
            anchor = nil; anchorWindow = nil
            target.app.activate(options: [.activateIgnoringOtherApps])
            guard AXUIElementPerformAction(element, kAXRaiseAction as CFString) == .success else {
                try fail("FOREGROUND_REQUIRED", "Cannot raise target window")
            }
            return ([:], Data())
        case "diagnostics":
            var result: Object = ["frontmost_process_id": Int(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0),
                "screen_capture": CGPreflightScreenCaptureAccess(), "input_control": AXIsProcessTrusted(),
                "capture_backend": "screencapturekit_window"]
            if let id = request["id"] as? String { result["target"] = try find(id).info }
            return (result, Data())
        case "inject": return (try inject(request), Data())
        case "capture":
            guard CGPreflightScreenCaptureAccess() else { try fail("SCREEN_CAPTURE_REQUIRED", "Grant Screen Recording permission in System Settings") }
            let saved = request["snapshot"] as? Object ?? [:]
            let target = try ready(saved)
            let identity = NSDictionary(dictionary: target.identity)
            if captureTarget?.identity != identity || captureTarget?.bounds != target.bounds {
                captureTarget = nil
                let available = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
                guard let scWindow = available.windows.first(where: { $0.windowID == target.id }), scWindow.frame == target.bounds else {
                    try fail("GEOMETRY_CHANGED", "Window changed while preparing capture")
                }
                captureTarget = (identity, target.bounds, SCContentFilter(desktopIndependentWindow: scWindow))
            }
            let scale = try target.scale()
            let config = SCStreamConfiguration()
            config.width = Int((target.bounds.width * scale).rounded())
            config.height = Int((target.bounds.height * scale).rounded())
            guard config.width * config.height * 4 <= 64 * 1024 * 1024 else {
                try fail("FRAME_TOO_LARGE", "Window exceeds 64 MiB capture limit")
            }
            config.showsCursor = false
            config.ignoreShadowsSingleWindow = true
            let image: CGImage
            do {
                image = try await SCScreenshotManager.captureImage(
                    contentFilter: captureTarget!.filter, configuration: config)
            } catch {
                captureTarget = nil
                throw error
            }
            _ = try ready(saved)
            let area = rect(saved["content_screen"] as! Object)
            let client = rect(saved["client_screen"] as! Object)
            let crop = CGRect(x: area.minX - client.minX, y: area.minY - client.minY, width: area.width, height: area.height)
            guard image.width == config.width, image.height == config.height, let cropped = image.cropping(to: crop) else {
                try fail("INVALID_FRAME", "Unexpected ScreenCaptureKit image dimensions")
            }
            let width = cropped.width, height = cropped.height
            var rgba = [UInt8](repeating: 0, count: width * height * 4)
            let drawn = rgba.withUnsafeMutableBytes { bytes -> Bool in
                guard let context = CGContext(data: bytes.baseAddress, width: width, height: height,
                    bitsPerComponent: 8, bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue) else { return false }
                context.draw(cropped, in: CGRect(x: 0, y: 0, width: width, height: height))
                return true
            }
            guard drawn else { try fail("CAPTURE_UNAVAILABLE", "Cannot convert screenshot to RGB") }
            var rgb = Data(count: width * height * 3)
            rgb.withUnsafeMutableBytes { (out: UnsafeMutableRawBufferPointer) in
                for index in 0..<(width * height) {
                    out[index * 3] = rgba[index * 4]
                    out[index * 3 + 1] = rgba[index * 4 + 1]
                    out[index * 3 + 2] = rgba[index * 4 + 2]
                }
            }
            return ([:], rgb)
        default: try fail("INVALID_ARGUMENT", "Unknown native operation")
        }
    }
}

@main struct Main {
    @MainActor static func main() async {
        // No implicit permission prompt or automatic retry of input operations.
        let bridge = NativeBridge()
        while let line = readLine() {
            var header: Object
            var payload = Data()
            do {
                guard let data = line.data(using: .utf8), let request = try JSONSerialization.jsonObject(with: data) as? Object else {
                    try fail("INVALID_ARGUMENT", "Invalid JSON request")
                }
                (header, payload) = try await bridge.handle(request)
            } catch let error as BridgeError {
                header = ["error": ["code": error.code, "message": error.message]]
            } catch {
                header = ["error": ["code": "MACOS_NATIVE_ERROR", "message": String(describing: error)]]
            }
            header["payload_len"] = payload.count
            do {
                var data = try JSONSerialization.data(withJSONObject: header, options: [.sortedKeys])
                data.append(10)
                try FileHandle.standardOutput.write(contentsOf: data)
                if !payload.isEmpty { try FileHandle.standardOutput.write(contentsOf: payload) }
            } catch { break }
        }
    }
}
