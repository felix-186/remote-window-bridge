# AIRemoteControl

[简体中文](README.md) | [English](README.en.md)

Rust 编写的远程桌面控制工具。Codex、Claude Code 或其他 MCP Agent，通过本机已经连接的 RDP / ToDesk / 向日葵窗口观察、点击和输入；远端无需安装本项目服务。

当前版本：**Windows CLI + MCP stdio 初版**。macOS/Linux 可编译和测试公共内核，真实桌面后端尚未实现。当前开发环境为 macOS，Windows API 已进行类型检查，真实远程客户端行为仍需按 [Windows 验收步骤](docs/windows-validation.md) 测试。

## 已实现

- 枚举和绑定窗口，验证窗口 ID、PID、进程创建时间与窗口类。
- 客户区 viewport 校准，物理像素/归一化坐标转换，支持负坐标副屏。
- Windows 可见屏幕区域捕获，仅返回绑定内容区；PNG 作为 MCP 图像结果返回。
- SendInput 单击/双击、左右中键、鼠标移动、横纵滚动、Unicode 文字与组合键。
- 执行前检查活动会话/输入桌面、前台、遮挡、几何、截图时效与画面变化，键盘检查最近点击后的焦点。
- `Ctrl+Alt+F10` 人工暂停、`Ctrl+Alt+F11` 本机恢复；Agent 不能解除人工暂停。
- action_id 去重、持久化动作审计、崩溃后未知结果查询，不自动补发副作用动作。
- 同一 Windows 会话只允许一个控制进程，避免不同 Agent 同时抢键鼠。
- Windows DPI manifest，以及 Windows/macOS/Linux CI 构建配置。

不内置特定模型，不设置终端、删除等业务黑名单。任务规划和业务确认由用户与所选 Agent 决定。

## Windows 构建与首次截图

需要 Rust 1.83+ 和 Visual Studio C++ Build Tools / Windows SDK（MSVC 工具链）。

```powershell
cargo build --locked --release
.\target\release\airemotecontrol.exe doctor
.\target\release\airemotecontrol.exe windows
```

从 `windows` 输出获取远程窗口的 `identity.id`，替换下面的示意 ID：

```powershell
.\target\release\airemotecontrol.exe capture --window win:a1234 --delay-ms 3000 --output preview.png
```

执行后有 3 秒切换到远程窗口。截图时目标必须前台、非最小化且无遮挡。检查图片，根据实际内容指定客户区内的 viewport，排除本地工具栏、黑边和侧栏：

```powershell
.\target\release\airemotecontrol.exe capture --window win:a1234 --viewport 0,40,1280,720 --output preview.png
```

viewport 单位是物理像素，格式 `x,y,width,height`，示例尺寸不能直接套用。程序不负责建立远程连接或登录。

## 接入 Agent

将 exe 放到固定路径，例如 `C:\AIRemoteControl\airemotecontrol.exe`。

Codex `config.toml`：

```toml
[mcp_servers.remote_desktop]
command = 'C:\AIRemoteControl\airemotecontrol.exe'
args = ['mcp', '--transport', 'stdio']
```

Claude Code：

```powershell
claude mcp add --transport stdio remote_desktop -- C:\AIRemoteControl\airemotecontrol.exe mcp --transport stdio
```

其他 Agent 使用同一 stdio 命令。客户端需能读取 MCP image content 才能直接理解截图；其自身的工具授权设置仍生效。同一 Windows 会话不要同时启动两个控制进程。

推荐给 Agent 的说明：

> 使用 remote_desktop 工具。先列出窗口，绑定已连接的远程桌面并设置有效 viewport。focus 后观察截图，坐标以返回图像的标称尺寸为准。每个新动作引用最新 frame_id 并使用唯一 action_id。injected 仅表示本机接受事件，结合执行后截图核对。超时先查询 desktop_get_action，不盲目重复提交。人工暂停后等待用户在本机恢复。

首版 Agent 和执行端建议在同一台 Windows 本机运行。WSL、其他主机或云端容器不能默认访问本机 Windows 桌面。

## MCP 工具

| 类别 | 工具 |
| --- | --- |
| 窗口 | `desktop_list_windows`、`desktop_attach`、`desktop_detach`、`desktop_focus` |
| 观察 | `desktop_get_status`、`desktop_observe`、`desktop_wait` |
| 输入 | `desktop_click`、`desktop_move`、`desktop_scroll`、`desktop_type_text`、`desktop_press_key` |
| 控制与恢复 | `desktop_pause`、`desktop_resume`、`desktop_get_action` |

`desktop_click` 参数示例：

```json
{
  "session_id": "从 attach 获取",
  "frame_id": "从最新截图获取",
  "action_id": "task-01-step-01",
  "x": 640,
  "y": 360,
  "coordinate_space": "image_pixels",
  "button": "left",
  "click_count": 1
}
```

可选 `coordinate_space: "normalized"`，x/y 范围 `[0,1]`。文字、按键同样需要三个 ID，并且先点击远端内容/输入框。每次动作消耗旧截图，成功注入后通常附带新截图。截图默认有效期 120 秒，显著画面变化或几何变化会提前拒绝输入。

状态 `injected`、`rejected`、`uncertain` 区分本机执行情况；业务成功由 Agent 核对。部分输入已发送而结果不确定时自动暂停，先检查远端状态再恢复。相同 action_id、相同参数仅返回已有结果；相同 ID 改参数返回冲突。观察或输入预检失败后会丢弃旧帧，必须重新观察。

协议层实现 stdio、initialize、ping、工具发现/调用、内联 PNG 和运行中取消，协商 `2024-11-05`、`2025-03-26`、`2025-06-18`、`2025-11-25` 的工具子集。没有声明 HTTP、resources、sampling 等扩展。当前使用小型独立协议层，不依赖 Agent 私有 SDK；后续可迁移官方 Rust MCP SDK，平台接口不变。

## 平台扩展

[src/platform/mod.rs](src/platform/mod.rs) 定义不暴露 HWND 的公共接口：

- `WindowBackend`：平台能力、权限状态/申请入口、窗口枚举、检查与激活。
- `CaptureBackend`：按快照捕获有效内容区，返回紧凑 RGB8。
- `InputBackend`：执行转换后的动作，平台内再次检查焦点和命中。
- `OcrProvider`：可选文字、置信度和 bbox 识别接口。

Windows 后端独立位于 [src/platform/windows.rs](src/platform/windows.rs)。会话、帧、取消、审计、坐标和 MCP 逻辑不依赖 Win32。后续 macOS 接入系统截图/输入 API；Linux 分别适配 X11、Wayland。见 [平台接口说明](docs/platform-interfaces.md)。

## 数据与限制

Windows 默认目录 `%LOCALAPPDATA%\AIRemoteControl\data`；通过 `AIREMOTE_DATA_DIR` 或 `--data-dir` 修改。非 Windows 的协议开发默认使用当前目录 `data`。不同 Windows 登录会话应使用不同数据目录；当前 JSONL 审计不支持多进程共享写入。

```text
data/
  audit.jsonl     # 持久化动作意图、请求摘要、执行结果
  frames/*.png   # 每次观察的内容区图像
```

`sensitive=true` 隐藏文字日志明文，截图仍可能包含内容。尚未自动清理/加密存储。损坏或截断的审计日志会阻止继续输入，应保留现场并检查未知动作，不能直接删除后自动补发。

本版尚未实现：WGC/PrintWindow 后台捕获、OCR 引擎、Tauri GUI、拖拽、客户端专用适配、独立 Broker/IPC、HTTP 和同窗口物理键鼠自动接管。

BitBlt 捕获要求窗口前台、可见、无遮挡；保守检测可能拒绝透明遮挡物。SendInput 是系统输入，不能消除所有焦点竞态；运行中不要与 Agent 同时操作。UAC 安全桌面、锁屏、权限不匹配不能自动跨越。HWND 不变的远程标签页切换不能通用识别，切换后暂停并重新绑定。文字、系统快捷键转发和各客户端行为需实机验证。

## 开发验证

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo check --locked --features windows-api-check
```

最后一条允许非 Windows 主机类型检查原生代码，**不等于 Windows 链接或实机验收**。该开发特性不用于发布。CI 包含 Windows MSVC 构建，但仍需实际触发后检查结果。

长期架构见 [设计文档](docs/design.md)，本版范围以本 README 和实现为准。最近一次代码检查见 [2026-10-18 检查记录](docs/review-2026-10-18.md)。

## 许可证

本项目采用 [MIT License](LICENSE)。
