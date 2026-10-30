# LocalPrivacyScreen Rust

基于 Rust + Tauri 2 的独立 Windows 本地隐私屏，用于本地执行任务不能锁屏、或使用 Codex 等工具控制本机执行任务时遮挡屏幕、屏蔽本机误触。Rust 后端负责注册表设置、自定义背景绘制、多显示器遮罩和物理键鼠拦截；配置界面使用 HTML/CSS，通过系统 WebView2 渲染。

## 使用

1. 解压发布包并运行 `LocalPrivacyScreen.exe`。
2. 在界面中录制退出快捷键、选择背景图并设置铺图方式。
3. 点击“启动隐私屏”。本机显示隐私画面并屏蔽物理键鼠。`DuGJRemoteDesk` 的键盘、鼠标和滚轮输入通过私有标记始终放行；截图和远程画面不受影响。
4. 按下配置的退出快捷键后，隐私屏关闭并返回配置界面。

设置保存在 `HKCU\\Software\\LocalPrivacyScreenRust`，不会生成配置文件。

运行日志保存在 `%LOCALAPPDATA%\LocalPrivacyScreenRust\logs\privacy.log`，可从界面底部的“打开运行日志”直接查看。日志记录程序启动和退出、隐私会话开始和结束、错误及键鼠输入来源计数，不记录按键内容、鼠标坐标或完整背景路径。日志达到 2 MB 时自动轮转，并保留两个历史文件。

## 远程输入说明

Windows 的 `LLKHF_INJECTED` / `LLMHF_INJECTED` 只表示事件经过可识别的用户态注入，不表示事件来自远程桌面。`DuGJRemoteDesk` 会给自家注入事件写入固定的 `dwExtraInfo` 标记，隐私屏同时依据该标记放行。

驱动型第三方远控（例如通过虚拟 HID 投递键鼠）可能不保留上述标志。此类事件与本地物理设备输入在低级钩子中不可区分；当前严格模式会拦截它们，避免将底层虚拟设备误当成允许的输入。

### 已验证环境

- 网易 UU 远程 `4.44.0.3919`：键盘、鼠标均可正常操作隐私屏下方的应用程序。
- `DuGJRemoteDesk`：键盘、鼠标和滚轮通过私有标记放行。

其他第三方远控仍需按其实际输入通道单独验证。

## 构建

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\build.ps1
```

输出：

- `target/release/local-privacy-screen.exe`
- `dist/LocalPrivacyScreen/LocalPrivacyScreen.exe`
- `dist/LocalPrivacyScreen.zip`
