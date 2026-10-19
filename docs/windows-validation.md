# Windows 实机验收

以下交互步骤尚未在当前 macOS 环境执行，CI 编译不能替代它们。

## 准备与闭环

1. Windows 执行 `cargo test --locked` 和 `cargo build --locked --release`。
2. 本机远程客户端连上测试远端，保持桌面解锁、窗口非最小化。
3. 远端浏览器打开 `examples/remote-test-page.html`，无需服务或网络。
4. 用 `doctor` 检查热键注册，随后退出该进程。用 `windows` 找到正确 ID，`capture` 确认内容区，再启动 Agent MCP 连接。
5. Agent list_windows / attach / focus / observe，确认收到真实图像和正确尺寸。
6. 点击九宫格，核对远端点击计数；不能只检查本机光标。
7. 点击输入框，输入 `Hello 远程电脑 123 😀`，核对实际内容。测试 Ctrl+A、方向键、Backspace、多行 Enter。
8. 测试双击、右键、滚轮与悬停，记录客户端版本，不把 RDP 结果等同于所有客户端。

## 异常矩阵

| 场景 | 预期 |
| --- | --- |
| 截图后移动/缩放窗口、跨屏 DPI 变化 | 旧请求被拒绝，重新观察 |
| 页面显著变化 | STALE_FRAME；微小语义变化可能漏检，记录限制 |
| 遮挡、最小化、失去前台 | 不继续捕获/输入，明确错误 |
| 相同 action_id 同参数重发 | 返回原结果，远端计数不增加 |
| 相同 action_id 改参数 | ACTION_ID_CONFLICT |
| 长输入/等待时 Ctrl+Alt+F10 | 本机停止后续业务输入，不等待 Agent |
| Agent 解除人工暂停 | PAUSED_BY_USER |
| Ctrl+Alt+F11 本机恢复 | 旧帧失效，重新观察和点击后才能键盘输入 |
| MCP stdin 关闭 | 取消执行，释放热键/输入互斥对象 |
| 同时启动两个进程 | 后者 LEASE_CONFLICT |
| 进程崩溃或动作回包丢失 | 查询持久化结果；未知为 uncertain，不自动重发 |
| 同窗口中焦点子 HWND 改变 | 键盘拒绝；同 HWND 内部变化不保证可检测 |

覆盖 100%/125%/150% DPI、负坐标副屏、远程缩放模式、中文和系统快捷键转发。记录截图耗时、暂停延迟、点击偏移与错误原因。

报告包含 Windows/客户端/Agent 版本、远端系统、显示布局、DPI、图像尺寸和日志位置。逐项填写通过、失败或未测，不把未测标成通过。
