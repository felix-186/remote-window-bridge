# 平台接口与适配约定

Windows 后端已实现；macOS/Linux 返回 `PLATFORM_UNSUPPORTED`，不会模拟成功。

## 公共边界

`WindowId(String)` 是平台不透明令牌，公共内核和 Agent 不解析 HWND、CGWindowID 或 XID。`WindowIdentity` 提供进程标识、创建时间和窗口类，用于识别窗口复用。

`WindowSnapshot` 包含客户区/内容区的本机物理像素矩形及 DPI。`ScreenPoint` 使用同一物理像素空间；原生 API 如果使用逻辑坐标，由后端在原生边界转换。不能给所有平台统一乘 DPI/96。

`RgbImage` 是紧凑 RGB8，从上到下，无 padding。当前捕获返回内容区物理尺寸。以后加入裁剪、缩放、旋转时扩展 `ImageTransform`，不能在 Agent 层猜测比例。

## 接口职责

| 接口 | 实现要求 |
| --- | --- |
| `WindowBackend` | 能力探测、权限状态/申请、窗口枚举、身份检查和显式激活。准确报告不支持的能力。 |
| `CaptureBackend` | 只捕获绑定内容区；前后检查取消、几何与绑定。可能永久阻塞的 API 需要可终止 Worker。 |
| `InputBackend` | 每个短输入批次检查取消、前台/命中/几何；部分注入需返回已发事件数；释放本程序按下的键。 |
| `OcrProvider` | 输出文字、置信度、相对输入图像的 bbox；由核心关联不可变帧。OCR 不能绕过窗口绑定。 |

权限申请入口供未来本地 UI 调用，不应自动绕过系统授权。Agent 无需了解平台原生 API。

## 并发、暂停与错误

`Control` 将代次与暂停来源放在一个原子值中。每个 `Cancellation` 固定创建时代次；暂停、恢复、取消、解绑和断开连接使旧 token 失效。暂停期间允许观察，不允许输入。

MCP 请求另有独立取消标记，在入队时注册，贯穿请求执行。即使取消早于执行/新 token 的创建，也必须保持已取消状态；不要通过重新创建 token 清除请求取消。窗口激活也接收同一个取消 token。

Windows 热键使用独立消息线程；MCP stdin 独立读取，运行中取消、暂停、EOF 不等长动作结束。已经进入 OS 队列的事件无法撤销。

当前 Windows 用命名内核对象实现单进程控制权。未来共享 Broker 时统一管理输入，不允许每个 MCP Bridge 各自执行键鼠。

截图变化阈值是启发式，无法保证识别每个小范围语义变化，也不是 UI 状态事务锁。相同 HWND 内部焦点变化可能不可见；高可靠运行推荐使用专用本机桌面。

## 后续平台实现

1. 新增平台模块，实现 Window/Capture/Input 三个后端，明确权限与热键能力。
2. 在 `platform::native` 选择实现，平台依赖按目标条件引入，公共 MCP 参数不变。
3. 运行公共坐标、取消、审计测试，补充该平台的窗口、遮挡和缩放集成测试。
4. 完成真实 MCP 图像→点击→远端反馈验收，再声明能力可用。

macOS 重点验证屏幕录制/辅助功能权限、Retina 点/像素以及截图和输入坐标映射。Linux 将 X11、Wayland 视为不同后端；Wayland 结合 Portal 和 compositor 实际能力，不能假设任意窗口枚举、激活或捕获总是可用。

## 当前与长期架构的差异

本版由一个 MCP 进程持有会话和独占输入权。独立 Broker、Named Pipe、可终止 PrintWindow Worker、Tauri UI、OCR 引擎、SQLite、HTTP 均属于后续扩展，不以接口存在表示已经实现。
