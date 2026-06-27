use std::time::Duration;

use global_hotkey::{
    hotkey::HotKey,
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use winit::event_loop::EventLoopProxy;

use crate::{AppEvent, UserEvent};

/// 启动全局热键监听（创建管理器 + 设置事件处理器 + 注册初始热键）
/// 使用 set_event_handler 让 winit 消息循环直接分发 WM_HOTKEY，避免独立线程问题
pub fn start_hotkey_thread(
    hotkey_str: &str,
    proxy: EventLoopProxy<UserEvent>,
) -> GlobalHotKeyManager {
    let manager = GlobalHotKeyManager::new().expect("Failed to create global hotkey manager");

    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed {
            let _ = proxy.send_event(UserEvent::App(AppEvent::ToggleVisibility));
            let _ = proxy.send_event(UserEvent::Redraw(Duration::ZERO));
        }
    }));

    if let Err(e) = manager.register(parse_hotkey(hotkey_str)) {
        eprintln!("[Muka-TaskSwitch] 注册初始热键 {} 失败: {}", hotkey_str, e);
    }
    manager
}

/// 重新注册热键：先注销旧热键，再注册新热键
pub fn register_hotkey(manager: &GlobalHotKeyManager, old_hotkey_str: &str, new_hotkey_str: &str) {
    let old_hotkey = parse_hotkey(old_hotkey_str);
    let new_hotkey = parse_hotkey(new_hotkey_str);

    let _ = manager.unregister_all(&[old_hotkey]);
    if let Err(e) = manager.register(new_hotkey) {
        eprintln!("[Muka-TaskSwitch] 注册热键 {} 失败: {}", new_hotkey_str, e);
    }
}

fn parse_hotkey(hotkey_str: &str) -> HotKey {
    let normalized = hotkey_str.replace(' ', "").to_lowercase();
    let parts: Vec<&str> = normalized.split('+').collect();

    let mut mods = global_hotkey::hotkey::Modifiers::empty();
    let mut key_str = "";

    for part in parts {
        match part {
            "ctrl" | "control" => mods |= global_hotkey::hotkey::Modifiers::CONTROL,
            "alt" => mods |= global_hotkey::hotkey::Modifiers::ALT,
            "shift" => mods |= global_hotkey::hotkey::Modifiers::SHIFT,
            "cmd" | "command" | "win" | "meta" => mods |= global_hotkey::hotkey::Modifiers::META,
            _ => key_str = part,
        }
    }

    let key = parse_key(key_str);
    HotKey::new(Some(mods), key)
}

fn parse_key(key_str: &str) -> global_hotkey::hotkey::Code {
    use global_hotkey::hotkey::Code;
    match key_str {
        "backquote" | "`" | "~" => Code::Backquote,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "space" => Code::Space,
        "esc" | "escape" => Code::Escape,
        "backspace" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => Code::Backquote,
    }
}
