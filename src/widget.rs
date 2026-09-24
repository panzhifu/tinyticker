//! 与显示后端无关的挂件核心：计时推进、结束事件、帧内容与布局。
//!
//! 后端（`wl` 的 layer-shell / `x11` 的 override-redirect）只负责尺寸、缩放、
//! 输入事件与呈现，一帧的内容由 [`Widget::build_frame`] 产出；内容未变化时返回
//! `None`，后端据此整帧跳过（连 shm 都不提交）。

use std::time::{Duration, Instant};

use crate::tray::TrayHandle;

use crate::clock;
use crate::config::{Config, PALETTES};
use crate::effect::{self, Effect, Gradient};
use crate::render::{self, premultiply, Canvas};
use crate::text;
use crate::textsrc;
use crate::timer::{Finished, Mode, Timer};
use crate::tray::{self, Command};

/// 逻辑画布大小（参考值；实际像素缓冲跟随窗口物理尺寸 × 缩放系数）。
pub const LOGICAL_SIZE: (u32, u32) = (200, 100);

/// 静态内容的心跳间隔。
///
/// 显示内容是秒级变化的，30fps 毫无意义；200ms 既能让托盘点击在
/// 一次眨眼内响应，又把空转开销降到 1/6。真正的省电靠 [`Widget::build_frame`]
/// 的脏检查——内容没变时连 shm 都不提交。
pub const DRAW_INTERVAL: Duration = Duration::from_millis(200);

/// 动画特效的心跳间隔。Catime 给动画特效配的是 33-120ms 的渲染定时器，
/// 50ms 是同一档；再快也只是白烧 CPU，因为位图字体本身只有 8 像素高。
pub const ANIM_INTERVAL: Duration = Duration::from_millis(50);

/// 动画相位进帧指纹时的量化步长，与 [`ANIM_INTERVAL`] 对齐。
const ANIM_STEP_MS: u32 = 50;

/// 一帧的完整内容与布局（buffer 物理像素坐标）。
///
/// 只带已栅格化的字形：文本本身在 [`Widget::last_frame`] 的指纹里已经有一份，
/// 这里再存一遍就是纯粹的死重。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// 两行已栅格化的字形。坐标是绝对的，特效与绘制都直接消费它。
    num_run: text::Run,
    status_run: text::Run,
    num_color: Gradient,
    status_color: Gradient,
    effect: Effect,
    /// 动画时钟（毫秒）。静态配色与静态特效恒为 0，好让脏检查照样能去重。
    phase_ms: u32,
    bg: u32,
    num_scale: u32,
    status_scale: u32,
    click_through: bool,
}

impl Frame {
    pub fn paint(&self, canvas: &mut Canvas) {
        canvas.fill(self.bg);
        // 两行各走一遍特效：它们各有各的颜色，合一张遮罩反而要多一次逐像素判断
        let rows = [
            effect::Row { run: &self.num_run, scale: self.num_scale },
            effect::Row { run: &self.status_run, scale: self.status_scale },
        ];
        effect::draw(canvas, &rows[..1], &self.num_color, self.effect, self.phase_ms);
        effect::draw(canvas, &rows[1..], &self.status_color, self.effect, self.phase_ms);
    }

    /// 点击穿透时接收输入的区域（surface 逻辑坐标 `x, y, w, h`）；
    /// 未开启穿透则返回 `None`（整窗接收）。
    pub fn input_rect(&self, scale_factor: f32) -> Option<(i32, i32, i32, i32)> {
        if !self.click_through {
            return None;
        }
        // 两行字形实际包围盒的并集：按 run 而不是按"字数 × 8"算，
        // 否则混排行（点阵 + TTF 中文）的右边会被切掉一截。
        let x_num = self.width.saturating_sub(self.num_run.width) / 2;
        let x_status = self.width.saturating_sub(self.status_run.width) / 2;
        let left = x_num.min(x_status);
        let right = (x_num + self.num_run.width)
            .max(x_status + self.status_run.width)
            .min(self.width);
        let top = self.num_run.top.min(self.status_run.top).max(0) as u32;
        let bottom = (self.num_run.bottom.max(self.status_run.bottom).max(0) as u32).min(self.height);
        let height = bottom.saturating_sub(top).max(1);

        // 物理像素 → surface 坐标（buffer 按 scale_factor 放大过）
        let sf = scale_factor.max(1.0);
        Some((
            (left as f32 / sf).floor() as i32,
            (top as f32 / sf).floor() as i32,
            ((right - left) as f32 / sf).ceil().max(1.0) as i32,
            (height as f32 / sf).ceil().max(1.0) as i32,
        ))
    }
}

pub struct Widget {
    pub config: Config,
    pub timer: Timer,
    /// 滚轮缩放倍数（0.5-3.0，持久化）。
    pub zoom: f32,
    tray: Option<TrayHandle>,
    /// 外部文本源：配了 `text_source` 才碰文件系统
    text_src: textsrc::Source,
    /// 特效动画的时钟原点。
    started: Instant,
    /// 上一帧的内容指纹（文本 / 状态 / 尺寸 / 字号 / 动画相位）。
    last_frame: Option<(String, String, u32, u32, u32, u32)>,
}

impl Widget {
    pub fn new(config: Config, autostart: bool) -> Self {
        let zoom = config.zoom;
        // 趁 config 还没进结构体先把路径取走
        let text_src = textsrc::Source::new(config.text_source.as_deref());
        let mut timer = Timer::new(config.mode, config.duration_secs);
        timer.set_pomo(&config.pomo);
        if autostart {
            timer.start();
        }
        Self {
            config,
            timer,
            zoom,
            tray: None,
            text_src,
            started: Instant::now(),
            last_frame: None,
        }
    }

    /// 有没有需要持续重绘的东西：动画特效，或会自动流动的渐变。
    fn animating(&self) -> bool {
        self.config.text_effect.animated()
            || self.config.color_running.animated()
            || self.config.color_paused.animated()
            || self.config.color_done.animated()
    }

    /// 心跳间隔：静态内容 200ms 就够，动画特效要 50ms 才看得出流动。
    pub fn tick_interval(&self) -> Duration {
        if self.animating() { ANIM_INTERVAL } else { DRAW_INTERVAL }
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
                    // 预设是单色板；写成预设会把用户手调的渐变覆盖掉，这是点预设的应有之义
                    self.config.color_running = Gradient::solid(p.running);
                    self.config.color_paused = Gradient::solid(p.paused);
                    self.config.color_done = Gradient::solid(p.done);
                    self.persist_appearance();
                }
            }
            Command::SetEffect(effect) => {
                if self.config.text_effect != effect {
                    self.config.text_effect = effect;
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
        self.text_src.refresh();
        self.timer.maybe_tick();
        let Some(ev) = self.timer.take_finished() else {
            return;
        };
        let (body, action) = match ev {
            Finished::Countdown => ("⏰ 计时结束", "再来一次"),
            Finished::PomodoroWork => ("🍅 专注完成，休息一下", "好的"),
            Finished::PomodoroBreak => ("☕ 休息结束，继续专注", "开始"),
            Finished::PomodoroLongBreak => ("🌿 长休息结束，开始下一组", "开始"),
            Finished::PomodoroAllDone => ("🎉 番茄钟全部完成", "再来一组"),
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
        // 状态行：番茄钟显示阶段与组内序号（收工则显示 DONE），时钟模式固定 CLOCK
        let status = match self.timer.mode {
            Mode::Clock => "CLOCK".to_string(),
            Mode::Pomodoro if self.timer.is_done() => "DONE".to_string(),
            Mode::Pomodoro => {
                let (label, n) = self.timer.pomo_status();
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
            Mode::Clock => self.config.color_running.clone(),
            _ if self.timer.is_done() => self.config.color_done.clone(),
            _ if self.timer.running => self.config.color_running.clone(),
            _ => self.config.color_paused.clone(),
        };
        // 外部文本源读到内容时顶替状态行；它为空/不可用则回到上面的正常状态。
        // 截断按**实测像素宽**算，不是按字数——中文一格点阵宽写不下时也必须整字退让。
        let status = match self.text_src.text() {
            Some(external) => text::fit(external, scale, width),
            None => status,
        };
        let status_color = Gradient::solid(0xA0A0AA);
        // 时钟模式实时读取本地时间；其余模式显示计时秒数
        let text = if self.timer.mode == Mode::Clock {
            let (h, m, s) = clock::now_hms();
            clock::format_hms(h, m, s, self.config.clock_12h)
        } else {
            render::format_time(self.timer.display_secs())
        };

        // 动画相位要进指纹，否则内容没变时脏检查会把每一帧都吃掉、特效就定住不动；
        // 不动画时恒为 0，去重行为和以前一样
        let phase_ms = if self.animating() { self.started.elapsed().as_millis() as u32 } else { 0 };
        let key = (text.clone(), status.clone(), width, height, scale, phase_ms / ANIM_STEP_MS);
        if self.last_frame.as_ref() == Some(&key) {
            return None;
        }
        self.last_frame = Some(key);

        // 布局：数字行 8x8 × (scale*2)，状态行 8x8 × scale，垂直居中。
        // 标称盒只按点阵算，TTF 字形从中线向下对基线、向上溢出的部分落进那条 gap，
        // 所以这套算式不随字形来源变化。
        let num_scale = scale * 2;
        let num_h = 8 * num_scale;
        let status_h = 8 * scale;
        let gap = 4 * scale;
        let total_h = (num_h + gap + status_h) as i32;
        let y_num = ((height as i32 - total_h) / 2).max(0);
        let y_status = y_num + (num_h + gap) as i32;
        let num_run = text::shape(&text, num_scale, y_num);
        let status_run = text::shape(&status, scale, y_status);

        Some(Frame {
            width,
            height,
            num_run,
            status_run,
            num_color,
            status_color,
            effect: self.config.text_effect,
            phase_ms,
            bg,
            num_scale,
            status_scale: scale,
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

    /// 最近一帧的状态行。内容只存在指纹里（`Frame` 不重复存一份），断言就从那儿读。
    fn last_status(w: &Widget) -> String {
        w.last_frame.as_ref().expect("还没有产出过帧").1.clone()
    }

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

    /// 状态行走 `Timer::pomo_status`，跑满组数后要换成 DONE。
    #[test]
    fn pomodoro_status_line_follows_the_phase_and_ends_at_done() {
        use crate::timer::Pomo;
        let cfg = Config {
            mode: Mode::Pomodoro,
            pomo: Pomo { work: 1, short_break: 1, long_break: 1, rounds: 1, cycles: 1 },
            ..Config::default()
        };
        let mut w = Widget::new(cfg, true);
        assert!(w.build_frame(200, 100, 1).is_some());
        assert_eq!(last_status(&w), "WORK 1");
        let t0 = Instant::now();
        w.timer.tick_at(t0);
        w.timer.tick_at(t0 + Duration::from_secs(1));
        assert!(w.build_frame(200, 100, 1).is_some());
        assert_eq!(last_status(&w), "LONG 1");
        // 长休息走完就是最后一组的结尾：停住并显示 DONE
        w.timer.tick_at(t0 + Duration::from_secs(2));
        assert!(w.build_frame(200, 100, 1).is_some(), "状态换了就该有新一帧");
        assert_eq!(last_status(&w), "DONE");
        assert!(!w.timer.running);
    }

    /// 点击穿透的输入区域必须同时罩住两行字形。这里把默认内容下的矩形钉成具体数字：
    /// `200×100` 画布、`scale=2` → 数字行 `1:00`(4 字 × 8 × 4 = 128px)、
    /// 状态行 `PAUSED`(6 字 × 8 × 2 = 96px)，垂直从 `y_num=22` 到状态行底 `78`。
    /// 改动包围盒算法（比如退回按字数算、或漏掉 TTF 向上溢出的部分）会立刻撞到这里。
    #[test]
    fn click_through_rect_spans_both_rows() {
        let cfg = Config { click_through: true, ..Config::default() };
        let mut w = Widget::new(cfg, false);
        let f = w.build_frame(200, 100, 2).expect("第一帧总要画");
        assert_eq!(f.input_rect(1.0), Some((36, 22, 128, 56)));
        // 没开穿透就整窗接收
        let mut plain = Widget::new(Config::default(), false);
        assert_eq!(plain.build_frame(200, 100, 2).unwrap().input_rect(1.0), None);
    }

    /// 特效开着时脏检查仍要生效：静态内容不该每帧重画。
    #[test]
    fn static_effect_still_dedupes_frames() {
        let cfg = Config { text_effect: Effect::Neon, ..Config::default() };
        let mut w = Widget::new(cfg, false);
        assert!(w.build_frame(200, 100, 1).is_some());
        assert!(w.build_frame(200, 100, 1).is_none(), "静态特效也该去重，否则白烧 CPU");
    }

    /// 动画特效 / 自动流动的渐变要把心跳提上去，否则 5fps 看不出流动。
    #[test]
    fn animating_content_shortens_the_heartbeat() {
        let mut w = Widget::new(Config::default(), false);
        assert_eq!(w.tick_interval(), DRAW_INTERVAL, "静态内容不该白醒");
        w.config.text_effect = Effect::Glow;
        assert_eq!(w.tick_interval(), DRAW_INTERVAL, "静态特效也不该提频");
        w.config.text_effect = Effect::Liquid;
        assert_eq!(w.tick_interval(), ANIM_INTERVAL);
        w.config.text_effect = Effect::None;
        // 只有超过两个停靠点的渐变才会自动流动
        w.config.color_running = Gradient::parse("#111111_#222222_#333333").unwrap();
        assert_eq!(w.tick_interval(), ANIM_INTERVAL);
        w.config.color_running = Gradient::parse("#111111_#222222").unwrap();
        assert_eq!(w.tick_interval(), DRAW_INTERVAL, "两停靠点是静止渐变");
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
