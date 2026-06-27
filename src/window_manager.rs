use std::path::Path;
use std::sync::OnceLock;

fn own_process_id() -> u32 {
    static PID: OnceLock<u32> = OnceLock::new();
    *PID.get_or_init(std::process::id)
}

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        System::{Threading::*,},
        UI::{WindowsAndMessaging::*,},
    },
};

/// 窗口信息
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowInfo {
    /// 窗口句柄（用 isize 存储以跨线程传递）
    pub hwnd: isize,
    /// 进程 ID
    pub process_id: u32,
    /// 进程名（从完整路径提取）
    pub process_name: String,
    /// 窗口标题
    pub window_title: String,
    /// 窗口类名
    pub class_name: String,
    /// 是否需要提升权限（进程名获取失败时）
    pub is_elevated: bool,
}

impl WindowInfo {
    pub fn id(&self) -> String {
        format!("{}_{:016x}", self.process_id, self.hwnd as usize)
    }

    pub fn display_title(&self) -> &str {
        if self.window_title.is_empty() {
            "无标题窗口"
        } else {
            &self.window_title
        }
    }

    pub fn display_process(&self) -> &str {
        if self.process_name.is_empty() {
            if self.is_elevated {
                "[需要管理员权限]"
            } else {
                "[未知进程]"
            }
        } else {
            &self.process_name
        }
    }
}

/// 窗口管理器：封装 Windows API 枚举与激活逻辑
pub struct WindowManager;

impl WindowManager {
    pub fn new() -> Self {
        Self
    }

    /// 枚举当前所有可见的顶层窗口
    pub fn enumerate_windows(&self) -> Vec<WindowInfo> {
        let mut list: Vec<WindowInfo> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_windows_proc), LPARAM(&mut list as *mut _ as isize));
        }
        list.sort_by(|a, b| a.window_title.to_lowercase().cmp(&b.window_title.to_lowercase()));
        list
    }

    /// 激活指定窗口（若最小化则恢复）
    pub fn activate_window(hwnd: isize) {
        let hwnd = HWND(hwnd as *mut std::ffi::c_void);
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 判断窗口类名是否应被排除
fn is_excluded_class_name(class_name: &str) -> bool {
    const EXCLUDED: &[&str] = &[
        "Windows.UI.Core.CoreWindow",
        "ApplicationFrameWindow",
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "Progman",
        "WorkerW",
        "Windows.UI.Composition.DesktopWindowContentBridge",
    ];
    EXCLUDED.iter().any(|&excluded| excluded.eq_ignore_ascii_case(class_name))
}

/// 获取窗口标题
unsafe fn get_window_title(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd);
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let copied = GetWindowTextW(hwnd, &mut buf);
    if copied == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied as usize])
}

/// 获取窗口类名
unsafe fn get_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buf);
    if len == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// 获取进程名（优先使用 QueryFullProcessImageNameW，只需 LIMITED 权限）
unsafe fn get_process_name(process_id: u32) -> (String, bool) {
    let access = PROCESS_QUERY_LIMITED_INFORMATION;
    let handle = OpenProcess(access, false, process_id);
    let mut elevated = false;

    let process_name = match handle {
        Ok(handle) if !handle.is_invalid() => {
            let mut buf = [0u16; 512];
            let mut size = buf.len() as u32;
            let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
            let _ = CloseHandle(handle);
            if result.is_ok() && size > 0 {
                let path = String::from_utf16_lossy(&buf[..size as usize]);
                Path::new(&path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                elevated = true;
                String::new()
            }
        }
        _ => {
            elevated = true;
            String::new()
        }
    };

    (process_name, elevated)
}

/// EnumWindows 回调函数
unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<WindowInfo>);

    // 只取可见窗口
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    // 排除工具窗口
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if (ex_style & WS_EX_TOOLWINDOW.0) != 0 {
        return TRUE;
    }

    // 排除具有所有者的窗口（通常是弹窗/对话框）
    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        if !owner.is_invalid() {
            return TRUE;
        }
    }

    // 排除子窗口
    if let Ok(parent) = GetParent(hwnd) {
        if !parent.is_invalid() {
            return TRUE;
        }
    }

    let class_name = get_class_name(hwnd);
    if is_excluded_class_name(&class_name) {
        return TRUE;
    }

    let window_title = get_window_title(hwnd);
    if window_title.is_empty() {
        return TRUE;
    }

    let mut process_id: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    // 排除自身进程，避免自己的窗口出现在列表里
    if process_id == own_process_id() {
        return TRUE;
    }

    let (process_name, is_elevated) = if process_id != 0 {
        get_process_name(process_id)
    } else {
        (String::new(), true)
    };

    list.push(WindowInfo {
        hwnd: hwnd.0 as isize,
        process_id,
        process_name,
        window_title,
        class_name,
        is_elevated,
    });

    TRUE
}
