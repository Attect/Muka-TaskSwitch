#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod hotkey;
mod tray;
mod window_manager;

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use config::{Config, FilterType, Theme};
use egui_winit::winit;
use global_hotkey::GlobalHotKeyManager;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize, PhysicalSize as WinitPhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    raw_window_handle::{HasWindowHandle as _, RawWindowHandle},
    window::{Window, WindowAttributes, WindowId, WindowLevel},
};
use window_manager::{WindowInfo, WindowManager};
use windows::core::w;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{
        DwmSetWindowAttribute, DWMNCRP_DISABLED, DWMNCRENDERINGPOLICY, DWMWCP_ROUND,
        DWMWA_NCRENDERING_POLICY, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_WINDOW_CORNER_PREFERENCE,
    },
    System::Com::{CoCreateInstance, CLSCTX_SERVER},
    UI::Shell::{ITaskbarList, TaskbarList},
    UI::WindowsAndMessaging::{
        GetForegroundWindow, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOOWNERZORDER, SWP_SHOWWINDOW, SW_HIDE,
    },
};

/// 跨线程事件
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// 切换窗口可见性
    ToggleVisibility,
    /// 显示窗口（用于托盘/热键，确保从隐藏状态恢复）
    ShowWindow,
    /// 刷新窗口列表
    RefreshWindows(Vec<WindowInfo>),
    /// 退出应用
    Exit,
}

/// 事件循环自定义事件
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// 来自托盘/热键/窗口监控的业务事件
    App(AppEvent),
    /// egui 请求重绘
    Redraw(Duration),
}

fn main() {
    setup_panic_hook();

    if !ensure_single_instance() {
        eprintln!("[Muka-程序切换实用工具] 已有一个实例在运行");
        std::process::exit(0);
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("Failed to create event loop");
    let proxy = event_loop.create_proxy();

    let config = Config::load();
    let running = Arc::new(AtomicBool::new(true));

    // 启动托盘
    tray::start_tray_thread(proxy.clone(), running.clone());

    // 启动全局热键
    let hotkey_manager = hotkey::start_hotkey_thread(&config.hotkey, proxy.clone());

    // 启动窗口监控线程
    start_window_monitor(proxy.clone(), config.realtime_refresh, running.clone());

    let app = TaskSwitchApp::new(config, WindowManager::new(), hotkey_manager);
    let mut glow_app = GlowApp::new(app, proxy, running);

    event_loop
        .run_app(&mut glow_app)
        .expect("Failed to run event loop");
}

/// OpenGL 窗口上下文
struct GlutinWindowContext {
    window: Window,
    gl_context: glutin::context::PossiblyCurrentContext,
    gl_display: glutin::display::Display,
    gl_surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
}

impl GlutinWindowContext {
    unsafe fn new(
        event_loop: &ActiveEventLoop,
        window_attributes: WindowAttributes,
    ) -> Option<Self> {
        use glutin::context::NotCurrentGlContext as _;
        use glutin::display::GetGlDisplay as _;
        use glutin::display::GlDisplay as _;
        use glutin::prelude::GlSurface as _;

        let config_template_builder = glutin::config::ConfigTemplateBuilder::new()
            .prefer_hardware_accelerated(None)
            .with_depth_size(0)
            .with_stencil_size(0)
            .with_transparency(true);

        let (mut window, gl_config) = glutin_winit::DisplayBuilder::new()
            .with_preference(glutin_winit::ApiPreference::FallbackEgl)
            .with_window_attributes(Some(window_attributes.clone()))
            .build(
                event_loop,
                config_template_builder,
                |mut configs| {
                    configs.next().expect("failed to find a matching glutin config")
                },
            )
            .ok()?;

        let gl_display = gl_config.display();

        let window = window.take().unwrap_or_else(|| {
            glutin_winit::finalize_window(event_loop, window_attributes.clone(), &gl_config)
                .expect("failed to finalize glutin window")
        });

        let raw_window_handle = window
            .window_handle()
            .expect("failed to get window handle")
            .as_raw();
        let context_attributes = glutin::context::ContextAttributesBuilder::new()
            .build(Some(raw_window_handle));
        let fallback_context_attributes = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::Gles(None))
            .build(Some(raw_window_handle));

        let not_current_gl_context = gl_display
            .create_context(&gl_config, &context_attributes)
            .unwrap_or_else(|_| {
                gl_display
                    .create_context(&gl_config, &fallback_context_attributes)
                    .expect("failed to create gl context even with fallback")
            });

        let (width, height): (u32, u32) = window.inner_size().into();
        let width = NonZeroU32::new(width).unwrap_or(NonZeroU32::MIN);
        let height = NonZeroU32::new(height).unwrap_or(NonZeroU32::MIN);
        let surface_attributes = glutin::surface::SurfaceAttributesBuilder::<
            glutin::surface::WindowSurface,
        >::new()
        .build(
            window
                .window_handle()
                .expect("failed to get window handle")
                .as_raw(),
            width,
            height,
        );
        let gl_surface = gl_display
            .create_window_surface(&gl_config, &surface_attributes)
            .expect("failed to create gl surface");
        let gl_context = not_current_gl_context
            .make_current(&gl_surface)
            .expect("failed to make gl context current");

        let _ = gl_surface.set_swap_interval(
            &gl_context,
            glutin::surface::SwapInterval::Wait(NonZeroU32::MIN),
        );

        Some(Self {
            window,
            gl_context,
            gl_display,
            gl_surface,
        })
    }

    fn window(&self) -> &Window {
        &self.window
    }

    fn resize(&self, physical_size: WinitPhysicalSize<u32>) {
        use glutin::surface::GlSurface as _;
        if physical_size.width == 0 || physical_size.height == 0 {
            return;
        }
        let width = NonZeroU32::new(physical_size.width).unwrap_or(NonZeroU32::MIN);
        let height = NonZeroU32::new(physical_size.height).unwrap_or(NonZeroU32::MIN);
        self.gl_surface.resize(&self.gl_context, width, height);
    }

    fn swap_buffers(&self) -> glutin::error::Result<()> {
        use glutin::surface::GlSurface as _;
        self.gl_surface.swap_buffers(&self.gl_context)
    }

    fn get_proc_address(&self, addr: &std::ffi::CStr) -> *const std::ffi::c_void {
        use glutin::display::GlDisplay as _;
        self.gl_display.get_proc_address(addr)
    }
}

struct GlowApp {
    app: TaskSwitchApp,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    running: Arc<AtomicBool>,
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<glow::Context>>,
    egui_glow: Option<egui_glow::EguiGlow>,
    repaint_delay: Duration,
}

impl GlowApp {
    fn new(
        app: TaskSwitchApp,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
        running: Arc<AtomicBool>,
    ) -> Self {
        Self {
            app,
            proxy,
            running,
            gl_window: None,
            gl: None,
            egui_glow: None,
            repaint_delay: Duration::MAX,
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) {
        let config = &self.app.config;

        // 用系统级 DPI 先计算右侧位置，让 winit 创建窗口时就位于右侧，
        // 避免 set_visible(true) 瞬间使用默认左侧位置
        let scale = system_scale_factor();
        let work_area = get_primary_work_area();
        let width = config.window_width;
        let height = work_area.height() / scale;
        let x = work_area.right() / scale - width;
        let y = work_area.top() / scale;

        let window_attributes = WindowAttributes::default()
            .with_title("Muka-程序切换实用工具")
            .with_inner_size(LogicalSize::new(width, height))
            .with_position(LogicalPosition::new(x, y))
            .with_visible(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);

        let gl_window = unsafe { GlutinWindowContext::new(event_loop, window_attributes) }
            .expect("failed to create window / gl context");
        let window = gl_window.window();

        // 再用实际 scale factor 精确校正一次
        let scale = window.scale_factor() as f32;
        let work_area = get_primary_work_area();
        let width = self.app.config.window_width;
        let height = work_area.height() / scale;
        let x = work_area.right() / scale - width;
        let y = work_area.top() / scale;
        window.set_outer_position(LogicalPosition::new(x, y));
        let _ = window.request_inner_size(LogicalSize::new(width, height));
        if let Some(hwnd) = hwnd_from_window(window) {
            apply_dwm_rounded_corners(hwnd);
        }
        window.set_visible(true);
        if let Some(hwnd) = hwnd_from_window(window) {
            remove_from_taskbar(hwnd);
        }

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                let s = std::ffi::CString::new(s)
                    .expect("failed to construct C string for gl proc address");
                gl_window.get_proc_address(&s)
            })
        };
        let gl = Arc::new(gl);

        let egui_glow = egui_glow::EguiGlow::new(event_loop, gl.clone(), None, None, true);

        let proxy = self.proxy.clone();
        egui_glow
            .egui_ctx
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::Redraw(info.delay));
            });

        // 加载中文字体
        setup_chinese_fonts(&egui_glow.egui_ctx);

        self.gl_window = Some(gl_window);
        self.gl = Some(gl);
        self.egui_glow = Some(egui_glow);

        // 初次显示后强制重绘一帧，确保立即定位
        if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for GlowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.create_window(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        use glow::HasContext as _;
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            self.running.store(false, Ordering::Relaxed);
            event_loop.exit();
            return;
        }

        if let WindowEvent::Resized(physical_size) = &event {
            if let Some(gl_window) = &self.gl_window {
                gl_window.resize(*physical_size);
            }
        }

        let response = self
            .egui_glow
            .as_mut()
            .unwrap()
            .on_window_event(self.gl_window.as_ref().unwrap().window(), &event);

        if response.repaint {
            if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                window.request_redraw();
            }
        }

        // 直接处理失焦隐藏，避免 egui_winit 未在焦点事件时触发重绘导致不隐藏
        if let WindowEvent::Focused(false) = &event {
            if self.app.config.hide_on_focus_loss && self.app.visible {
                if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                    self.app.hide_window(window);
                }
            }
        }
        if let WindowEvent::Focused(true) = &event {
            if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                window.request_redraw();
            }
        }

        if matches!(event, WindowEvent::RedrawRequested) {
            let gl_window = self.gl_window.as_ref().unwrap();
            let window = gl_window.window();
            let gl = self.gl.as_ref().unwrap();
            let egui_glow = self.egui_glow.as_mut().unwrap();

            egui_glow.run(window, |egui_ctx| {
                self.app.update(egui_ctx, window);
            });

            if self.app.exit_requested {
                event_loop.exit();
                return;
            }

            let clear = self.app.clear_color();
            unsafe {
                gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }

            egui_glow.paint(window);

            let _ = gl_window.swap_buffers();

            event_loop.set_control_flow(if self.repaint_delay.is_zero() {
                window.request_redraw();
                ControlFlow::Poll
            } else if let Some(instant) = Instant::now().checked_add(self.repaint_delay) {
                ControlFlow::WaitUntil(instant)
            } else {
                ControlFlow::Wait
            });
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::App(app_event) => match app_event {
                AppEvent::Exit => {
                    self.running.store(false, Ordering::Relaxed);
                    event_loop.exit();
                }
                AppEvent::ToggleVisibility => {
                    if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                        if self.app.visible {
                            // 已经显示时热键触发，直接重新聚焦，避免隐藏再显示的闪烁
                            self.app.refocus_window(window);
                        } else {
                            self.app.visible = true;
                            let ctx = self.egui_glow.as_ref().unwrap().egui_ctx.clone();
                            self.app.position_window(&ctx, window);
                        }
                    }
                }
                AppEvent::ShowWindow => {
                    if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                        if !self.app.visible {
                            self.app.visible = true;
                            let ctx = self.egui_glow.as_ref().unwrap().egui_ctx.clone();
                            self.app.position_window(&ctx, window);
                        } else {
                            self.app.refocus_window(window);
                        }
                    }
                }
                AppEvent::RefreshWindows(list) => {
                    self.app.windows = list;
                    self.app.apply_filter();
                    self.app.last_refresh = Instant::now();
                    if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                        window.request_redraw();
                    }
                }
            },
            UserEvent::Redraw(delay) => {
                self.repaint_delay = delay;
                if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                    window.request_redraw();
                }
            }
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = &cause {
            if let Some(window) = self.gl_window.as_ref().map(|w| w.window()) {
                window.request_redraw();
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.running.store(false, Ordering::Relaxed);
        self.app.config.save();
        if let Some(egui_glow) = self.egui_glow.as_mut() {
            egui_glow.destroy();
        }
    }
}

/// 加载系统中文字体（优先微软雅黑）
fn setup_chinese_fonts(ctx: &egui::Context) {
    fn load_font() -> Option<(Vec<u8>, u32)> {
        let candidates: &[(&str, u32)] = &[
            (r"C:\Windows\Fonts\msyh.ttc", 0),
            (r"C:\Windows\Fonts\msyhbd.ttc", 0),
            (r"C:\Windows\Fonts\simsun.ttc", 0),
            (r"C:\Windows\Fonts\simhei.ttf", 0),
            (r"C:\Windows\Fonts\msjhenghei.ttc", 0),
            (r"C:\Windows\Fonts\malgun.ttf", 0),
        ];
        for (path, index) in candidates {
            if let Ok(bytes) = std::fs::read(path) {
                return Some((bytes, *index));
            }
        }
        None
    }

    if let Some((font_bytes, index)) = load_font() {
        let mut font_data = egui::FontData::from_owned(font_bytes);
        font_data.index = index;
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("chinese".to_owned(), std::sync::Arc::new(font_data));
        if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            proportional.insert(0, "chinese".to_owned());
        }
        if let Some(monospace) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            monospace.push("chinese".to_owned());
        }
        ctx.set_fonts(fonts);
    } else {
        eprintln!("[Muka-程序切换实用工具] 未找到系统中文字体，中文可能显示为方块");
    }
}

/// 安装 panic 钩子，将崩溃信息写入日志文件便于诊断
fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{}\n{:?}", info, std::backtrace::Backtrace::capture());
        eprintln!("[Muka-程序切换实用工具] PANIC: {}", msg);
        if let Some(dir) = Config::config_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("panic.log"), msg);
        }
    }));
}

/// 通过命名互斥体确保仅运行一个实例
fn ensure_single_instance() -> bool {
    use windows::Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
    };
    unsafe {
        match CreateMutexW(None, true, w!("Muka-TaskSwitch-SingleInstance")) {
            Ok(_) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    false
                } else {
                    true
                }
            }
            Err(_) => false,
        }
    }
}

/// 后台线程：定期扫描窗口变化并推送更新
fn start_window_monitor(
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    _realtime: bool,
    running: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let manager = WindowManager::new();
        let mut last_list: Vec<WindowInfo> = Vec::new();

        while running.load(Ordering::Relaxed) {
            let list = manager.enumerate_windows();
            if list != last_list {
                let _ = proxy.send_event(UserEvent::App(AppEvent::RefreshWindows(list.clone())));
                last_list = list;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });
}

struct TaskSwitchApp {
    windows: Vec<WindowInfo>,
    filtered_windows: Vec<WindowInfo>,
    filter_text: String,
    filter_type: FilterType,
    selected_index: usize,
    config: Config,
    window_manager: WindowManager,
    hotkey_manager: GlobalHotKeyManager,
    current_hotkey: String,
    last_refresh: Instant,
    show_settings: bool,
    pending_hotkey: String,
    visible: bool,
    focus_search: bool,
    was_focused: bool,
    positioned: bool,
    first_focus_time: Option<Instant>,
    clear_color: egui::Color32,
    exit_requested: bool,
}

impl TaskSwitchApp {
    fn new(config: Config, window_manager: WindowManager, hotkey_manager: GlobalHotKeyManager) -> Self {
        let pending_hotkey = config.hotkey.clone();
        let current_hotkey = config.hotkey.clone();
        Self {
            windows: Vec::new(),
            filtered_windows: Vec::new(),
            filter_text: String::new(),
            filter_type: config.filter_type,
            selected_index: 0,
            config,
            window_manager,
            hotkey_manager,
            current_hotkey,
            last_refresh: Instant::now(),
            show_settings: false,
            pending_hotkey,
            visible: true,
            focus_search: true,
            was_focused: false,
            positioned: false,
            first_focus_time: None,
            clear_color: egui::Color32::TRANSPARENT,
            exit_requested: false,
        }
    }

    fn clear_color(&self) -> [f32; 4] {
        egui::Rgba::from(self.clear_color).to_array()
    }

    fn apply_filter(&mut self) {
        self.filtered_windows = filter_windows(&self.windows, &self.filter_text, self.filter_type);
        if self.selected_index >= self.filtered_windows.len() {
            self.selected_index = 0;
        }
    }

    fn activate_selected(&mut self) {
        if let Some(info) = self.filtered_windows.get(self.selected_index) {
            WindowManager::activate_window(info.hwnd);
        }
    }

    fn refocus_window(&mut self, window: &Window) {
        if let Some(hwnd) = hwnd_from_window(window) {
            unsafe {
                // 仅在当前窗口不是前台窗口时才调用 SetForegroundWindow，
                // 避免已聚焦时重复调用造成窗口闪烁
                if GetForegroundWindow() != hwnd {
                    let _ = SetForegroundWindow(hwnd);
                }
            }
        }
        self.focus_search = true;
        self.was_focused = false;
        self.first_focus_time = None;
    }

    fn generate_accent_color(text: &str) -> egui::Color32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();

        let hue = (hash % 360) as f32;
        let saturation = 0.75;
        let lightness = 0.55;
        hsv_to_rgb(hue / 360.0, saturation, lightness)
    }

    fn item_bg_color(visuals: &egui::Visuals, is_selected: bool, is_hovered: bool) -> egui::Color32 {
        if is_selected {
            visuals.selection.bg_fill
        } else if is_hovered {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        }
    }

    fn position_window(&mut self, _ctx: &egui::Context, window: &Window) {
        let scale = window.scale_factor() as f32;
        let work_area = get_primary_work_area();
        let width = self.config.window_width;
        let height = work_area.height() / scale;
        let x = work_area.right() / scale - width;
        let y = work_area.top() / scale;

        window.set_outer_position(LogicalPosition::new(x, y));
        let _ = window.request_inner_size(LogicalSize::new(width, height));
        window.set_visible(true);

        if let Some(hwnd) = hwnd_from_window(window) {
            remove_from_taskbar(hwnd);
        }

        if let Some(hwnd) = hwnd_from_window(window) {
            unsafe {
                let physical_width = (width * scale) as i32;
                let physical_x = work_area.right() as i32 - physical_width;
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    physical_x,
                    work_area.top() as i32,
                    physical_width,
                    work_area.height() as i32,
                    SWP_FRAMECHANGED | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                );
                let _ = SetForegroundWindow(hwnd);
            }
            apply_dwm_rounded_corners(hwnd);
        }
        self.positioned = true;
    }

    fn hide_window(&mut self, window: &Window) {
        self.visible = false;
        self.positioned = false;
        window.set_visible(false);
        if let Some(hwnd) = hwnd_from_window(window) {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, window: &Window) {
        if self.visible && !self.positioned {
            self.position_window(ctx, window);
        }

        // 失去焦点自动隐藏：只要窗口在显示期间曾经获得过焦点，失焦后立即隐藏
        if self.config.hide_on_focus_loss {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
            if focused && !self.was_focused {
                self.first_focus_time = Some(Instant::now());
            }
            let ever_focused = self.first_focus_time.is_some();
            if self.was_focused && !focused && ever_focused {
                self.hide_window(window);
            }
            self.was_focused = focused;
        }

        // 应用主题
        let system_dark = ctx.style().visuals.dark_mode;
        ctx.set_visuals(self.config.theme.to_egui_visuals(system_dark));

        // 定位完成前保持透明背景，避免在左侧留下残影
        self.clear_color = if self.positioned {
            ctx.style().visuals.panel_fill
        } else {
            egui::Color32::TRANSPARENT
        };

        // 全局快捷键
        let mut activate = false;
        let mut move_down = false;
        let mut move_up = false;
        let mut hide = false;
        ctx.input(|i| {
            hide = i.key_pressed(egui::Key::Escape);
            activate = !self.filter_text.is_empty()
                && i.key_pressed(egui::Key::Enter)
                && !self.filtered_windows.is_empty();
            move_down = i.key_pressed(egui::Key::ArrowDown);
            move_up = i.key_pressed(egui::Key::ArrowUp);
        });
        if hide {
            self.hide_window(window);
        }
        if activate {
            self.activate_selected();
            if self.config.hide_on_focus_loss {
                self.hide_window(window);
            }
        }
        if move_down && !self.filtered_windows.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.filtered_windows.len();
        }
        if move_up && !self.filtered_windows.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.filtered_windows.len() - 1
            } else {
                self.selected_index - 1
            };
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .fill(ctx.style().visuals.panel_fill)
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    // 标题栏
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("Muka-程序切换实用工具")
                                    .strong()
                                    .size(15.0),
                            );
                            ui.label(
                                egui::RichText::new(format!("{} 个窗口", self.windows.len()))
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("⚙").clicked() {
                                self.show_settings = !self.show_settings;
                            }
                            if ui.button("🔄").clicked() {
                                let list = self.window_manager.enumerate_windows();
                                self.windows = list;
                                self.apply_filter();
                                self.last_refresh = Instant::now();
                            }
                        });
                    });
                    ui.separator();

                    if self.show_settings {
                        render_settings(self, ui);
                    } else {
                        render_main(self, ui, ctx);
                    }
                });
            });

        if let Some(size) = ctx.input(|i| i.viewport().inner_rect).map(|r| r.size()) {
            self.config.window_width = size.x;
            self.config.window_height = size.y;
        }

        // 强制鼠标光标为默认箭头，避免在窗口背景/文本上显示输入光标
        ctx.set_cursor_icon(egui::CursorIcon::Default);
    }
}

fn hwnd_from_window(window: &Window) -> Option<HWND> {
    window.window_handle().ok().and_then(|h| match h.as_raw() {
        RawWindowHandle::Win32(win) => Some(HWND(win.hwnd.get() as *mut std::ffi::c_void)),
        _ => None,
    })
}

/// 从任务栏移除窗口按钮
fn remove_from_taskbar(hwnd: HWND) {
    unsafe {
        let result = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_SERVER);
        if let Ok(taskbar) = result {
            let _ = taskbar.DeleteTab(hwnd);
        }
    }
}

/// 使用 DWM 设置系统级抗锯齿圆角并禁用窗口阴影
fn apply_dwm_rounded_corners(hwnd: HWND) {
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );

        let policy = DWMNCRP_DISABLED;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &policy as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<DWMNCRENDERINGPOLICY>() as u32,
        );
    }
}

/// 获取系统 DPI 缩放（启动时尚无 egui Context，先用系统级 DPI）
fn system_scale_factor() -> f32 {
    unsafe {
        use windows::Win32::UI::HiDpi::GetDpiForSystem;
        let dpi = GetDpiForSystem();
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }
}

/// 获取主显示器工作区（排除任务栏）
fn get_primary_work_area() -> egui::Rect {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};
        let mut rect = windows::Win32::Foundation::RECT::default();
        if SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut std::ffi::c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
        {
            egui::Rect::from_min_max(
                egui::pos2(rect.left as f32, rect.top as f32),
                egui::pos2(rect.right as f32, rect.bottom as f32),
            )
        } else {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0))
        }
    }
}

fn render_main(app: &mut TaskSwitchApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    // 搜索框
    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.filter_text)
                .hint_text("搜索窗口...")
                .desired_width(f32::INFINITY),
        );
        if app.focus_search {
            response.request_focus();
            app.focus_search = false;
        }
        if response.changed() {
            app.apply_filter();
        }
        if !app.filter_text.is_empty() && ui.small_button("✕").clicked() {
            app.filter_text.clear();
            app.apply_filter();
            app.focus_search = true;
        }
    });

    // 过滤器类型
    ui.horizontal(|ui| {
        for filter_type in [FilterType::All, FilterType::ProcessName, FilterType::WindowTitle] {
            let selected = app.filter_type == filter_type;
            if ui.selectable_label(selected, filter_type.label()).clicked() && !selected {
                app.filter_type = filter_type;
                app.config.filter_type = filter_type;
                app.apply_filter();
            }
        }
    });

    if !app.filter_text.is_empty() {
        ui.label(
            egui::RichText::new(format!("找到 {} 个匹配项", app.filtered_windows.len()))
                .small()
                .color(ui.visuals().weak_text_color()),
        );
    }

    ui.separator();

    // 窗口列表
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            if app.filtered_windows.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(32.0);
                    ui.label(
                        egui::RichText::new("未找到匹配的窗口")
                            .size(16.0)
                            .color(ui.visuals().weak_text_color()),
                    );
                });
                return;
            }

            let mut clicked_index: Option<usize> = None;
            for (idx, info) in app.filtered_windows.iter().enumerate() {
                let is_selected = idx == app.selected_index;
                let accent = TaskSwitchApp::generate_accent_color(&info.process_name);

                let available_width = ui.available_width() - 6.0;
                let item_height = 46.0;
                let (rect, response) = ui.allocate_exact_size(
                    egui::Vec2::new(available_width, item_height),
                    egui::Sense::click(),
                );
                let is_hovered = response.hovered();

                // 绘制背景
                let bg_color = TaskSwitchApp::item_bg_color(ui.visuals(), is_selected, is_hovered);
                ui.painter().rect_filled(rect, egui::CornerRadius::same(6), bg_color);

                // 绘制内容
                let content_rect = rect.shrink2(egui::Vec2::new(10.0, 8.0));
                ui.scope_builder(egui::UiBuilder::new().max_rect(content_rect), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(ui.available_height());
                        // 左侧彩色指示条
                        let bar_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::Vec2::new(4.0, ui.available_height()),
                        );
                        ui.painter().rect_filled(bar_rect, egui::CornerRadius::same(2), accent);
                        ui.add_space(10.0);

                        // 文本
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(info.display_title())
                                        .strong()
                                        .size(13.0)
                                        .color(ui.visuals().text_color()),
                                )
                                .selectable(false)
                                .truncate(),
                            );
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(info.display_process())
                                            .small()
                                            .color(ui.visuals().weak_text_color()),
                                    )
                                    .selectable(false)
                                    .truncate(),
                                );
                                if info.is_elevated {
                                    ui.label(
                                        egui::RichText::new("⚠ 管理员")
                                            .small()
                                            .color(egui::Color32::ORANGE),
                                    );
                                }
                            });
                        });
                    });
                });

                // 禁止子标签把光标变成输入样式
                if response.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::Default);
                }

                if response.clicked() {
                    clicked_index = Some(idx);
                }

                if response.hovered() && ui.input(|i| i.pointer.secondary_clicked()) {
                    app.selected_index = idx;
                }

                ui.add_space(4.0);
            }

            if let Some(idx) = clicked_index {
                app.selected_index = idx;
                app.activate_selected();
            }
        });

    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.label(
            egui::RichText::new(format!(
                "上次刷新: {:.1}s 前",
                app.last_refresh.elapsed().as_secs_f32()
            ))
            .small()
            .color(ui.visuals().weak_text_color()),
        );
    });
}

fn render_settings(app: &mut TaskSwitchApp, ui: &mut egui::Ui) {
    ui.heading("设置");
    ui.separator();

    ui.horizontal(|ui| {
        ui.label("主题:");
        egui::ComboBox::from_id_salt(egui::Id::new("theme"))
            .selected_text(app.config.theme.label())
            .show_ui(ui, |ui| {
                for theme in [Theme::System, Theme::Dark, Theme::Light] {
                    if ui.selectable_label(app.config.theme == theme, theme.label()).clicked() {
                        app.config.theme = theme;
                    }
                }
            });
    });

    ui.horizontal(|ui| {
        ui.label("全局热键:");
        ui.text_edit_singleline(&mut app.pending_hotkey);
        if ui.small_button("应用").clicked() {
            let old = app.current_hotkey.clone();
            app.config.hotkey = app.pending_hotkey.clone();
            app.current_hotkey = app.pending_hotkey.clone();
            hotkey::register_hotkey(&app.hotkey_manager, &old, &app.config.hotkey);
        }
    });
    ui.label(
        egui::RichText::new("例如: Alt+Backquote, Ctrl+Shift+Q")
            .small()
            .color(ui.visuals().weak_text_color()),
    );

    ui.checkbox(&mut app.config.always_on_top, "窗口置顶");
    ui.checkbox(&mut app.config.hide_on_focus_loss, "失去焦点时自动隐藏");

    let mut startup = app.config.start_with_windows;
    if ui.checkbox(&mut startup, "开机启动").changed() {
        app.config.start_with_windows = startup;
        app.config.apply_startup();
    }

    ui.checkbox(&mut app.config.realtime_refresh, "实时刷新窗口列表");

    ui.separator();
    if ui.button("保存并返回").clicked() {
        app.config.save();
        app.show_settings = false;
    }
}

fn filter_windows(windows: &[WindowInfo], text: &str, filter_type: FilterType) -> Vec<WindowInfo> {
    let text = text.to_lowercase();
    if text.is_empty() {
        return windows.to_vec();
    }
    windows
        .iter()
        .filter(|w| match filter_type {
            FilterType::All => {
                w.process_name.to_lowercase().contains(&text)
                    || w.window_title.to_lowercase().contains(&text)
            }
            FilterType::ProcessName => w.process_name.to_lowercase().contains(&text),
            FilterType::WindowTitle => w.window_title.to_lowercase().contains(&text),
        })
        .cloned()
        .collect()
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> egui::Color32 {
    let c = v * s;
    let x = c * (1.0 - ((h * 6.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h * 6.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    egui::Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_windows() -> Vec<WindowInfo> {
        vec![
            WindowInfo {
                hwnd: 1,
                process_id: 100,
                process_name: "notepad.exe".to_string(),
                window_title: "rust-notes".to_string(),
                class_name: "Notepad".to_string(),
                is_elevated: false,
            },
            WindowInfo {
                hwnd: 2,
                process_id: 200,
                process_name: "rust-app.exe".to_string(),
                window_title: "Untitled".to_string(),
                class_name: "AppWindow".to_string(),
                is_elevated: false,
            },
            WindowInfo {
                hwnd: 3,
                process_id: 300,
                process_name: "Taskmgr.exe".to_string(),
                window_title: "任务管理器".to_string(),
                class_name: "TaskManagerWindow".to_string(),
                is_elevated: true,
            },
        ]
    }

    #[test]
    fn test_filter_all() {
        let windows = sample_windows();
        let result = filter_windows(&windows, "rust", FilterType::All);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|w| w.process_name == "notepad.exe"));
        assert!(result.iter().any(|w| w.process_name == "rust-app.exe"));
    }

    #[test]
    fn test_filter_process_name() {
        let windows = sample_windows();
        let result = filter_windows(&windows, "rust-app", FilterType::ProcessName);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].process_name, "rust-app.exe");
    }

    #[test]
    fn test_filter_window_title() {
        let windows = sample_windows();
        let result = filter_windows(&windows, "任务", FilterType::WindowTitle);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].window_title, "任务管理器");
    }

    #[test]
    fn test_filter_empty_returns_all() {
        let windows = sample_windows();
        let result = filter_windows(&windows, "", FilterType::All);
        assert_eq!(result.len(), windows.len());
    }

    #[test]
    fn test_config_roundtrip() {
        let config = Config {
            hotkey: "Ctrl+Shift+Q".to_string(),
            theme: Theme::Dark,
            window_width: 500.0,
            window_height: 800.0,
            always_on_top: false,
            hide_on_focus_loss: false,
            start_with_windows: true,
            filter_type: FilterType::ProcessName,
            realtime_refresh: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config.hotkey, restored.hotkey);
        assert_eq!(config.theme, restored.theme);
        assert_eq!(config.start_with_windows, restored.start_with_windows);
        assert_eq!(config.filter_type, restored.filter_type);
    }
}
