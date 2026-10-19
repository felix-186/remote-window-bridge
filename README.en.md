# remote-window-bridge

[简体中文](README.md) | [English](README.en.md)

A remote desktop control tool written in Rust. Codex, Claude Code, and other MCP agents can observe, click, and type through an **already-connected remote desktop window** on the local machine, such as RDP, ToDesk, or Sunlogin. No remote-window-bridge service needs to be installed on the remote computer.

Current release: **0.1.0, initial Windows CLI and MCP stdio implementation**. The shared core builds and runs tests on macOS and Linux, but native desktop backends for those platforms are not implemented. Builds, type checks, and tests do not replace acceptance testing with real remote desktop clients on Windows.

## Implemented features

- Window enumeration and binding, with checks for window ID, PID, process creation time, and window class.
- Client-area viewport calibration, physical pixel and normalized coordinate mapping, and secondary monitors with negative coordinates.
- Capture of the visible Windows screen region corresponding to the bound viewport, returned as inline PNG content through MCP.
- SendInput support for single and double clicks, left/right/middle buttons, pointer movement, vertical/horizontal scrolling, Unicode text, and key chords.
- Preflight checks for the active session and input desktop, foreground window, occlusion, geometry, frame age, and scene changes. Keyboard input also checks focus against the most recent content click.
- Local pause with `Ctrl+Alt+F10` and resume with `Ctrl+Alt+F11`. Agents cannot clear a user pause.
- Action deduplication using `action_id`, persistent auditing, and queries for uncertain results after a crash. Actions with side effects are not automatically replayed.
- One control process per Windows session to prevent agents from competing for input.
- A Windows DPI manifest and CI build configuration for Windows, macOS, and Linux.

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
claude mcp add --transport stdio remote_desktop -- C:\remote-window-bridge\remote-window-bridge.exe mcp --transport stdio
```

Other agents can use the same stdio command. Clients must support MCP image content to interpret screenshots directly; their own tool permissions still apply. Do not start two control processes in the same Windows session.

Suggested agent instructions:

> Use the remote_desktop tools. First list windows, bind the already-connected remote desktop, and set the correct viewport. Focus the window and observe its screenshot. Coordinates refer to the returned image's stated dimensions. Every new action must reference the latest frame_id and use a unique action_id. An injected result only means that the local OS accepted events; verify the remote outcome using the after-image. After a timeout, query desktop_get_action before repeating an action. If the user pauses control, wait for them to resume locally.

For this release, run the agent and executable on the same Windows machine. WSL, another host, or a cloud container cannot be assumed to have access to that machine's Windows desktop.

## MCP tools

| Category | Tools |
| --- | --- |
| Windows | `desktop_list_windows`, `desktop_attach`, `desktop_detach`, `desktop_focus` |
| Observation | `desktop_get_status`, `desktop_observe`, `desktop_wait` |
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

The `injected`, `rejected`, and `uncertain` statuses describe local execution, not business success. Partial input with an uncertain result pauses control automatically; inspect the remote state before resuming. Reusing an `action_id` with identical arguments returns the existing result without reinjection. Reusing it with different arguments causes a conflict. Failed observation or input preflight discards the old frame, requiring a new observation.

The protocol layer implements stdio, initialization, ping, tool discovery and calls, inline PNGs, and request cancellation. It negotiates the tools subset of protocol versions `2024-11-05`, `2025-03-26`, `2025-06-18`, and `2025-11-25`. HTTP, resources, sampling, and other extensions are not advertised. The current implementation uses a small independent protocol layer rather than an agent-specific SDK. It can later migrate to the official Rust MCP SDK without changing platform interfaces.

## Platform extension interfaces

[src/platform/mod.rs](src/platform/mod.rs) defines shared interfaces without exposing HWND handles:

- `WindowBackend`: capabilities, permission status and request entry points, window enumeration, inspection, and activation.
- `CaptureBackend`: capture the calibrated content area from a window snapshot and return tightly packed RGB8 pixels.
- `InputBackend`: execute mapped actions and repeat native focus and hit checks before injection.
- `OcrProvider`: an optional interface for text recognition, confidence scores, and bounding boxes.

The Windows implementation is isolated in [src/platform/windows.rs](src/platform/windows.rs). Shared session, frame, cancellation, audit, coordinate, and MCP logic does not depend on Win32. Future macOS support can use native capture and input APIs; Linux requires separate X11 and Wayland adapters.

## Data and limitations

On Windows, data defaults to `%LOCALAPPDATA%\remote-window-bridge\data`. Override it with `REMOTE_WINDOW_BRIDGE_DATA_DIR` or `--data-dir`. For protocol development on other platforms, the default is `data` in the current directory. Use separate data directories for different Windows login sessions: the JSONL audit implementation does not support concurrent writers.

```text
data/
  audit.jsonl     # Persistent action intents, request fingerprints, and outcomes
  frames/*.png   # Content-area images from observations
```

Setting `sensitive=true` redacts plaintext from text-input action logs, but screenshots may still contain that text. Automatic cleanup and storage encryption are not implemented. A corrupt or truncated audit log blocks further input. Preserve it and inspect uncertain actions before repair; do not delete the log and automatically replay those actions.

Not implemented in this release: WGC/PrintWindow background capture, an OCR engine, a Tauri GUI, dragging, client-specific adapters, a separate broker or IPC bridge, HTTP transport, and automatic detection of physical keyboard or mouse takeover within the same window.

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

After installing Rust on macOS/Linux, you can run the build and check commands above, `doctor`, and the MCP protocol server. Window enumeration, capture, and input return `PLATFORM_UNSUPPORTED`. The last command type-checks native Windows code on a non-Windows development host. **It does not verify Windows linking or real desktop behavior.** The feature is for development, not release builds. CI includes a Windows MSVC build and uploads the `remote-window-bridge-windows-x64` artifact; check the corresponding workflow run for actual results.

This README and the implementation define the current release's scope.

## License

This project is licensed under the [MIT License](LICENSE).
