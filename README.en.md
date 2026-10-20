# remote-window-bridge

[简体中文](README.md) | [English](README.en.md)

A remote desktop control tool written in Rust. Codex, Claude Code, and other MCP agents can observe, click, and type through an **already-connected remote desktop window** on the local machine, such as RDP, ToDesk, or Sunlogin. No remote-window-bridge service needs to be installed on the remote computer.

Current release: **0.1.0, initial Windows CLI and MCP stdio implementation**. The shared core builds and runs tests on macOS and Linux, but native desktop backends for those platforms are not implemented. Builds, type checks, and tests do not replace acceptance testing with real remote desktop clients on Windows.

## Implemented features

- Window enumeration and binding, with checks for window ID, PID, process creation time, and window class.
- Client-area viewport calibration, physical pixel and normalized coordinate mapping, and secondary monitors with negative coordinates.
- Capture of the visible Windows screen region corresponding to the bound viewport, returned as inline PNG content through MCP.
- SendInput support for single and double clicks, left/right/middle buttons, pointer movement, vertical/horizontal scrolling, Unicode or ASCII scan-code text, and key chords.
- New observations attempt to activate the bound window automatically. Preflight checks cover the active session and input desktop, foreground window, occlusion, geometry, frame age, and scene changes. Keyboard input also checks focus against the most recent content click.
- Local pause with `Ctrl+Alt+F10` and resume with `Ctrl+Alt+F11`. Agents cannot clear a user pause.
- Action deduplication using `action_id`, persistent auditing, and queries for uncertain results after a crash. Actions with side effects are not automatically replayed.
- One control process per Windows session to prevent agents from competing for input.
- A Windows DPI manifest.

The program does not embed a specific model or impose task-specific blocklists for terminals, deletion, or other business operations. Task planning and business confirmations remain with the user and their chosen agent.

## Build on Windows and take your first screenshot

Requirements: Rust 1.83 or newer, Visual Studio C++ Build Tools, and the Windows SDK using the MSVC toolchain.

```powershell
cargo build --locked --release
.\target\release\remote-window-bridge.exe doctor
.\target\release\remote-window-bridge.exe windows
```

Find the remote desktop window's `identity.id` in the `windows` output and replace the example ID below:

```powershell
.\target\release\remote-window-bridge.exe capture --window win:a1234 --delay-ms 3000 --output preview.png
```

You have three seconds to switch to the remote desktop window. During capture, it must be in the foreground, not minimized, and unobstructed. Inspect the screenshot and specify a viewport within the client area to exclude local toolbars, letterboxing, and sidebars:

```powershell
.\target\release\remote-window-bridge.exe capture --window win:a1234 --viewport 0,40,1280,720 --output preview.png
```

The viewport uses physical pixels in `x,y,width,height` format. Adjust the example dimensions to match your window. The program does not establish remote connections or log in to remote computers.

## Connect an agent

Place the executable at a stable path, such as `C:\remote-window-bridge\remote-window-bridge.exe`.

For Codex, add this to `config.toml`:

```toml
[mcp_servers.remote_desktop]
command = 'C:\remote-window-bridge\remote-window-bridge.exe'
args = ['mcp', '--transport', 'stdio']
```

For Claude Code:

```powershell
claude.cmd mcp add --transport stdio --scope user remote_desktop -- C:\remote-window-bridge\remote-window-bridge.exe mcp --transport stdio
```

For an npm installation of Claude Code in PowerShell, use `claude.cmd` so that the `claude.ps1` wrapper does not consume the `--` separator and cause server flags to be parsed as Claude options. In other shells or native installations, use the actual Claude executable command.

Other agents can use the same stdio command. Clients must support MCP image content to interpret screenshots directly; their own tool permissions still apply. Do not start two control processes in the same Windows session.

Suggested agent instructions:

> Use the remote_desktop tools. List windows, bind the already-connected remote desktop, and calibrate the viewport. Inspect the actual screenshot after focus. If inline images are unavailable, open frame.image_path with a local image-capable tool; if you still cannot see it, stop visual actions and never invent a screen description. Every new action must reference the latest frame_id and use a unique action_id. Serialize calls within one session, use next_frame_id for subsequent actions, and never send dependent actions in parallel. An injected result means only local OS acceptance: inspect the after-image and verify text is visible before pressing Enter. For ASCII commands, use input_mode=scan_code with a remote US keyboard layout, English input, and CapsLock off. After a terminal conversation, call desktop_observe directly so the program activates the bound window automatically. Only if automatic activation fails, wait for manual selection with desktop_focus and timeout_ms=30000; never ask the user to return to the terminal to confirm. Do not focus before every input; it clears keyboard focus verification. After a timeout, query desktop_get_action before repeating an action. If the user pauses control, wait for them to resume locally.

For this release, run the agent and executable on the same Windows machine. WSL, another host, or a cloud container cannot be assumed to have access to that machine's Windows desktop.

## MCP tools

| Category | Tools |
| --- | --- |
| Windows | `desktop_list_windows`, `desktop_attach`, `desktop_detach`, `desktop_focus` |
| Observation | `desktop_get_status`, `desktop_observe`, `desktop_wait` |
| Composite input | `desktop_fill` (click and type), `desktop_key_at` (click and send a chord) |
| Input | `desktop_click`, `desktop_move`, `desktop_scroll`, `desktop_type_text`, `desktop_press_key` |
| Control and recovery | `desktop_pause`, `desktop_resume`, `desktop_get_action` |

Example arguments for `desktop_click`:

```json
{
  "session_id": "returned-by-attach",
  "frame_id": "returned-by-latest-observation",
  "action_id": "task-01-step-01",
  "x": 640,
  "y": 360,
  "coordinate_space": "image_pixels",
  "button": "left",
  "click_count": 1
}
```

You can also use `coordinate_space: "normalized"`, with x and y in `[0,1]`. Text and key tools require the same three IDs, and you must click the remote content or input field first. Each action consumes the previous observation; successful injection usually returns a new screenshot. Frames expire after 120 seconds by default. Significant scene or geometry changes can cause input to be rejected earlier.

### Reduce tool round trips

For routine input, prefer `desktop_fill`: provide the three IDs, x/y selected from the latest image, and `text`. One call activates the target, validates the original frame and fresh scene, clicks, types, and returns an after-image. It appends text without clearing the field or pressing Enter. Newline, Tab, and other control characters in the text are rejected. `input_mode` and `sensitive` work as in the ordinary text tool.

After verifying the actual text in the screenshot, call `desktop_key_at` with a new `action_id`, the returned `next_frame_id`, the target x/y, and, for example, `keys: ["Enter"]`. It restores foreground within the same call, rechecks the scene, clicks the target, and sends the chord. The normal path for querying an IP in an existing WSL terminal becomes: initial observation → `desktop_fill` with `hostname -I` → inspect the echoed command → `desktop_key_at` with Enter and read the after-image.

Both tools accept `settle_ms` (default 250, range 0–5000) to wait between injection and the after-image, returning only the final screenshot. Waiting does not establish content correctness; slow connections may still require `desktop_wait`. Each composite has one action ID and audit intent. Partial execution pauses control rather than automatically repeating the whole operation. Individual click, text, and key tools remain available for tasks requiring intermediate inspection.

Windows scan-code text input has no fixed 10 ms delay per character; cancellation and focus checks still run for every character. Ordinary keys and chords also use scan codes, with a virtual-key fallback for special keys, to improve remote-client forwarding compatibility. A shorter screenshot delay does not make remote commands finish sooner; verify the actual output.

The `injected`, `rejected`, and `uncertain` statuses describe local execution, not business success. Partial input with an uncertain result pauses control automatically; inspect the remote state before resuming. Reusing an `action_id` with identical arguments returns the existing result without reinjection. Reusing it with different arguments causes a conflict. Failed observation or input preflight discards the old frame, requiring a new observation.

### Troubleshooting Sunlogin and other clients

After sending an instruction in the terminal, have the agent call `desktop_observe` directly. While running, the program attempts to activate the bound window before capturing it, so you do not need to select it manually each time. If normal activation is refused, it sends one Alt press/release to the current foreground application and retries activation; no task text or clicks are sent to the terminal. Recovering foreground clears keyboard focus verification, so the agent must click remote content again before typing. Paused observations do not activate windows. Input preflight and per-character input checks do not repeatedly reclaim lost foreground.

Sunlogin/AweSun may expose its `FlutterMultiWindow` wrapper and `ORAY_FLUTTER_VIEW_CONTAINER` view as separate top-level windows. They are paired only when process identity, executable, title, and physical client rectangle match exactly and the match is unique in both directions. Activation, capture, and input surfaces are tracked separately. Changes to pairing, identity, or geometry invalidate old frames. Sharing the AweSun process alone never authorizes input to its main window or another session.

Only if automatic activation fails should the agent request manual selection and call `desktop_focus` with the current `session_id` and `timeout_ms: 30000` (maximum 60000). Select the remote window while it waits, **without returning to the terminal to say done**. The tool continues automatically and returns a new screenshot. Windows permissions, menus, or desktop state can still prevent activation; this does not guarantee unattended foreground recovery in every environment.

Focus waiting succeeds only after activation and a new screenshot both succeed. Losing foreground immediately after activation retries within the remaining timeout. The Alt fallback is scoped to each explicit request, so later requests can try again rather than retaining a terminal's failure indefinitely. `window_diagnostics` in status and `details` in foreground errors report the actual foreground, keyboard focus, parent/owner relationships, and resolved surface roles.

Run read-only diagnostics even while the MCP server owns the controller lease, without starting another controller, activating a window, or injecting input:

```powershell
cargo run --locked -- diagnose --window win:e0fda
```

Replace the example with an actual window ID. For an off-screen viewport, errors include `content_screen`, `client_screen`, and `virtual_screen` to help calibration. The program does not silently crop and change coordinate semantics.

Automatic activation occurs only for an explicit observation or the first sample of a wait. Later wait samples and action after-images do not keep activating the window. If you switch applications then, the tool reports lost foreground or a capture failure without replaying the action. This still does not provide background parallel control: the agent uses the mouse and keyboard on the same desktop. For concurrent work, place both the agent and remote client in an isolated desktop environment, such as a virtual machine. Moving the window to another monitor or virtual desktop does not isolate global input.

`desktop_type_text` defaults to `input_mode: "auto"`: an entirely printable ASCII string uses `scan_code`; other strings use `unicode` for the entire request. Commands such as `ls` and `dir /b` therefore use scan codes without an explicit mode. Chinese or mixed-language strings still use `KEYEVENTF_UNICODE`, whose forwarding depends on the remote client. You can explicitly select `unicode` or `scan_code`. Scan-code input maps a physical US keyboard and requires English input, a US layout, and CapsLock off remotely. Explicit `scan_code` rejects the entire request for newline, Tab, Chinese, or other unsupported characters. Send Enter/Tab separately with the key tool. The program does not switch channels and retry after sending, or use the clipboard, avoiding duplicate input.

Text action results report the selected `scan_code` or `unicode` in `text_input_mode`, which is also persisted in the audit log. Event counts do not identify the channel: ASCII without Shift can produce two events per character in either mode. Check `server.process_id` and `server.default_text_input_mode` in `desktop_get_status` to identify the running service; the new default should be `auto`. Older audit records remain queryable. If retrying an action across an upgrade produces an ID conflict because the mode changed, query its existing outcome before deciding what to do; do not blindly submit it under a new ID.

Each frame includes `frame.image_path`, an absolute path to the local PNG, so you can check whether the agent read the screen correctly. Calibrate the viewport from the full client area rather than copying example dimensions. Use an existing remote terminal when available, and verify the typed command is visible before pressing Enter.

The protocol layer implements stdio, initialization, ping, tool discovery and calls, inline PNGs, and request cancellation. It negotiates the tools subset of protocol versions `2024-11-05`, `2025-03-26`, `2025-06-18`, and `2025-11-25`. HTTP, resources, sampling, and other extensions are not advertised. The current implementation uses a small independent protocol layer rather than an agent-specific SDK. It can later migrate to the official Rust MCP SDK without changing platform interfaces.

## Platform extension interfaces

### Performance and scene checks

Intermediate `desktop_wait` samples capture and compare RGB buffers in memory. Only the final sample is encoded and saved as PNG. Input preflight still verifies geometry, control generation, and the fresh scene. Coordinate actions additionally compare a clipped 96×96 pixel neighborhood around the target and reject changes above 4%, preventing small target changes from disappearing in a viewport-wide average. Cursors and animations can cause conservative rejection; observe again rather than automatically replaying input.

`desktop_get_status.performance` exposes timings and call counts for the last 32 MCP requests in microseconds: `queue`, `activate`, `inspect`, `capture`, `scene_check`, `audit`, `inject`, `settle`, `encode_png`, `frame_write`, `response_encode`, and `transport_write`. `dispatch` and `action_total` include nested stages, so durations must not be added together. `inject` includes native input checks. Metrics contain no command text, exclude model reasoning time, and do not prove remote execution. Complete timings for the current status call become visible on the next request.

[src/platform/mod.rs](src/platform/mod.rs) defines shared interfaces without exposing HWND handles:

- `WindowBackend`: capabilities, permission status and request entry points, window enumeration, inspection, and activation.
- `CaptureBackend`: capture the calibrated content area from a window snapshot and return tightly packed RGB8 pixels.
- `InputBackend`: execute mapped actions and repeat native focus and hit checks before injection.
- `OcrProvider`: an optional interface for text recognition, confidence scores, and bounding boxes.

The Windows entry point is [src/platform/windows.rs](src/platform/windows.rs), with window resolution/activation, capture, and input split into `src/platform/windows/window.rs`, `capture.rs`, and `input.rs`. Pure client policy lives in `src/platform/clients.rs`, recognizing AweSun and RustDesk session classes together with their executables. `window_diagnostics.target.client_profile` reports policy and a suggested echo delay; hints never override `settle_ms` automatically. Shared session, frame, cancellation, audit, coordinate, and MCP logic does not depend on Win32. Future macOS support can use native capture and input APIs; Linux requires separate X11 and Wayland adapters.

## Data and limitations

On Windows, data defaults to `%LOCALAPPDATA%\remote-window-bridge\data`. Override it with `REMOTE_WINDOW_BRIDGE_DATA_DIR` or `--data-dir`. For protocol development on other platforms, the default is `data` in the current directory. Use separate data directories for different Windows login sessions: the JSONL audit implementation does not support concurrent writers.

```text
data/
  audit.jsonl                              # Active audit segment
  audit.jsonl.segment-00000000000000000001.jsonl # Archived audit segment
  frames/*.png                            # Published content-area images
```

Screenshots default to limits of 256 files, 256 MiB, and 24 hours. Cleanup runs on the first publication and every 32 publications. The current screenshot and files younger than 120 seconds are protected, making these soft limits. Cleanup only removes regular PNG files bearing bridge-generated names in the dedicated directory, leaving unrelated files and symlinks alone. Inspect policy and cleanup errors in `desktop_get_status.storage`; embedding callers can adjust `frame_store.retention`.

Audit logs rotate before the next record after reaching 16 MiB. Historical segments are never automatically deleted. Memory caches 512 actions; older actions are located through a temporary disk index. The index disappears when the process exits and is rebuilt by validating all logs at startup, so startup time still grows with history. Logs remain the sole durable source of truth: preserve all segments together. `sensitive=true` redacts plaintext from text-input logs, but screenshots can still contain that text. Storage encryption is not implemented. Missing intermediate segments or corrupt/truncated records prevent startup; inspect uncertain actions before repair, and never delete history then automatically replay input.

Not implemented in this release: WGC/PrintWindow background capture, an OCR engine, a Tauri GUI, dragging, client-private protocol operations, a separate broker or IPC bridge, HTTP transport, and automatic detection of physical keyboard or mouse takeover within the same window.

BitBlt capture requires a foreground, visible, unobstructed window. Conservative occlusion checks may reject transparent overlays. SendInput injects system-wide input and cannot eliminate every focus race; do not operate the desktop concurrently with the agent. The program does not bypass UAC secure desktops, lock screens, or integrity-level restrictions. Switching remote tabs without changing the HWND cannot be detected reliably; pause and reattach after switching. Text input, system shortcut forwarding, and each remote client's behavior require testing on Windows.

## Development and running locally

### Set up the environment

On Windows, install Rust with Cargo using the `stable-x86_64-pc-windows-msvc` toolchain, Visual Studio C++ Build Tools, and the Windows SDK. Reopen your terminal after installation and check:

```powershell
rustc --version
cargo --version
```

The minimum Rust version is 1.83; stable is recommended for everyday development. Clone the project and enter the repository root:

```powershell
git clone https://github.com/felix-186/remote-window-bridge.git
cd remote-window-bridge
cargo build --locked
```

The debug executable is `target/debug/remote-window-bridge.exe`; the release executable is `target/release/remote-window-bridge.exe`. The Rust library import name is `remote_window_bridge`.

### Debug the CLI locally

```powershell
cargo run --locked -- --help
cargo run --locked -- doctor
cargo run --locked -- windows
cargo run --locked -- capture --window win:a1234 --delay-ms 3000 --output preview.png --data-dir .local-data
```

Arguments after `--` go to the program. Connect RDP, ToDesk, or Sunlogin locally first, then replace `win:a1234` with an actual window ID from `windows`. `doctor` reports platform capabilities; it does not connect to the remote computer or perform acceptance testing. `capture` waits 3000ms by default; `--delay-ms` accepts 0–60000ms. Switch to the target window during the countdown. Omitting the viewport captures the entire client area; inspect the image, then use `--viewport x,y,width,height` to exclude local UI.

Open [examples/remote-test-page.html](examples/remote-test-page.html) in the remote browser to check clicks, text input, and keyboard shortcuts.

### Debug the MCP server

Build the debug executable, then let your agent launch it directly. This Codex configuration assumes the repository is at `C:\work\code\GitHub\remote-window-bridge`; replace it with your actual absolute path:

```toml
[mcp_servers.remote_desktop]
command = 'C:\work\code\GitHub\remote-window-bridge\target\debug\remote-window-bridge.exe'
args = ['mcp', '--transport', 'stdio', '--data-dir', 'C:\work\code\GitHub\remote-window-bridge\.local-data']
```

After changing Rust code, stop the agent's MCP server, run `cargo build --locked` again, and restart the server to load the new executable. You can also check startup in a terminal with the following command. The server waits for newline-delimited MCP JSON-RPC messages on stdin, writes only protocol messages to stdout, and reports errors on stderr. It is not an interactive command prompt.

```powershell
cargo run --locked -- mcp --transport stdio --data-dir .local-data
```

`--data-dir` applies only to `capture` and `mcp`. It takes precedence over `REMOTE_WINDOW_BRIDGE_DATA_DIR`, followed by the default directory. Relative paths resolve from the process working directory, so use absolute paths in agent configuration. The development directory `.local-data/` is ignored by Git.

### Development checks

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo check --locked --features windows-api-check
```

After installing Rust on macOS/Linux, you can run the build and check commands above, `doctor`, and the MCP protocol server. Window enumeration, capture, and input return `PLATFORM_UNSUPPORTED`. The last command type-checks native Windows code on a non-Windows development host. **It does not verify Windows linking or real desktop behavior.** The feature is for development, not release builds.

This README and the implementation define the current release's scope.

## License

This project is licensed under the [MIT License](LICENSE).
