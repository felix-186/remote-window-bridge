# remote-window-bridge

[简体中文](README.md) | [English](README.en.md)

Rust 编写的远程桌面控制工具。Codex、Claude Code 或其他 MCP Agent，通过本机已经连接的 RDP / ToDesk / 向日葵窗口观察、点击和输入；远端无需安装本项目服务。

当前版本：**0.1.0，Windows CLI + MCP stdio 初版**。macOS/Linux 可编译和测试公共内核，真实桌面后端尚未实现。构建、类型检查与测试不能替代 Windows 远程客户端的实机验收。

## 已实现

- 枚举和绑定窗口，验证窗口 ID、PID、进程创建时间与窗口类。
- 客户区 viewport 校准，物理像素/归一化坐标转换，支持负坐标副屏。
- Windows 可见屏幕区域捕获，仅返回绑定内容区；PNG 作为 MCP 图像结果返回。
- SendInput 单击/双击、左右中键、鼠标移动、横纵滚动、Unicode / ASCII 扫描码文字与组合键。
- 新截图请求自动尝试激活已绑定窗口；执行前检查活动会话/输入桌面、前台、遮挡、几何、截图时效与画面变化，键盘检查最近点击后的焦点。
- `Ctrl+Alt+F10` 人工暂停、`Ctrl+Alt+F11` 本机恢复；Agent 不能解除人工暂停。
- action_id 去重、持久化动作审计、崩溃后未知结果查询，不自动补发副作用动作。
- 同一 Windows 会话只允许一个控制进程，避免不同 Agent 同时抢键鼠。
- Windows DPI manifest。

不内置特定模型，不设置终端、删除等业务黑名单。任务规划和业务确认由用户与所选 Agent 决定。

## Windows 构建与首次截图

需要 Rust 1.83+ 和 Visual Studio C++ Build Tools / Windows SDK（MSVC 工具链）。

```powershell
cargo build --locked --release
.\target\release\remote-window-bridge.exe doctor
.\target\release\remote-window-bridge.exe windows
```

从 `windows` 输出获取远程窗口的 `identity.id`，替换下面的示意 ID：

```powershell
.\target\release\remote-window-bridge.exe capture --window win:a1234 --delay-ms 3000 --output preview.png
```

执行后有 3 秒切换到远程窗口。截图时目标必须前台、非最小化且无遮挡。检查图片，根据实际内容指定客户区内的 viewport，排除本地工具栏、黑边和侧栏：

```powershell
.\target\release\remote-window-bridge.exe capture --window win:a1234 --viewport 0,40,1280,720 --output preview.png
```

viewport 单位是物理像素，格式 `x,y,width,height`，示例尺寸不能直接套用。程序不负责建立远程连接或登录。

## 接入 Agent

将 exe 放到固定路径，例如 `C:\remote-window-bridge\remote-window-bridge.exe`。

Codex `config.toml`：

```toml
[mcp_servers.remote_desktop]
command = 'C:\remote-window-bridge\remote-window-bridge.exe'
args = ['mcp', '--transport', 'stdio']
```

Claude Code：

```powershell
claude.cmd mcp add --transport stdio --scope user remote_desktop -- C:\remote-window-bridge\remote-window-bridge.exe mcp --transport stdio
```

PowerShell 下若通过 npm 安装 Claude Code，使用 `claude.cmd`，避免 `claude.ps1` 吞掉参数分隔符 `--`，导致程序参数被 Claude 当成自身选项。在其他 shell 或原生安装中使用实际的 Claude 可执行命令。

其他 Agent 使用同一 stdio 命令。客户端需能读取 MCP image content 才能直接理解截图；其自身的工具授权设置仍生效。同一 Windows 会话不要同时启动两个控制进程。

推荐给 Agent 的说明：

> 使用 remote_desktop 工具。先列出窗口，绑定已连接的远程桌面并设置有效 viewport。focus 后检查实际返回的截图；若无法读取内联图片，用本机图片工具打开 frame.image_path，仍无法查看则停止视觉操作，不编造画面。每个新动作引用最新 frame_id 并使用唯一 action_id，同一会话按顺序调用，后续使用 next_frame_id，不并行发送依赖动作。injected 仅表示本机接受事件，必须核对执行后截图，确认文字出现后才按回车。ASCII 命令可使用 input_mode=scan_code，要求远端美式键盘、英文输入且 CapsLock 关闭。终端对话后直接调用 desktop_observe，由程序自动激活已绑定窗口；只有自动激活失败才用 desktop_focus 的 timeout_ms=30000 等待手动切窗，禁止要求点击后再回终端回复。不要每次输入前都 focus，它会清除键盘焦点验证。超时先查询 desktop_get_action，不盲目重复提交。人工暂停后等待用户在本机恢复。

首版 Agent 和执行端建议在同一台 Windows 本机运行。WSL、其他主机或云端容器不能默认访问本机 Windows 桌面。

## MCP 工具

| 类别 | 工具 |
| --- | --- |
| 窗口 | `desktop_list_windows`、`desktop_attach`、`desktop_detach`、`desktop_focus` |
| 观察 | `desktop_get_status`、`desktop_observe`、`desktop_wait` |
| 组合输入 | `desktop_fill`（点击并输入）、`desktop_key_at`（点击并发送按键） |
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

### 减少工具往返

常规输入优先用 `desktop_fill`：传入三个 ID、从最新截图确定的 x/y、`text`，一次调用内完成激活、原帧和新画面校验、点击、输入，并返回执行后截图。它只追加文字，不清空输入框，也不发送回车；不允许文字内包含换行或 Tab 等控制字符。`input_mode` 与 `sensitive` 的含义和普通文字工具一致。

确认截图中命令或文本正确后，用新的 `action_id`、返回的 `next_frame_id`、目标 x/y 调用 `desktop_key_at`，例如 `keys: ["Enter"]`。该调用会自行恢复前台、重新校验画面、点击目标并发送按键。因此“在已有 WSL 终端查询 IP”的常规路径可缩为：初始观察 → `desktop_fill` 输入 `hostname -I` → 人工或 Agent 核对回显 → `desktop_key_at` 回车并读取截图。

两个组合工具均支持 `settle_ms`（默认 250，范围 0–5000），用于注入后、截图前等待远端回显；每次只返回最终截图。等待时间不代表内容已经验证，慢连接仍可能需要 `desktop_wait`。组合动作只产生一个动作 ID 和审计意图；执行中部分事件已发送但失败时暂停，不能自动重复整个组合。单步点击、文字和按键工具仍可用于需要逐步核对的操作。

Windows 扫描码文字输入不再附加每字符 10 毫秒的固定等待，仍逐字符检查取消状态和输入焦点。普通按键和组合键也使用扫描码（特殊键保留虚拟键路径），改善远程客户端的按键转发兼容性。默认截图等待缩短不代表远端命令执行时间缩短；请根据实际回显验证结果。

状态 `injected`、`rejected`、`uncertain` 区分本机执行情况；业务成功由 Agent 核对。部分输入已发送而结果不确定时自动暂停，先检查远端状态再恢复。相同 action_id、相同参数仅返回已有结果；相同 ID 改参数返回冲突。观察或输入预检失败后会丢弃旧帧，必须重新观察。

### 向日葵等客户端的排障

在终端发送指令后，Agent 直接调用 `desktop_observe` 即可：运行状态下，程序会先尝试自动激活已绑定窗口，再截图，不需要每次手动切过去。普通激活被拒绝时，会向当前前台发送一次 Alt 按下/释放，再尝试激活；不会向终端发送任务文字或点击。恢复前台后会清除旧的键盘焦点验证，Agent 需要重新点击远端内容后输入。暂停状态不会自动切窗，输入预检或逐字输入中丢失前台也不会反复抢回窗口。

向日葵的 `FlutterMultiWindow` 外层窗口和 `ORAY_FLUTTER_VIEW_CONTAINER` 视图容器可作为独立顶层窗口出现。程序仅在进程身份、可执行文件、标题、物理客户区完全一致且双向匹配唯一时将它们配对，分别记录激活、捕获、输入窗口。窗口配对、身份或几何变化后，旧帧不能继续输入；不会因为同属 `AweSun.exe` 就允许向日葵主界面或另一个会话获得输入。

只有自动恢复失败后才需要手动选择窗口：Agent 调用 `desktop_focus`，传入当前 `session_id` 和 `timeout_ms: 30000`（最大 60000）。调用等待期间点击远程会话窗口，**不要再切回终端回复“好”**；工具自动继续并返回新截图。Windows 权限、菜单或桌面状态仍可能阻止激活，不能保证所有客户端和系统环境都无需人工介入。

焦点等待以“激活并取得新截图”成功为准；激活后立刻丢失前台会在剩余时间内重试。Alt 恢复限于每次显式请求，后续请求可重新尝试，不会永久缓存某个终端窗口的失败状态。`desktop_get_status` 的 `window_diagnostics` 及前台错误的 `details` 提供实际前台、键盘焦点、父/所有者窗口与解析后的窗口角色。

也可以在 MCP 运行期间执行只读诊断，不会启动第二个控制进程、抢焦点或注入输入：

```powershell
cargo run --locked -- diagnose --window win:e0fda
```

用实际窗口 ID 替换示例。窗口未完全位于屏幕内时，错误会返回 `content_screen`、`client_screen` 和 `virtual_screen`，便于校准 viewport；程序不会默默裁剪并改变坐标含义。

自动激活仅发生在显式观察请求或等待的首张截图。等待中的后续采样、动作后的截图不会反复激活窗口；如果此时切到其他应用，工具报告丢失前台或截图失败，不自动重发动作。但这仍不是后台并行控制：同一桌面的鼠标键盘会被 Agent 使用，不能同时稳定操作其他应用。需要并行工作时，应把 Agent 与远程客户端放在独立的桌面环境（例如虚拟机）中；仅移动到另一块显示器或虚拟桌面不能隔离全局输入。

`desktop_type_text` 默认 `input_mode: "auto"`：整段文本均为可打印 ASCII 时使用 `scan_code`，否则整段使用 `unicode`。因此 `ls`、`dir /b` 等命令无需指定模式就会走扫描码；中文或中英混合文本仍走 `KEYEVENTF_UNICODE`，是否转发取决于远程客户端。可显式指定 `unicode` 或 `scan_code`。扫描码按美式物理键盘映射，要求远端处于英文输入、美式布局且 CapsLock 关闭；显式 `scan_code` 下换行、Tab、中文等会整段拒绝，Enter/Tab 使用按键工具单独发送。程序不会在发送后自动换通道重试，也不使用剪贴板，避免重复输入。

文字动作结果的 `text_input_mode` 明确返回选中的 `scan_code` 或 `unicode`，该字段也会写入审计记录。不能用 `sent_events` 区分通道：不含 Shift 的 ASCII 在两种模式下都可能每字符两个事件。用 `desktop_get_status` 的 `server.process_id` 和 `server.default_text_input_mode` 检查正在运行的服务；新版默认模式应为 `auto`。旧审计记录仍可查询。升级前后重试同一动作若因模式变化出现 ID 冲突，应先查询旧动作结果，不能更换 ID 后盲目补发。

每张截图的 `frame.image_path` 是本机 PNG 的绝对路径，可用于检查 Agent 是否正确读取画面。不要照搬示例 viewport：先查看整个客户区，再校准实际内容区。远端已打开终端时直接使用，输入并确认命令可见后再按 Enter。

协议层实现 stdio、initialize、ping、工具发现/调用、内联 PNG 和运行中取消，协商 `2024-11-05`、`2025-03-26`、`2025-06-18`、`2025-11-25` 的工具子集。没有声明 HTTP、resources、sampling 等扩展。当前使用小型独立协议层，不依赖 Agent 私有 SDK；后续可迁移官方 Rust MCP SDK，平台接口不变。

## 平台扩展

### 性能与画面校验

`desktop_wait` 的中间采样仅捕获 RGB 并在内存中比较，结束时才编码和保存一张 PNG。输入前仍重新捕获并检查几何、控制代次和场景；带坐标的动作还检查操作点周围的 96×96 像素区域（边界处裁剪，变化比例超过 4% 时拒绝），避免小目标变化被整张截图稀释。光标、动画可能导致保守拒绝，此时重新观察，不自动重放动作。

`desktop_get_status.performance` 返回最近 32 次 MCP 请求的分阶段耗时和调用次数，单位为微秒：`queue`、`activate`、`inspect`、`capture`、`scene_check`、`audit`、`inject`、`settle`、`encode_png`、`frame_write`、`response_encode` 和 `transport_write`。`dispatch`、`action_total` 包含内部阶段，不能相加；`inject` 包括原生输入前检查。记录不包含命令文字；不衡量模型思考时间，也不能单独证明远端已执行成功。当前状态请求的完整耗时在下一次请求中可见。

[src/platform/mod.rs](src/platform/mod.rs) 定义不暴露 HWND 的公共接口：

- `WindowBackend`：平台能力、权限状态/申请入口、窗口枚举、检查与激活。
- `CaptureBackend`：按快照捕获有效内容区，返回紧凑 RGB8。
- `InputBackend`：执行转换后的动作，平台内再次检查焦点和命中。
- `OcrProvider`：可选文字、置信度和 bbox 识别接口。

Windows 后端入口位于 [src/platform/windows.rs](src/platform/windows.rs)，窗口解析与激活、捕获、输入分别位于 `src/platform/windows/window.rs`、`capture.rs`、`input.rs`。纯客户端策略位于 `src/platform/clients.rs`，按可执行文件和窗口类识别向日葵、RustDesk 会话；`window_diagnostics.target.client_profile` 提供策略和回显等待建议，建议值不会自动覆盖 `settle_ms`。会话、帧、取消、审计、坐标和 MCP 逻辑不依赖 Win32。后续 macOS 接入系统截图/输入 API；Linux 分别适配 X11、Wayland。

## 数据与限制

Windows 默认目录 `%LOCALAPPDATA%\remote-window-bridge\data`；通过 `REMOTE_WINDOW_BRIDGE_DATA_DIR` 或 `--data-dir` 修改。非 Windows 的协议开发默认使用当前目录 `data`。不同 Windows 登录会话应使用不同数据目录；当前 JSONL 审计不支持多进程共享写入。

```text
data/
  audit.jsonl                              # 当前审计段
  audit.jsonl.segment-00000000000000000001.jsonl # 归档审计段
  frames/*.png                            # 已发布的内容区图像
```

截图默认保留上限为 256 张、256 MiB、24 小时，首次发布及每 32 次发布清理一次。当前截图和最近 120 秒的截图受保护，所以上限为软限制；只清理专用目录内程序命名的普通 PNG 文件，不处理其他文件或符号链接。策略与清理错误可在 `desktop_get_status.storage` 查看，嵌入调用方可通过 `frame_store.retention` 调整。

审计达到 16 MiB 后在下一条记录前分段，不自动删除历史。内存仅缓存 512 个动作，旧动作从临时磁盘索引定位；索引在进程关闭后消失，重启时顺序校验全部日志并重建，因此启动耗时仍随历史增长。日志仍是唯一持久依据，必须一起保留全部审计段。`sensitive=true` 隐藏文字日志明文，截图仍可能包含内容；未实现存储加密。缺失中间段、损坏或截断的日志会阻止服务启动，应保留现场并检查未知动作，不能直接删除后自动补发。

本版尚未实现：WGC/PrintWindow 后台捕获、OCR 引擎、Tauri GUI、拖拽、客户端私有协议操作、独立 Broker/IPC、HTTP 和同窗口物理键鼠自动接管。

BitBlt 捕获要求窗口前台、可见、无遮挡；保守检测可能拒绝透明遮挡物。SendInput 是系统输入，不能消除所有焦点竞态；运行中不要与 Agent 同时操作。UAC 安全桌面、锁屏、权限不匹配不能自动跨越。HWND 不变的远程标签页切换不能通用识别，切换后暂停并重新绑定。文字、系统快捷键转发和各客户端行为需实机验证。

## 开发与运行

### 准备环境

Windows 安装 Rust（包含 Cargo，使用 `stable-x86_64-pc-windows-msvc` 工具链）和 Visual Studio C++ Build Tools / Windows SDK。安装后重新打开终端，确认以下命令可用：

```powershell
rustc --version
cargo --version
```

Rust 最低版本为 1.83，日常开发建议使用 stable。克隆项目并进入仓库根目录：

```powershell
git clone https://github.com/felix-186/remote-window-bridge.git
cd remote-window-bridge
cargo build --locked
```

调试程序位于 `target/debug/remote-window-bridge.exe`，发布程序位于 `target/release/remote-window-bridge.exe`。Rust 库的导入名为 `remote_window_bridge`。

### 本地调试 CLI

```powershell
cargo run --locked -- --help
cargo run --locked -- doctor
cargo run --locked -- windows
cargo run --locked -- capture --window win:a1234 --delay-ms 3000 --output preview.png --data-dir .local-data
```

`--` 后面是程序参数。先在本机连接 RDP / ToDesk / 向日葵，再从 `windows` 输出取实际窗口 ID 替换 `win:a1234`。`doctor` 输出平台能力，不会自动连接远端或完成实机验收。`capture` 默认等待 3000ms，可用 `--delay-ms` 设置为 0–60000ms；倒计时内切到目标窗口。首次不传 viewport 会捕获整个客户区，检查图片后用 `--viewport x,y,width,height` 排除本地界面。

可在远端浏览器打开 [examples/remote-test-page.html](examples/remote-test-page.html)，检查点击、文字输入和快捷键结果。

### 调试 MCP 服务

先构建调试程序，再让 Agent 直接启动它。以下 Codex 配置假设仓库位于 `C:\work\code\GitHub\remote-window-bridge`，请改为实际绝对路径：

```toml
[mcp_servers.remote_desktop]
command = 'C:\work\code\GitHub\remote-window-bridge\target\debug\remote-window-bridge.exe'
args = ['mcp', '--transport', 'stdio', '--data-dir', 'C:\work\code\GitHub\remote-window-bridge\.local-data']
```

修改 Rust 代码后停止 Agent 的 MCP 服务，重新执行 `cargo build --locked`，再重启服务加载新程序。也可以在终端执行下列命令检查启动；服务等待客户端从 stdin 发送 MCP JSON-RPC 消息，每条一行，stdout 只输出协议消息，stderr 输出错误。它不是交互式命令行。

```powershell
cargo run --locked -- mcp --transport stdio --data-dir .local-data
```

`--data-dir` 仅适用于 `capture` 和 `mcp`，优先级高于环境变量 `REMOTE_WINDOW_BRIDGE_DATA_DIR`，再使用默认目录。相对路径以进程工作目录为准，因此 Agent 配置建议使用绝对路径。开发数据目录 `.local-data/` 已被 Git 忽略。

### 开发检查

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo check --locked --features windows-api-check
```

macOS/Linux 安装 Rust 后可运行上述构建和检查命令，以及 `doctor` 和 MCP 协议服务；窗口枚举、截图和输入会返回 `PLATFORM_UNSUPPORTED`。最后一条允许非 Windows 主机类型检查原生代码，**不等于 Windows 链接或实机验收**。该开发特性不用于发布。

本版范围以本 README 和实现为准。

## 许可证

本项目采用 [MIT License](LICENSE)。
