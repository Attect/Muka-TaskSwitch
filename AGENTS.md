# Agent 指南

## 项目概述

`Muka-程序切换实用工具` 是一个 Windows 平台的窗口切换工具，使用 Rust + egui + winit + glow 实现。项目已经从 `eframe` 迁移到自定义 winit 事件循环，以获得对窗口创建、定位、显示/隐藏的完全控制。

## 关键文件

- `src/main.rs`：主程序入口、自定义 winit 事件循环 (`GlowApp`)、`TaskSwitchApp` 业务逻辑。
- `src/window_manager.rs`：Windows 窗口枚举、进程信息获取、窗口激活。
- `src/tray.rs`：系统托盘图标与菜单，通过 `EventLoopProxy<UserEvent>` 与主循环通信。
- `src/hotkey.rs`：全局热键，使用 `GlobalHotKeyEvent::set_event_handler` 直接由 winit 消息循环分发。
- `src/config.rs`：配置结构与持久化。

## 构建命令

```bash
# 开发
cargo run

# Release
cargo build --release
```

Release 产物：`target/release/muka-taskswitch.exe`

## 重要实现细节

1. **窗口创建与定位**
   - 使用 `GlutinWindowContext` 直接创建 winit 窗口和 OpenGL 上下文。
   - 窗口初始 `with_visible(false)`，然后通过 `WindowAttributes::with_position` 和 `set_outer_position` 定位到屏幕右侧，最后 `set_visible(true)`，避免左侧闪现。
   - 使用 `SetWindowPos` 精确设置物理像素尺寸，确保窗口撑满工作区高度。

2. **显示 / 隐藏**
   - 隐藏：调用 `window.set_visible(false)` 和 `ShowWindow(hwnd, SW_HIDE)`。
   - 显示：在 `user_event` 中直接调用 `position_window`，内部先 `set_visible(true)` 再 `SetWindowPos(... | SWP_SHOWWINDOW)`。

3. **任务栏**
   - 通过 `CoCreateInstance(ITaskbarList) + DeleteTab(hwnd)` 从任务栏移除自身按钮。
   - 在窗口创建后和每次显示后都会调用。

4. **圆角**
   - 使用 DWM：`DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND`。
   - 同时设置 `DWMWA_NCRENDERING_POLICY = DWMNCRP_DISABLED` 去除阴影残影。

5. **全局热键**
   - 不要启动独立接收线程；应使用 `GlobalHotKeyEvent::set_event_handler` 并在回调中通过 `EventLoopProxy` 发送事件。

6. **单实例**
   - 通过命名互斥体 `Muka-TaskSwitch-SingleInstance` 实现。

7. **中文字体**
   - 启动时尝试加载系统字体：`msyh.ttc`、`simsun.ttc`、`simhei.ttf` 等。

## 修改注意事项

- 不要重新引入 `eframe`，窗口创建逻辑已经替换为 winit + glutin。
- 托盘/热键事件统一使用 `UserEvent` / `AppEvent`，通过 `EventLoopProxy` 唤醒主循环。
- 退出时应将 `GlowApp.running` 置为 `false`，以便后台线程（托盘、窗口监控）及时退出。
- 涉及 Windows API 调用时，注意使用 `windows` crate 的类型转换（如 `HWND`、`RECT` 等）。

## 测试建议

修改后至少验证：

1. `cargo build` 通过。
2. Release 构建通过。
3. 启动无左侧残影。
4. 热键可显示/聚焦窗口。
5. 托盘左键/右键菜单正常。
6. 失去焦点自动隐藏正常（若开启）。
7. 退出后进程完全结束。
