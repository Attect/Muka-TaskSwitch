use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterType {
    #[default]
    All,
    ProcessName,
    WindowTitle,
}

impl FilterType {
    pub fn label(&self) -> &'static str {
        match self {
            FilterType::All => "全部",
            FilterType::ProcessName => "进程名",
            FilterType::WindowTitle => "窗口标题",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

impl Theme {
    pub fn label(&self) -> &'static str {
        match self {
            Theme::System => "跟随系统",
            Theme::Dark => "深色",
            Theme::Light => "浅色",
        }
    }

    pub fn to_egui_visuals(&self, system_dark: bool) -> egui::Visuals {
        match self {
            Theme::Dark => egui::Visuals::dark(),
            Theme::Light => egui::Visuals::light(),
            Theme::System => {
                if system_dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// 全局热键字符串，如 "Alt+Backquote"
    pub hotkey: String,
    /// 界面主题
    pub theme: Theme,
    /// 窗口宽度
    pub window_width: f32,
    /// 窗口高度
    pub window_height: f32,
    /// 是否置顶
    pub always_on_top: bool,
    /// 是否失去焦点自动隐藏
    pub hide_on_focus_loss: bool,
    /// 是否开机启动
    pub start_with_windows: bool,
    /// 默认过滤器类型
    pub filter_type: FilterType,
    /// 是否启用事件钩子实时刷新
    pub realtime_refresh: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Backquote".to_string(),
            theme: Theme::default(),
            window_width: 480.0,
            window_height: 720.0,
            always_on_top: true,
            hide_on_focus_loss: true,
            start_with_windows: false,
            filter_type: FilterType::default(),
            realtime_refresh: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        match Self::config_path() {
            Some(path) if path.exists() => {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str(&content) {
                        Ok(config) => config,
                        Err(e) => {
                            eprintln!("[Muka-TaskSwitch] 配置文件解析失败，使用默认配置: {}", e);
                            Self::default()
                        }
                    },
                    Err(e) => {
                        eprintln!("[Muka-TaskSwitch] 读取配置文件失败: {}", e);
                        Self::default()
                    }
                }
            }
            _ => Self::default(),
        }
    }

    pub fn save(&self) {
        if let Some(path) = Self::config_path() {
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("[Muka-TaskSwitch] 创建配置目录失败: {}", e);
                    return;
                }
            }
            match serde_json::to_string_pretty(self) {
                Ok(content) => {
                    if let Err(e) = std::fs::write(&path, content) {
                        eprintln!("[Muka-TaskSwitch] 写入配置文件失败: {}", e);
                    }
                }
                Err(e) => eprintln!("[Muka-TaskSwitch] 序列化配置失败: {}", e),
            }
        }
    }

    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "attect", "Muka-TaskSwitch")
            .map(|dirs| dirs.config_dir().join("config.json"))
    }

    /// 切换开机启动状态
    pub fn apply_startup(&self) {
        #[cfg(windows)]
        unsafe {
            use windows::{
                core::*,
                Win32::Foundation::ERROR_SUCCESS,
                Win32::System::Registry::*,
            };
            let hkcu = HKEY_CURRENT_USER;
            let subkey = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
            let value_name = w!("Muka-TaskSwitch");
            let mut key = HKEY::default();

            let open_result = RegOpenKeyExW(hkcu, subkey, None, KEY_WRITE, &mut key);
            if open_result != ERROR_SUCCESS {
                return;
            }

            if self.start_with_windows {
                if let Ok(exe) = std::env::current_exe() {
                    let exe_str = exe.to_string_lossy().to_string();
                    let wide: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(
                        wide.as_ptr() as *const u8,
                        wide.len() * 2,
                    );
                    let _ = RegSetValueExW(key, value_name, None, REG_SZ, Some(bytes));
                }
            } else {
                let _ = RegDeleteValueW(key, value_name);
            }
            let _ = RegCloseKey(key);
        }
    }
}
