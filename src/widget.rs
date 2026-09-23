//! 与显示后端无关的挂件核心：计时推进、结束事件、帧内容与布局。
//!
//! 后端（`wl` 的 layer-shell / `x11` 的 override-redirect）只负责尺寸、缩放、
//! 输入事件与呈现，一帧的内容由 [`Widget::build_frame`] 产出；内容未变化时返回
//! `None`，后端据此整帧跳过（连 shm 都不提交）。

use std::time::Duration;

use crate::tray::TrayHandle;

use crate::clock;
use crate::config::{Config, PALETTES};
use crate::render::{self, premultiply, Canvas};
use crate::timer::{Finished, Mode, Phase, Timer};
use crate::tray::{self, Command};

/// 逻辑画布大小（参考值；实际像素缓冲跟随窗口物理尺寸 × 缩放系数）。
pub const LOGICAL_SIZE: (u32, u32) = (200, 100);

/// 心跳间隔：处理托盘命令与计时的心跳（也驱动重绘）。
///
/// 显示内容是秒级变化的，30fps 毫无意义；200ms 既能让托盘点击在
/// 一次眨眼内响应，又把空转开销降到 1/6。真正的省电靠 [`Widget::build_frame`]
/// 的脏检查——内容没变时连 shm 都不提交。
pub const DRAW_INTERVAL: Duration = Duration::from_millis(200);

/// 一帧的完整内容与布局（buffer 物理像素坐标）。
pub struct Frame {
    pub text: String,
    pub status: String,
    pub width: u32,
    pub height: u32,
    num_color: u32,
    status_color: u32,
    bg: u32,
    num_scale: u32,
    status_scale: u32,
    y_num: i32,
    y_status: i32,
    click_through: bool,
}

impl Frame {
    pub fn paint(&self, canvas: &mut Canvas) {
        canvas.fill(self.bg);
        canvas.draw_text_centered(self.y_num, &self.text, self.num_color, self.num_scale);
        canvas.draw_text_centered(self.y_status, &self.status, self.status_color, self.status_scale);
    }

    /// 点击穿透时接收输入的区域（surface 逻辑坐标 `x, y, w, h`）；
    /// 未开启穿透则返回 `None`（整窗接收）。
    pub fn input_rect(&self, scale_factor: f32) -> Option<(i32, i32, i32, i32)> {
        if !self.click_through {
            return None;
        }
        // 两行文字并集的包围盒
        let text_w = render::text_width(self.text.chars().count(), self.num_scale);
        let status_w = render::text_width(self.status.chars().count(), self.status_scale);
        let x_num = self.width.saturating_sub(text_w) / 2;
        let x_status = self.width.saturating_sub(status_w) / 2;
        let left = x_num.min(x_status);
        let right = (x_num + text_w).max(x_status + status_w).min(self.width);
        let top = self.y_num.max(0) as u32;
        let bottom = (self.y_status.max(0) as u32 + 8 * self.status_scale).min(self.height);

        // 物理像素 → surface 坐标（buffer 按 scale_factor 放大过）
        let sf = scale_factor.max(1.0);
        Some((
            (left as f32 / sf).floor() as i32,
            (top as f32 / sf).floor() as i32,
            ((right - left) as f32 / sf).ceil().max(1.0) as i32,
            (bottom.saturating_sub(top) as f32 / sf).ceil().max(1.0) as i32,
        ))
    }
}

pub struct Widget {
    pub config: Config,
    pub timer: Timer,
    /// 滚轮缩放倍数（0.5-3.0，持久化）。
    pub zoom: f32,
    tray: Option<TrayHandle>,
    /// 上一帧的内容指纹（文本 / 状态 / 尺寸 / 字号）。
    last_frame: Option<(String, String, u32, u32, u32)>,
}

impl Widget {
    pub fn new(config: Config, autostart: bool) -> Self {
        let zoom = config.zoom;
        let mut timer = Timer::new(config.mode, config.duration_secs);
        timer.set_pomo_durations(config.pomo_work, config.pomo_break);
        if autostart {
            timer.start();
        }
        Self {
            config,
            timer,
            zoom,
            tray: None,
            last_frame: None,
        }
    }

    /// 接收托盘线程发来的句柄（用于发通知）。
    pub fn accept_tray_handle(&mut self, handle: TrayHandle) {
        self.tray = Some(handle);
    }

    /// 处理托盘命令；返回 `true` 表示请求退出。
    pub fn handle_cmd(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Start => self.timer.start(),
            Command::Pause => self.timer.pause(),
            Command::Toggle => {
                if self.timer.running {
                    self.timer.pause();
                } else {
                    self.timer.start();
                }
            }
            Command::Reset => self.timer.reset(),
            Command::Preset(secs) => self.timer.start_countdown(secs),
            Command::SetMode(mode) => self.timer.set_mode(mode),
            Command::ZoomBy(dy) => {
                if self.zoom_by(dy as f32) {
                    self.config.zoom = self.zoom;
                    self.persist_appearance();
                }
            }
            Command::SetAlpha(alpha) => {
                if self.config.bg_alpha != alpha {
                    self.config.bg_alpha = alpha;
                    self.persist_appearance();
                }
            }
            Command::SetIcon(mode) => {
                if self.config.tray_icon != mode {
                    self.config.tray_icon = mode;
                    self.persist_appearance();
                }
            }
            Command::SetPalette(i) => {
                if let Some(p) = PALETTES.get(i) {
                    self.config.color_bg = p.bg;
                    self.config.color_running = p.running;
                    self.config.color_paused = p.paused;
                    self.config.color_done = p.done;
                    self.persist_appearance();
                }
            }
            Command::Quit => return true,
        }
        false
    }

    /// 外观变了：强制重绘一次，并立刻写回配置——菜单点击是明确意图，
    /// 不该因为进程被 kill 而丢掉。
    fn persist_appearance(&mut self) {
        self.invalidate();
        self.config.save();
    }

    /// 推进计时，并处理本次产生的结束事件（通知 + on_finish 命令）。
    pub fn tick(&mut self) {
        self.timer.maybe_tick();
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
                let spawned = std::process::Command::new("sh").arg("-c").arg(&cmd).spawn();
                match spawned {
                    Ok(mut child) => {
                        let _ = child.wait();
                    }
                    Err(e) => eprintln!("⚠️ on_finish 执行失败: {e}"),
                }
            });
        }
    }

    /// 滚轮缩放：`dy` 为带符号的格数；返回 `true` 表示倍数确实变了。
    pub fn zoom_by(&mut self, dy: f32) -> bool {
        let zoom = (self.zoom + dy * 0.25).clamp(0.5, 3.0);
        if zoom == self.zoom {
            return false;
        }
        self.zoom = zoom;
        true
    }

    /// 生成当前帧；内容与上一帧相同则返回 `None`（调用方据此跳过重绘）。
    pub fn build_frame(&mut self, width: u32, height: u32, scale: u32) -> Option<Frame> {
        let bg = premultiply(self.config.color_bg, self.config.bg_alpha);
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
            Mode::Clock => premultiply(self.config.color_running, 0xFF),
            _ if self.timer.is_done() => premultiply(self.config.color_done, 0xFF),
            _ if self.timer.running => premultiply(self.config.color_running, 0xFF),
            _ => premultiply(self.config.color_paused, 0xFF),
        };
        let status_color = premultiply(0xA0A0AA, 0xFF);
        // 时钟模式实时读取本地时间；其余模式显示计时秒数
        let text = if self.timer.mode == Mode::Clock {
            let (h, m, s) = clock::now_hms();
            clock::format_hms(h, m, s, self.config.clock_12h)
        } else {
            render::format_time(self.timer.display_secs())
        };

        let key = (text.clone(), status.clone(), width, height, scale);
        if self.last_frame.as_ref() == Some(&key) {
            return None;
        }
        self.last_frame = Some(key);

        // 布局：数字行 8x8 × (scale*2)，状态行 8x8 × scale，垂直居中
        let num_scale = scale * 2;
        let num_h = 8 * num_scale;
        let status_h = 8 * scale;
        let gap = 4 * scale;
        let total_h = (num_h + gap + status_h) as i32;
        let y_num = ((height as i32 - total_h) / 2).max(0);
        let y_status = y_num + (num_h + gap) as i32;

        Some(Frame {
            text,
            status,
            width,
            height,
            num_color,
            status_color,
            bg,
            num_scale,
            status_scale: scale,
            y_num,
            y_status,
            click_through: self.config.click_through,
        })
    }

    /// 让下一帧强制重绘。两个用途：X11 的 `Expose`（窗口被揭开后像素已丢），
    /// 以及改了颜色/透明度之后——帧指纹只含文本与尺寸，不清它就会把这次重绘吃掉。
    pub fn invalidate(&mut self) {
        self.last_frame = None;
    }

    /// 退出前把当前模式、倒计时总时长、缩放和窗口位置写回配置。
    pub fn persist(&mut self, pos: Option<(i32, i32)>) {
        self.config.duration_secs = self.timer.total;
        self.config.mode = self.timer.mode;
        self.config.zoom = self.zoom;
        if let Some((x, y)) = pos {
            self.config.window_pos = Some((x, y));
        }
        self.config.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn toggle_flips_running_and_back() {
        let mut w = Widget::new(Config::default(), false);
        assert!(!w.timer.running);
        w.handle_cmd(Command::Toggle);
        assert!(w.timer.running);
        w.handle_cmd(Command::Toggle);
        assert!(!w.timer.running);
    }

    #[test]
    fn toggle_restarts_a_finished_countdown() {
        let cfg = Config { duration_secs: 2, ..Config::default() };
        let mut w = Widget::new(cfg, true);
        let t0 = Instant::now();
        w.timer.tick_at(t0);
        w.timer.tick_at(t0 + Duration::from_secs(3));
        assert!(w.timer.is_done() && !w.timer.running);
        w.handle_cmd(Command::Toggle);
        assert!(w.timer.running, "结束后左键应当重新开始，而不是停在 DONE");
        assert!(!w.timer.is_done());
    }

    /// 外观命令带磁盘写（`config.save()`），不适合在单测里走；这里只测它的另一半：
    /// 改颜色必须 invalidate 才能到屏幕上。
    #[test]
    fn color_change_needs_an_invalidate_to_reach_the_screen() {
        let mut w = Widget::new(Config::default(), false);
        assert!(w.build_frame(200, 100, 2).is_some(), "第一帧总要画");
        assert!(w.build_frame(200, 100, 2).is_none(), "内容没变就该跳过这一帧");
        // 只改透明度：指纹（文本/状态/尺寸/字号）完全不变
        w.config.bg_alpha = 200;
        assert!(w.build_frame(200, 100, 2).is_none(), "指纹不含颜色，不 invalidate 就会被吃掉");
        w.invalidate();
        assert!(w.build_frame(200, 100, 2).is_some());
    }
}
