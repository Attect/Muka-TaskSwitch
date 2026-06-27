use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder, TrayIconEvent,
};
use winit::event_loop::EventLoopProxy;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
};

use crate::{AppEvent, UserEvent};

/// 启动系统托盘监听线程
pub fn start_tray_thread(proxy: EventLoopProxy<UserEvent>, running: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let icon = match create_tray_icon() {
            Ok(icon) => icon,
            Err(e) => {
                eprintln!("[Muka-TaskSwitch] 创建托盘图标失败: {}", e);
                return;
            }
        };

        let menu = Menu::new();
        let show_item = MenuItem::new("显示 Muka-程序切换实用工具", true, None);
        let quit_item = MenuItem::new("退出", true, None);
        let _ = menu.append(&show_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit_item);

        let tray_icon = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Muka-程序切换实用工具")
            .with_icon(icon)
            .build()
        {
            Ok(icon) => icon,
            Err(e) => {
                eprintln!("[Muka-TaskSwitch] 创建托盘图标失败: {}", e);
                return;
            }
        };

        // 关键：左键不弹出菜单，仅右键弹出菜单
        tray_icon.set_show_menu_on_left_click(false);
        tray_icon.set_show_menu_on_right_click(true);

        let menu_channel = MenuEvent::receiver();
        let tray_channel = TrayIconEvent::receiver();

        while running.load(Ordering::Relaxed) {
            // 处理菜单事件
            if let Ok(event) = menu_channel.try_recv() {
                if event.id == show_item.id() {
                    let _ = proxy.send_event(UserEvent::App(AppEvent::ShowWindow));
                    let _ = proxy.send_event(UserEvent::Redraw(Duration::ZERO));
                } else if event.id == quit_item.id() {
                    let _ = proxy.send_event(UserEvent::App(AppEvent::Exit));
                    break;
                }
            }

            // 处理托盘图标点击事件：左键/双击显示窗口
            if let Ok(event) = tray_channel.try_recv() {
                match event {
                    TrayIconEvent::Click { button, .. } => {
                        use tray_icon::MouseButton;
                        if matches!(button, MouseButton::Left) {
                            let _ = proxy.send_event(UserEvent::App(AppEvent::ShowWindow));
                            let _ = proxy.send_event(UserEvent::Redraw(Duration::ZERO));
                        }
                    }
                    TrayIconEvent::DoubleClick { .. } => {
                        let _ = proxy.send_event(UserEvent::App(AppEvent::ShowWindow));
                        let _ = proxy.send_event(UserEvent::Redraw(Duration::ZERO));
                    }
                    _ => {}
                }
            }

            // Windows 消息泵：必须运行，否则托盘事件不会被分发
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    });
}

fn create_tray_icon() -> Result<tray_icon::Icon, tray_icon::BadIcon> {
    // 32x32 橙色圆形图标，不透明背景
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];
    let cx = (size - 1) as f32 / 2.0;
    let cy = (size - 1) as f32 / 2.0;
    let radius = size as f32 / 2.0 - 1.0;
    let bg = [255u8, 140, 0, 255]; // 深橙色
    let fg = [255u8, 255, 255, 255]; // 白色 "M" 形状

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let idx = (y * size + x) * 4;
            if dx * dx + dy * dy <= radius * radius {
                let in_letter = (8..=23).contains(&x)
                    && (8..=23).contains(&y)
                    && (x == 8 || x == 23 || x == y || x + y == size - 1);
                let color = if in_letter { fg } else { bg };
                rgba[idx] = color[0];
                rgba[idx + 1] = color[1];
                rgba[idx + 2] = color[2];
                rgba[idx + 3] = color[3];
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, size as u32, size as u32)
}
