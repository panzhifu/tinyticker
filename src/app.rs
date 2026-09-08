//! 悬浮窗口：winit 事件循环 + [`PixelSurface`] 呈现（Wayland ARGB / X11 softbuffer）。
//!
//! 无边框、置顶；左键按住拖动，右键关闭。绘制约 30fps 节流。

use std::num::NonZeroU32;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ldtray::TrayHandle;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{Window, WindowLevel};

use crate::clock;
use crate::config::Config;
use crate::render;
use crate::surface::{self, PixelSurface};
use crate::timer::{Finished, Mode, Phase, Timer};
use crate::tray::{self, Command};

/// 逻辑画布大小（参考值；实际像素缓冲跟随窗口物理尺寸 × 缩放系数）。
const LOGICAL_SIZE: (u32, u32) = (200, 100);

/// 轮询间隔：处理托盘命令与计时的心跳（也驱动重绘）。
///
/// 显示内容是秒级变化的，30fps 毫无意义；200ms 既能让托盘点击在
/// 一次眨眼内响应，又把空转开销降到 1/6。真正的省电靠 `last_frame`
/// 脏检查——内容没变时连 shm 都不提交。
const DRAW_INTERVAL: Duration = Duration::from_millis(200);

pub struct App {
    rx: Receiver<Command>,
    handle_rx: Receiver<TrayHandle>, // 托盘线程就绪后发来
    tray: Option<TrayHandle>,
    config: Config,
    timer: Timer,
    window: Option<Arc<Window>>,
    surface: Option<Box<dyn PixelSurface>>,
    buffer_size: (u32, u32), // 像素缓冲尺寸（物理像素）
    font_scale: u32,         // 字形缩放倍数（DPI × 缩放倍数，至少 1）
    zoom: f32,               // 滚轮缩放倍数（持久化）
    /// 上一帧的内容指纹（文本 / 状态 / 尺寸 / 缩放）；用于跳过无变化的重绘。
    last_frame: Option<(String, String, u32, u32, u32)>,
    /// 当前生效的输入区域（surface 坐标），避免重复下发。
    last_input_region: Option<(i32, i32, i32, i32)>,
}

impl App {
    pub fn new(
        rx: Receiver<Command>,
        handle_rx: Receiver<TrayHandle>,
        config: Config,
        autostart: bool,
    ) -> Self {
        let mut timer = Timer::new(config.mode, config.duration_secs);
        timer.set_pomo_durations(config.pomo_work, config.pomo_break);
        if autostart {
            timer.start();
        }
        Self {
            rx,
            handle_rx,
            tray: None,
            zoom: config.zoom,
            config,
            timer,
            window: None,
            surface: None,
            buffer_size: LOGICAL_SIZE,
            font_scale: 1,
            last_frame: None,
            last_input_region: None,
        }
    }

    /// 处理来自托盘的命令，并接收托盘句柄（用于发通知）。
    fn handle_commands(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(handle) = self.handle_rx.try_recv() {
            self.tray = Some(handle);
        }
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Command::Start => self.timer.start(),
                Command::Pause => self.timer.pause(),
                Command::Reset => self.timer.reset(),
                Command::Preset(secs) => self.timer.start_countdown(secs),
                Command::SetMode(mode) => self.timer.set_mode(mode),
                Command::Quit => {
                    self.persist_state();
                    event_loop.exit();
                }
            }
        }
    }

    /// 退出前把当前模式、倒计时总时长和窗口位置写回配置。
    fn persist_state(&mut self) {
        self.config.duration_secs = self.timer.total;
        self.config.mode = self.timer.mode;
        self.config.zoom = self.zoom;
        if let Some(window) = &self.window
            && let Ok(pos) = window.outer_position()
        {
            self.config.window_pos = Some((pos.x, pos.y));
        }
        self.config.save();
    }

    /// 绘制一帧（CPU 写预乘 ARGB 缓冲，Wayland 走 shm、X11 走 softbuffer）。
    ///
    /// 内容未变化（文本 / 状态 / 尺寸 / 缩放指纹相同）时整帧跳过，连 shm
    /// 都不提交——常驻挂件下绝大多数帧都会被这里拦下。
    fn draw(&mut self) {
        let (bw, bh) = self.buffer_size;
        let scale = self.font_scale;

        // 预取绘制参数（paint 闭包不能借用 self）
        let bg = render::premultiply(self.config.color_bg, self.config.bg_alpha);
        // 状态行：番茄钟显示阶段与轮数，时钟模式固定 CLOCK
        let status = match self.timer.mode {
            Mode::Clock => "CLOCK".to_string(),
            Mode::Pomodoro => {
                let (label, n) = if self.timer.phase == Phase::Work {
                    ("WORK", self.timer.round + 1)
                } else {
                    ("BREAK", self.timer.round)
                };
                format!("{label} {n}")
            }
            Mode::Stopwatch if self.timer.running => "RUNNING".to_string(),
            Mode::Stopwatch => "STOPWATCH".to_string(),
            _ if self.timer.is_done() => "DONE".to_string(),
            _ if self.timer.running => "RUNNING".to_string(),
            _ => "PAUSED".to_string(),
        };
        // 数字颜色：时钟常亮运行色；其余按 结束/运行/暂停 三态
        let num_color = match self.timer.mode {
            Mode::Clock => render::premultiply(self.config.color_running, 0xFF),
            _ if self.timer.is_done() => render::premultiply(self.config.color_done, 0xFF),
            _ if self.timer.running => render::premultiply(self.config.color_running, 0xFF),
            _ => render::premultiply(self.config.color_paused, 0xFF),
        };
        let status_color = render::premultiply(0xA0A0AA, 0xFF);
        // 时钟模式实时读取本地时间；其余模式显示计时秒数
        let text = if self.timer.mode == Mode::Clock {
            let (h, m, s) = clock::now_hms();
            clock::format_hms(h, m, s, self.config.clock_12h)
        } else {
            render::format_time(self.timer.display_secs())
        };

        // 内容指纹：没变就不重绘
        let frame_key = (text.clone(), status.clone(), bw, bh, scale);
        if self.last_frame.as_ref() == Some(&frame_key) {
            return;
        }
        self.last_frame = Some(frame_key);

        // 布局：数字行 8x8 × (scale*2)，状态行 8x8 × scale，垂直居中
        let num_scale = scale * 2;
        let num_h = 8 * num_scale;
        let status_h = 8 * scale;
        let gap = (4 * scale) as i32;
        let total_h = num_h as i32 + gap + status_h as i32;
        let y_num = ((bh as i32 - total_h) / 2).max(0);
        let y_status = y_num + num_h as i32 + gap;

        {
            let Some(surface) = &mut self.surface else {
                return;
            };
            surface.draw_frame(&mut |buf| {
                let mut canvas = render::Canvas::new(buf, bw, bh);
                // 半透明背景（bg_alpha 可在配置文件调整，0 = 完全透明只剩文字）
                canvas.fill(bg);
                canvas.draw_text_centered(y_num, &text, num_color, num_scale);
                canvas.draw_text_centered(y_status, &status, status_color, scale);
            });
        }

        // 输入区域：默认整窗接收（拖动更顺手）；开启 click_through 后
        // 收缩到两行文字的包围盒，透明处不再拦截鼠标，代价是拖动要点中文字。
        let target = if self.config.click_through {
            let text_w = text.len() as u32 * 8 * num_scale;
            let status_w = status.len() as u32 * 8 * scale;
            let x_num = bw.saturating_sub(text_w) / 2;
            let x_status = bw.saturating_sub(status_w) / 2;
            let left = x_num.min(x_status);
            let right = (x_num + text_w).max(x_status + status_w).min(bw);
            let top = y_num as u32;
            let bottom = (y_status.max(0) as u32 + status_h).min(bh);

            // 物理像素 → surface 坐标（winit 会设置 buffer_scale = scale_factor）
            let sf = self
                .window
                .as_ref()
                .map_or(1.0, |w| w.scale_factor() as f32)
                .max(1.0);
            Some((
                (left as f32 / sf).floor() as i32,
                (top as f32 / sf).floor() as i32,
                ((right.saturating_sub(left)) as f32 / sf).ceil().max(1.0) as i32,
                ((bottom.saturating_sub(top)) as f32 / sf).ceil().max(1.0) as i32,
            ))
        } else {
            None
        };
        if self.last_input_region != target {
            if let Some(surface) = &mut self.surface {
                surface.set_input_region(target);
            }
            self.last_input_region = target;
        }
    }

    /// 每帧更新：命令、计时、结束事件、绘制；随后约定下一次唤醒。
    fn update(&mut self, event_loop: &ActiveEventLoop) {
        self.handle_commands(event_loop);
        self.timer.maybe_tick();
        self.handle_finished();
        self.draw();
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + DRAW_INTERVAL));
    }

    /// 结束事件处理：按来源发通知；倒计时/番茄钟专注完成时执行 on_finish 命令。
    fn handle_finished(&mut self) {
        let Some(ev) = self.timer.take_finished() else {
            return;
        };
        let (body, action) = match ev {
            Finished::Countdown => ("⏰ 计时结束", "再来一次"),
            Finished::PomodoroWork => ("🍅 专注完成，休息一下", "好的"),
            Finished::PomodoroBreak => ("☕ 休息结束，继续专注", "开始"),
        };
        if let Some(handle) = &self.tray {
            tray::notify(handle, body, action);
        }
        // 锁屏 / 关机 / 打开文件等场景都由用户命令覆盖
        if matches!(ev, Finished::Countdown | Finished::PomodoroWork)
            && let Some(cmd) = self.config.on_finish.clone()
        {
            // 后台线程等待子进程退出，避免僵尸进程，也不阻塞 UI
            std::thread::spawn(move || {
                match std::process::Command::new("sh").arg("-c").arg(&cmd).spawn() {
                    Ok(mut child) => {
                        let _ = child.wait();
                    }
                    Err(e) => eprintln!("⚠️ on_finish 执行失败: {e}"),
                }
            });
        }
    }

    fn resize_surface(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if let Some(surface) = &mut self.surface {
            surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap());
            self.buffer_size = (w, h);
        }
        if let Some(window) = &self.window {
            self.font_scale = ((window.scale_factor() as f32 * self.zoom).round() as u32).max(1);
        }
    }

    /// 应用缩放：请求新窗口尺寸（Resized 事件随后校正缓冲），字号立即更新。
    fn apply_zoom(&mut self) {
        if let Some(window) = &self.window {
            let sf = window.scale_factor() as f32;
            let size = PhysicalSize::new(
                (LOGICAL_SIZE.0 as f32 * self.zoom * sf) as u32,
                (LOGICAL_SIZE.1 as f32 * self.zoom * sf) as u32,
            );
            // 返回值是新请求的尺寸（Wayland 上要等合成器确认，此处不依赖）
            let _ = window.request_inner_size(size);
            self.font_scale = ((sf * self.zoom).round() as u32).max(1);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // 悬浮窗配置：无边框、固定尺寸、置顶
        // （WindowLevel 仅在 X11 / Windows / macOS 生效；Wayland 协议不允许应用
        //   自行设置层级，需由合成器规则实现，此处自动降级为普通层级）
        let attrs = Window::default_attributes()
            .with_title("TinyTicker")
            .with_inner_size(LogicalSize::new(
                LOGICAL_SIZE.0 as f32 * self.zoom,
                LOGICAL_SIZE.1 as f32 * self.zoom,
            ))
            .with_decorations(false) // 无边框（去除标题栏）
            .with_transparent(true) // 声明透明窗口；不声明 winit 会设置 opaque region，合成器强制不透明
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop);

        let window = Arc::new(event_loop.create_window(attrs).expect("无法创建窗口"));

        // 恢复上次退出时的窗口位置（Wayland 忽略，X11/Win/macOS 生效）
        if let Some((x, y)) = self.config.window_pos {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }

        // 按显示协议选择呈现后端（Wayland → 自研 ARGB shm；X11 → softbuffer）
        let mut surface = surface::create(&window).expect("无法创建呈现后端");

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap());

        self.font_scale = ((window.scale_factor() as f32 * self.zoom).round() as u32).max(1);
        self.buffer_size = (w, h);
        self.surface = Some(surface);
        self.window = Some(window);

        // 30fps 靠事件循环定时唤醒驱动（不能依赖 about_to_wait 里的
        // request_redraw：Wayland 的 frame callback 链可能不再回来）
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + DRAW_INTERVAL));
    }


    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.persist_state();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.resize_surface(size.width, size.height);
            }
            WindowEvent::RedrawRequested => {
                self.update(event_loop);
            }
            // 无边框窗口：左键按住拖动窗口，右键点击关闭
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(window) = &self.window {
                    let _ = window.drag_window();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.persist_state();
                event_loop.exit();
            }
            // 滚轮缩放窗口与字号（倍数持久化）
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(v, _) => v,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                let zoom = (self.zoom + dy * 0.25).clamp(0.5, 3.0);
                if zoom != self.zoom {
                    self.zoom = zoom;
                    self.apply_zoom();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.update(event_loop);
    }
}
