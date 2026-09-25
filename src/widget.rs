//! 与显示后端无关的挂件核心：计时推进、结束事件、帧内容与布局。
//!
//! 后端（`wl` 的 layer-shell / `x11` 的 override-redirect）只负责尺寸、缩放、
//! 输入事件与呈现，一帧的内容由 [`Widget::build_frame`] 产出；内容未变化时返回
//! `None`，后端据此整帧跳过（连 shm 都不提交）。

use std::time::{Duration, Instant};

use crate::tray::TrayHandle;

use crate::audio;
use crate::clock;
use crate::config::{
    COLOR_OPTIONS, Config, PALETTES, Watch, ZOOM_MAX, ZOOM_MIN, config_path, toggle_autostart,
};
use crate::effect::{self, Effect, Gradient};
use crate::render::{self, Canvas, premultiply};
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

/// 显示百分之一秒时的心跳间隔。Catime 的毫秒档同为 20ms：百分位每秒只翻 100 次，
/// 再快人眼也读不出两位数，而这 20ms 是本程序最贵的一档心跳。
pub const CENTIS_INTERVAL: Duration = Duration::from_millis(20);

/// 走针的时钟挂件（显示秒、不带百分秒）的心跳。读数一秒只翻一次，250ms 足以
/// 把它接住——Catime 的 `GetTimerInterval()` 在"时钟带秒"那一档用的也是 250ms。
pub const CLOCK_INTERVAL: Duration = Duration::from_millis(250);

/// 空闲档：没跑计时、没动画、也不是走针挂钟——屏幕上没有任何东西在变，醒一次
/// 只为接命令与探配置变更。命令到达靠 `wake` 管道提前唤醒（`src/wake.rs`），
/// 所以这一档不拿响应速度换电：托盘点击与套接字命令照样即时结算。
pub const IDLE_INTERVAL: Duration = Duration::from_millis(1000);

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
            effect::Row {
                run: &self.num_run,
                scale: self.num_scale,
            },
            effect::Row {
                run: &self.status_run,
                scale: self.status_scale,
            },
        ];
        effect::draw(
            canvas,
            &rows[..1],
            &self.num_color,
            self.effect,
            self.phase_ms,
        );
        effect::draw(
            canvas,
            &rows[1..],
            &self.status_color,
            self.effect,
            self.phase_ms,
        );
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
        let bottom =
            (self.num_run.bottom.max(self.status_run.bottom).max(0) as u32).min(self.height);
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
    /// 滚轮缩放倍数（`ZOOM_MIN`-`ZOOM_MAX`，持久化）。
    pub zoom: f32,
    tray: Option<TrayHandle>,
    /// 外部文本源：配了 `text_source` 才碰文件系统
    text_src: textsrc::Source,
    /// 特效动画的时钟原点。
    started: Instant,
    /// 挂件是否被藏起来：计时照常跑，只是不呈现（托盘「隐藏挂件」/ `--hide`）。
    ///
    /// 不进配置：退出时是隐藏状态，下次启动该是看得见的，否则用户只会以为程序没起来。
    hidden: bool,
    /// 编辑态：一档"我正在摆弄这个挂件"的临时状态（`🛠 编辑态` / 挂件中键 / `--edit`）。
    ///
    /// 不进配置，理由与 [`Widget::hidden`] 同：下次启动该是普通状态。它换来三件事——
    /// 整窗接收输入（点击穿透临时让路，于是拖/滚轮不必瞄准那两行字）、右键先考虑
    /// 退出编辑态而不是关掉挂件、到点静默（不弹通知也不执行结束命令）。
    edit: bool,
    /// 配置文件的变更探测：手改 `config.conf` 不用重启就生效（#17）。
    cfg_watch: Watch,
    /// **只存在于内存里**的一次性结束命令（`tinyticker 25m --and "..."`）。
    ///
    /// 存在的理由是安全而不是能力：`on_finish` 是常驻配置，一次误留就变成"每次
    /// 计时结束都关机"。这条走完一次自己清空，不需要谁记得去改配置文件。
    armed: Option<String>,
    /// 请求后端把窗口挪回出厂位置（`Command::Reset*` 置位，后端每拍取走一次）。
    reposition: bool,
    /// 上一帧的内容指纹（文本 / 状态 / 尺寸 / 字号 / 动画相位）。
    last_frame: Option<(String, String, u32, u32, u32, u32)>,
    /// 上一次回填给托盘的番茄段号（变化检测的基准，与 `hidden` 同步回填同手法）。
    last_pomo: Option<usize>,
    /// 上一次回填给托盘的倒计时进度（`tray_throttle = timer` 的驱动量；只在变化时推）。
    last_progress: u8,
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
            hidden: false,
            edit: false,
            cfg_watch: Watch::new(config_path()),
            armed: None,
            reposition: false,
            last_frame: None,
            last_pomo: None,
            last_progress: 0,
        }
    }

    /// 有没有需要持续重绘的东西：动画特效，或会自动流动的渐变。
    fn animating(&self) -> bool {
        self.config.text_effect.animated()
            || self.config.color_running.animated()
            || self.config.color_paused.animated()
            || self.config.color_done.animated()
    }

    /// 数字行这一帧要不要带百分秒。暂停时百分位原样冻住，显示不变——所以这一项
    /// **不看运行与否**，看的是"该不该画百分位"；心跳提不提频是另一回事（下面 `centis_live`）。
    /// 时钟挂件现在也覆盖（GAP §一"#15 没收掉的"那半句），但关掉显示秒时
    /// 百分秒一并压掉（与 Catime `drawing_time_format.c:255-263` 同规则）。
    fn shows_centis(&self) -> bool {
        self.config.centiseconds && (self.timer.mode != Mode::Clock || self.config.clock_seconds)
    }

    /// 百分位这一帧到底有没有在走：计时模式要看跑没跑，时钟挂件永远在走。
    /// 只有它才配把心跳提到 20ms——冻住的读数白醒只会烧 CPU。
    fn centis_live(&self) -> bool {
        self.shows_centis() && (self.timer.mode == Mode::Clock || self.timer.running)
    }

    /// 计时模式（非时钟、非到点顶替）下的数字行文本。
    fn timing_text(&self) -> String {
        if self.shows_centis() {
            render::format_centis(self.timer.display_centis(), self.config.time_pad)
        } else {
            render::format_time(self.timer.display_secs(), self.config.time_pad)
        }
    }

    /// 挂件当前是否被藏着（后端据此决定要不要呈现）。
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// 取走"把窗口挪回出厂位置"的请求（一次性的，取完就清）。
    pub fn take_reposition(&mut self) -> bool {
        let r = self.reposition;
        self.reposition = false;
        r
    }

    /// 心跳阶梯：空闲 1s → 走针时钟 250ms → 静态内容 200ms → 动画 50ms → 百分秒 20ms。
    ///
    /// 只有真在动的东西才提频：停住的读数百分位是冻的，白醒只会烧 CPU；空闲档
    /// 的即时响应靠 `wake` 管道，不靠多醒几次。
    pub fn tick_interval(&self) -> Duration {
        if self.hidden {
            // 藏着的时候没人看：计时照跑就留 200ms 档，连计时都没跑就降到最慢档
            return if self.timer.running || self.animating() {
                DRAW_INTERVAL
            } else {
                IDLE_INTERVAL
            };
        }
        if self.centis_live() {
            CENTIS_INTERVAL
        } else if self.animating() {
            ANIM_INTERVAL
        } else if self.timer.running {
            DRAW_INTERVAL
        } else if self.timer.mode == Mode::Clock && self.config.clock_seconds {
            CLOCK_INTERVAL
        } else {
            IDLE_INTERVAL
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
            Command::SetMode(mode) => {
                self.timer.set_mode(mode);
                // 模式单选是从套接字/命令行进来的（菜单那条自己 note 过），
                // 不回填的话勾会停在"上次点菜单的结果"上
                self.sync_tray();
            }
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
            Command::SetThrottle(t) => {
                if self.config.tray_throttle != t {
                    self.config.tray_throttle = t;
                    self.persist_appearance();
                }
            }
            Command::ToggleNumbers => {
                self.config.tray_numbers = !self.config.tray_numbers;
                self.persist_appearance();
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
            Command::SetRunningColor(i) => {
                // 值串就是配置文件里要写的那串，解析失败（不该发生）就什么都不做
                let Some(g) = COLOR_OPTIONS.get(i).and_then(|v| Gradient::parse(v)) else {
                    return false;
                };
                if self.config.color_running != g {
                    self.config.color_running = g;
                    self.persist_appearance();
                }
            }
            Command::SetEffect(effect) => {
                if self.config.text_effect != effect {
                    self.config.text_effect = effect;
                    self.persist_appearance();
                }
            }
            Command::ToggleCentiseconds => {
                self.config.centiseconds = !self.config.centiseconds;
                self.persist_appearance();
            }
            Command::SetCentiseconds(on) => {
                if self.config.centiseconds != on {
                    self.config.centiseconds = on;
                    self.persist_appearance();
                    // 套接字那条路不经过菜单：勾得自己跟上
                    self.sync_tray();
                }
            }
            Command::SetLanguage(lang) => {
                if self.config.language != lang {
                    self.config.language = lang;
                    crate::lang::set(lang);
                    self.persist_appearance();
                    // 标签在节点里，重建菜单才能看到换语言
                    self.sync_tray();
                }
            }
            Command::SetPomoStep(i) => {
                // 跳段只换读数；当前段的勾等下一拍 `tick()` 里的变化检测同步
                if self.timer.goto_step(i) {
                    self.invalidate();
                }
            }
            Command::SetTimePad(pad) => {
                if self.config.time_pad != pad {
                    self.config.time_pad = pad;
                    self.persist_appearance();
                }
            }
            Command::ToggleClockSeconds => {
                self.config.clock_seconds = !self.config.clock_seconds;
                self.persist_appearance();
            }
            Command::ToggleHidden => self.set_hidden(!self.hidden),
            Command::SetHidden(v) => self.set_hidden(v),
            Command::ToggleEdit => self.set_edit(!self.edit),
            Command::SetEdit(v) => self.set_edit(v),
            Command::ToggleNotify => {
                self.config.notify = !self.config.notify;
                self.persist_appearance();
            }
            Command::PreviewSound => {
                // 试听也走真实链路：同一份音量、同一个后台线程，响不响一听就知道
                if let Some(spec) = &self.config.alarm_sound {
                    audio::play(spec, self.config.alarm_volume);
                }
            }
            Command::ArmFinish(cmd) => self.armed = Some(cmd),
            Command::ToggleAutostart => match toggle_autostart() {
                Ok(on) => eprintln!(
                    "→ {}",
                    if on {
                        "已登记开机自启（~/.config/autostart/）"
                    } else {
                        "已取消开机自启"
                    }
                ),
                // 没有合格的 HOME / XDG_CONFIG_HOME 时本来也没地方放条目
                Err(e) => eprintln!("⚠️ 改开机自启失败: {e}"),
            },
            Command::ResetConfig => {
                self.apply_config(Config::default());
                self.reposition = true;
                self.persist_appearance();
                self.sync_tray();
                eprintln!("↺ 已恢复出厂设置（时长预设档位与 GIF 动图要重启才回到菜单上）");
            }
            Command::ResetPosition => {
                self.config.window_pos = None;
                self.reposition = true;
                self.persist_appearance();
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
        // 把自己的写入对齐进基准，否则下一拍会把它当成"用户手改了配置"再绕一圈回来
        self.cfg_watch.sync();
    }

    /// 把配置派生的勾选态回填给托盘（GAP §七那条漂移的收口）。
    ///
    /// 快照里把 `mode` 盖成计时器的实时值：配置里那个是"启动模式"，菜单的勾
    /// 该说"现在在跑什么"。只在不经过菜单的那几条路（套接字/热加载/命令行命令）
    /// 调用——菜单自己 note 过，再绕一圈只是白费一次节点重建。
    fn sync_tray(&self) {
        let Some(handle) = &self.tray else { return };
        let mut snap = self.config.clone();
        snap.mode = self.timer.mode;
        tray::sync_config(handle, &snap);
    }

    /// 手改配置文件不用重启（GAP #17）。每拍最多 stat 一次，真变了才读文件。
    fn reload_config(&mut self) {
        if !self.cfg_watch.changed(Instant::now()) {
            return;
        }
        let next = Config::load();
        // 值一样就什么都不做：自己的写入、编辑器的原子替换、只改注释都能走到这里
        if next == self.config {
            return;
        }
        // 托盘图标不用重启了：`SyncConfig` 会把那一格回填到新值，图标每拍从
        // state 取。真正还卡在启动时的只剩三样：预设档位与番茄分段（菜单结构）、
        // GIF 路径（解码器建一次）、窗口位置（surface 归后端）。
        let restart = next.presets != self.config.presets
            || next.pomo.seq != self.config.pomo.seq
            || next.tray_gif != self.config.tray_gif
            || next.window_pos != self.config.window_pos;
        self.apply_config(next);
        self.sync_tray();
        eprintln!(
            "↻ 配置已热加载{}",
            if restart {
                "（时长预设 / 番茄分段 / GIF 动图 / 窗口位置要重启才生效）"
            } else {
                ""
            }
        );
    }

    /// 把一份新配置落到挂件与计时器上。拆出来是为了能在不碰真实配置文件的前提下测它。
    fn apply_config(&mut self, next: Config) {
        if next.text_source != self.config.text_source {
            self.text_src = textsrc::Source::new(next.text_source.as_deref());
        }
        if next.status_font_px != self.config.status_font_px {
            text::set_px_per_cell(next.status_font_px);
        }
        if next.language != self.config.language {
            crate::lang::set(next.language);
        }
        let running = self.timer.running;
        self.config = next;
        self.timer.set_pomo(&self.config.pomo);
        // 正在跑的时候不动模式与总时长：那是用户此刻正在用的东西，改配置文件
        // 不该把它掐掉；空着的时候才跟着文件走
        if !running {
            if self.timer.mode != self.config.mode {
                self.timer.set_mode(self.config.mode); // 换模式自带 reset
            }
            if self.timer.total != self.config.duration_secs {
                self.timer.total = self.config.duration_secs;
                self.timer.reset();
            }
        }
        // 后端每拍从 `Widget::zoom` 结算窗口尺寸，所以这里只改值
        self.zoom = self.config.zoom;
        self.invalidate();
    }

    /// 这次结束该执行哪条命令：一次性的优先且用完清空，否则回到常驻的 `on_finish`。
    ///
    /// 只有倒计时归零与番茄钟专注完成算"计时结束"——休息结束不许把待执行的那条
    /// 一次性命令悄悄吃掉。
    fn take_finish_cmd(&mut self, ev: Finished) -> Option<String> {
        if !matches!(ev, Finished::Countdown | Finished::PomodoroWork) {
            return None;
        }
        self.armed.take().or_else(|| self.config.on_finish.clone())
    }

    /// 推进计时，并处理本次产生的结束事件（通知 + on_finish 命令）。
    pub fn tick(&mut self) {
        self.text_src.refresh();
        self.reload_config();
        self.timer.maybe_tick();
        // 番茄分段的当前段号变了才推一次：菜单那一串的勾读的是计时器的真值
        let step = self.timer.pomo_step();
        if step != self.last_pomo {
            self.last_pomo = step;
            if let Some(handle) = &self.tray {
                tray::sync_pomo(handle, step);
            }
        }
        // `tray_throttle = timer`：倒计时进度 1% 一格推给托盘（不跑就归 0）。
        // 托盘侧只存最新值，多实例消息不排队——这是给动画看的新鲜数，不是事件。
        if self.config.tray_throttle == tray::Throttle::Timer {
            let pct = match self.timer.mode {
                Mode::Countdown
                    if self.timer.running && self.timer.total > 0 && !self.timer.is_done() =>
                {
                    (100 - self.timer.display_secs() * 100 / self.timer.total).clamp(0, 100) as u8
                }
                Mode::Pomodoro
                    if self.timer.running && self.timer.total > 0 && !self.timer.is_done() =>
                {
                    // 番茄钟的"当前段"进度：total 是阶段时长，语义与 Catime 的
                    // TIMER 档一致（它按 CLOCK_TOTAL_TIME 算的也是"当前这段"）
                    (100 - self.timer.display_secs() * 100 / self.timer.total).clamp(0, 100) as u8
                }
                _ => 0,
            };
            if pct != self.last_progress {
                self.last_progress = pct;
                if let Some(handle) = &self.tray {
                    tray::sync_progress(handle, pct);
                }
            }
        }
        let Some(ev) = self.timer.take_finished() else {
            return;
        };
        // 编辑态期间到点只把读数换成 DONE：不弹通知、也不执行结束命令——"我在摆弄
        // 这个挂件"不该被一次跑完的倒计时打断，更不该顺手把关机那条触发掉。
        // 一次性武装（`armed`）因此也不被消费，退出编辑态后仍然算数。
        if self.edit {
            eprintln!("🛠 编辑态：本次结束事件只改了读数，未发通知、未执行结束命令");
            return;
        }
        let (body_zh, body_en, act_zh, act_en) = match ev {
            Finished::Countdown => ("⏰ 计时结束", "⏰ Time's up", "再来一次", "Again"),
            Finished::PomodoroWork => (
                "🍅 专注完成，休息一下",
                "🍅 Focus done — take a break",
                "好的",
                "OK",
            ),
            Finished::PomodoroStep => (
                "🔁 这一段结束，继续下一段",
                "🔁 Step done — continuing",
                "继续",
                "Next",
            ),
            Finished::PomodoroBreak => (
                "☕ 休息结束，继续专注",
                "☕ Break over — back to focus",
                "开始",
                "Start",
            ),
            Finished::PomodoroLongBreak => (
                "🌿 长休息结束，开始下一组",
                "🌿 Long break over",
                "开始",
                "Start",
            ),
            Finished::PomodoroAllDone => (
                "🎉 番茄钟全部完成",
                "🎉 Pomodoro complete",
                "再来一组",
                "Again",
            ),
        };
        let (body, action) = (
            crate::lang::tr(body_zh, body_en),
            crate::lang::tr(act_zh, act_en),
        );
        // 通知可以整个关掉（`on_finish` 照旧执行），也可以把正文写死成一句话——
        // 写死时五种事件共用同一句，那是用户的选择而不是我们的
        if self.config.notify {
            let text = self.config.notify_text.as_deref().unwrap_or(body);
            if let Some(handle) = &self.tray {
                tray::notify(handle, text, action);
            }
        }
        // 提示音与通知各走各的开关（Catime 同构：文件/音量独立于 Toast）。
        // 内部是后台线程，这里不等它；音量 0 / 未配都不出声。
        if let Some(spec) = &self.config.alarm_sound {
            audio::play(spec, self.config.alarm_volume);
        }
        // 锁屏 / 关机 / 打开文件等场景都由用户命令覆盖
        let had_armed = self.armed.is_some();
        if let Some(cmd) = self.take_finish_cmd(ev) {
            if had_armed {
                eprintln!("↦ 执行一次性结束命令（`--and`，只此一次）");
            }
            // 后台线程等待子进程退出，避免僵尸进程，也不阻塞 UI
            std::thread::spawn(move || {
                let spawned = std::process::Command::new("sh").arg("-c").arg(&cmd).spawn();
                match spawned {
                    Ok(mut child) => {
                        let _ = child.wait();
                    }
                    Err(e) => eprintln!("⚠️ 结束命令执行失败: {e}"),
                }
            });
        }
    }

    /// 滚轮缩放：`dy` 为带符号的格数；返回 `true` 表示倍数确实变了。
    pub fn zoom_by(&mut self, dy: f32) -> bool {
        let zoom = (self.zoom + dy * 0.25).clamp(ZOOM_MIN, ZOOM_MAX);
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
        // 编辑态优先占住这一行：那是模式提示（"此刻按右键是退出编辑态"），
        // 比外部文本的内容更要紧——用户得先看得见自己在哪一档。
        let status = if self.edit {
            "EDIT".to_string()
        } else {
            match self.text_src.text() {
                Some(external) => text::fit(external, scale, width),
                None => status,
            }
        };
        let status_color = Gradient::solid(0xA0A0AA);
        // 数字行字号只依赖传进来的 scale，所以先算：到点顶替的自定义文本要按
        // 同一字号裁剪，晚一步就拿不到正确的宽度了
        let num_scale = scale * 2;
        // 时钟模式实时读取本地时间；其余模式显示计时数。到点且配了 `timeout_text`
        // 时整行换成那句话，`"0"` 是"留空"的哨兵值（状态行仍写 DONE，不会以为程序没了）
        let text = if self.timer.mode == Mode::Clock {
            let raw = if self.config.centiseconds && self.config.clock_seconds {
                let (h, m, s, cs) = clock::now_hms_cs();
                clock::format_clock_cs(h, m, s, cs, self.config.clock_12h)
            } else {
                let (h, m, s) = clock::now_hms();
                clock::format_clock(h, m, s, self.config.clock_12h, self.config.clock_seconds)
            };
            // 12 小时制 + 百分秒是 14 字符，比默认窗口宽：按实测像素宽整字退让，
            // 而不是把半个数字裁掉悬在边外
            text::fit(&raw, num_scale, width)
        } else if self.timer.is_done() {
            match self.config.timeout_text.as_deref() {
                Some("0") => String::new(),
                Some(t) => text::fit(t, num_scale, width),
                None => self.timing_text(),
            }
        } else {
            self.timing_text()
        };

        // 动画相位要进指纹，否则内容没变时脏检查会把每一帧都吃掉、特效就定住不动；
        // 不动画时恒为 0，去重行为和以前一样
        let phase_ms = if self.animating() {
            self.started.elapsed().as_millis() as u32
        } else {
            0
        };
        let key = (
            text.clone(),
            status.clone(),
            width,
            height,
            scale,
            phase_ms / ANIM_STEP_MS,
        );
        if self.last_frame.as_ref() == Some(&key) {
            return None;
        }
        self.last_frame = Some(key);

        // 布局：数字行 8x8 × (scale*2)，状态行 8x8 × scale，垂直居中。
        // 标称盒只按点阵算，TTF 字形从中线向下对基线、向上溢出的部分落进那条 gap，
        // 所以这套算式不随字形来源变化。
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
            // 编辑态临时压过点击穿透：正在摆弄挂件的时候整窗都该接得到手，
            // 不该去瞄那两行字。退出编辑态自动还原，配置项一个字都不用改。
            click_through: self.config.click_through && !self.edit,
        })
    }

    /// 让下一帧强制重绘。两个用途：X11 的 `Expose`（窗口被揭开后像素已丢），
    /// 以及改了颜色/透明度之后——帧指纹只含文本与尺寸，不清它就会把这次重绘吃掉。
    pub fn invalidate(&mut self) {
        self.last_frame = None;
    }

    /// 隐藏 / 显示挂件。隐藏期间后端整帧跳过呈现，计时器照常按真实时长推进。
    fn set_hidden(&mut self, hidden: bool) {
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden;
        // 重新显示时必须重画：脏检查只看内容指纹，而内容在隐藏期间可能压根没变过，
        // 不 invalidate 的话表面会一直停在"没有缓冲"的空白状态
        self.invalidate();
        // `--hide` / `--show` 从套接字进来，不经过菜单：那一格的勾要自己跟上
        if let Some(handle) = &self.tray {
            tray::sync_hidden(handle, hidden);
        }
    }

    /// 进 / 出编辑态。只改内存并标脏：输入区域与状态行都在帧里，不重画就等于没改。
    fn set_edit(&mut self, on: bool) {
        if self.edit == on {
            return;
        }
        self.edit = on;
        // 右键的含义跟着变了，得说清楚——否则用户按右键期待"退出编辑态"，
        // 却把整个程序关掉
        eprintln!(
            "🛠 编辑态{}：{}",
            if on { "开" } else { "关" },
            if on {
                "整窗接收输入，到点静默；右键退出编辑态"
            } else {
                "输入区域与右键都回到普通态"
            }
        );
        self.invalidate();
        // 菜单里那一格要跟着真值走：中键与套接字这两条路都不经过托盘，
        // 不回填的话它显示的永远是"上次点菜单的结果"（GAP §七 那条漂移）
        if let Some(handle) = &self.tray {
            tray::sync_edit(handle, on);
        }
    }

    /// 挂件中键：翻一档编辑态（后端的手势入口）。
    pub fn toggle_edit(&mut self) {
        self.set_edit(!self.edit);
    }

    /// 挂件上按右键：编辑态下先退出编辑态，否则按原来的语义退出程序。
    /// 返回 `true` 表示后端应当收尾退出。
    pub fn right_click(&mut self) -> bool {
        if self.edit {
            self.set_edit(false);
            return false;
        }
        true
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
    use crate::render::Pad;
    use std::time::Instant;

    /// 最近一帧的状态行。内容只存在指纹里（`Frame` 不重复存一份），断言就从那儿读。
    fn last_status(w: &Widget) -> String {
        w.last_frame.as_ref().expect("还没有产出过帧").1.clone()
    }

    /// 最近一帧的数字行，读法同上——指纹的第 0 格。
    fn last_text(w: &Widget) -> String {
        w.last_frame.as_ref().expect("还没有产出过帧").0.clone()
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
        let cfg = Config {
            duration_secs: 2,
            ..Config::default()
        };
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
            pomo: Pomo {
                work: 1,
                short_break: 1,
                long_break: 1,
                rounds: 1,
                cycles: 1,
                seq: Vec::new(),
            },
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
        let cfg = Config {
            click_through: true,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        let f = w.build_frame(200, 100, 2).expect("第一帧总要画");
        assert_eq!(f.input_rect(1.0), Some((36, 22, 128, 56)));
        // 没开穿透就整窗接收
        let mut plain = Widget::new(Config::default(), false);
        assert_eq!(
            plain.build_frame(200, 100, 2).unwrap().input_rect(1.0),
            None
        );
    }

    /// 特效开着时脏检查仍要生效：静态内容不该每帧重画。
    #[test]
    fn static_effect_still_dedupes_frames() {
        let cfg = Config {
            text_effect: Effect::Neon,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        assert!(w.build_frame(200, 100, 1).is_some());
        assert!(
            w.build_frame(200, 100, 1).is_none(),
            "静态特效也该去重，否则白烧 CPU"
        );
    }

    /// 隐藏只降心跳、不改计时，并且一定要标脏——否则放回来那一帧会被指纹吃掉。
    #[test]
    fn hiding_slows_the_heartbeat_and_forces_a_redraw() {
        let cfg = Config {
            centiseconds: true,
            duration_secs: 60,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, true);
        assert_eq!(w.tick_interval(), CENTIS_INTERVAL, "跑动中该是 20ms");
        assert!(!w.handle_cmd(Command::ToggleHidden), "隐藏不是退出命令");
        assert!(w.hidden());
        assert_eq!(
            w.tick_interval(),
            DRAW_INTERVAL,
            "藏着但计时在跑：留 200ms 档"
        );
        // 计时照常按真实时长推进
        let t0 = Instant::now();
        w.timer.tick_at(t0);
        w.timer.tick_at(t0 + Duration::from_secs(30));
        assert_eq!(w.timer.display_secs(), 30, "隐藏期间不该停表");
        // 藏着且没在跑：直接睡到空闲档
        w.handle_cmd(Command::Pause);
        assert_eq!(
            w.tick_interval(),
            IDLE_INTERVAL,
            "藏着又没东西在走，醒那么勤干嘛"
        );
        assert!(!w.handle_cmd(Command::SetHidden(false)));
        assert!(!w.hidden());
        w.timer.start();
        assert_eq!(w.tick_interval(), CENTIS_INTERVAL);
        assert!(w.build_frame(200, 100, 1).is_some(), "放回来必须重画一帧");
        assert!(w.build_frame(200, 100, 1).is_none(), "之后照常去重");
        // 设定值命令是幂等的：重复执行不会再翻一次
        w.handle_cmd(Command::SetHidden(false));
        assert!(!w.hidden());
    }

    /// 编辑态把输入区域放开到整窗，状态行要说清楚自己在哪一档，右键先退回普通态。
    ///
    /// 输入矩形那两个具体数字来自 `click_through_rect_spans_both_rows`；这里只验
    /// "编辑态要把它换成整窗"，退出后必须逐位还原回矩形。
    #[test]
    fn edit_mode_widens_input_and_takes_the_right_button_over() {
        let cfg = Config {
            click_through: true,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        let f = w.build_frame(200, 100, 2).expect("第一帧总要画");
        assert_eq!(
            f.input_rect(1.0),
            Some((36, 22, 128, 56)),
            "普通态只接收那两行字"
        );

        w.toggle_edit();
        let f = w.build_frame(200, 100, 2).expect("换档就该重画一帧");
        assert_eq!(last_status(&w), "EDIT", "状态行要占住一格说明自己在编辑态");
        assert_eq!(f.input_rect(1.0), None, "编辑态整窗可点");

        // 编辑态的右键是"退出编辑态"，不是把挂件关掉
        assert!(!w.right_click(), "编辑态下右键不该退出程序");
        assert!(!w.edit);
        let f = w.build_frame(200, 100, 2).expect("退回普通态也要重画");
        assert_eq!(
            last_status(&w),
            "PAUSED",
            "退回普通态就该把状态行还给计时器"
        );
        assert_eq!(f.input_rect(1.0), Some((36, 22, 128, 56)));
        assert!(w.right_click(), "普通态的右键仍然是关掉挂件");
    }

    /// 中键与托盘 / 套接字那条命令走的是同一份状态；设定值命令要幂等，
    /// 因为 `tinyticker --edit` 可以被反复敲。
    #[test]
    fn edit_mode_shares_one_state_and_set_is_idempotent() {
        let mut w = Widget::new(Config::default(), false);
        w.handle_cmd(Command::SetEdit(true));
        assert!(w.edit);
        w.handle_cmd(Command::SetEdit(true));
        assert!(w.edit, "重复设定不该把它翻回去");
        w.handle_cmd(Command::SetEdit(false));
        assert!(!w.edit);
        w.handle_cmd(Command::ToggleEdit);
        assert!(w.edit, "切换命令走的是同一格状态");
        // 编辑态挂在 `Widget` 而不是 `Config` 上（同 `hidden`）：退出时不会留下
        // 一个"右键关不掉"的挂件，这一点由类型本身保证
    }

    /// 编辑态期间到点只改读数：不发通知、不执行结束命令，而一次性武装**不被吃掉**
    /// （否则用户调完外观回来，那次 `--and` 早就悄悄没了）。
    #[test]
    fn edit_mode_swallows_the_finish_event_without_consuming_the_arming() {
        let cfg = Config {
            duration_secs: 1,
            on_finish: Some("echo persistent".into()),
            ..Config::default()
        };
        let mut w = Widget::new(cfg, true);
        w.handle_cmd(Command::ArmFinish("echo once".into()));
        w.toggle_edit();
        let t0 = Instant::now();
        w.timer.tick_at(t0);
        w.timer.tick_at(t0 + Duration::from_secs(2));
        w.tick();
        assert!(w.timer.is_done(), "读数照常被换成 DONE");
        assert_eq!(
            w.armed.as_deref(),
            Some("echo once"),
            "到点静默不等于把武装消费掉"
        );
        // 退出编辑态后由下一次结束来兑现它（执行那半边由 `armed_finish_command_is_one_shot` 覆盖）
        w.toggle_edit();
        assert_eq!(
            w.take_finish_cmd(Finished::Countdown).as_deref(),
            Some("echo once")
        );
    }

    /// 热加载：空着的计时器跟着文件走，正在跑的不动；缩放与配色当场生效。
    #[test]
    fn apply_config_follows_the_file_only_when_idle() {
        let idle = Config {
            duration_secs: 60,
            mode: Mode::Countdown,
            ..Config::default()
        };
        let mut w = Widget::new(idle, false);
        assert_eq!(w.timer.total, 60);
        w.apply_config(Config {
            mode: Mode::Stopwatch,
            duration_secs: 300,
            ..Config::default()
        });
        assert_eq!(
            (w.timer.mode, w.timer.total),
            (Mode::Stopwatch, 300),
            "空着时该跟文件走"
        );
        assert_eq!(w.timer.display_secs(), 0);
        w.apply_config(Config {
            mode: Mode::Countdown,
            duration_secs: 45,
            ..Config::default()
        });
        w.timer.start();
        w.apply_config(Config {
            mode: Mode::Pomodoro,
            duration_secs: 900,
            ..Config::default()
        });
        assert_eq!(w.timer.mode, Mode::Countdown, "跑动中不该被配置文件掐掉");
        assert_eq!(w.timer.total, 45);
        assert!(w.timer.running);
        w.apply_config(Config {
            zoom: 2.0,
            bg_alpha: 128,
            ..Config::default()
        });
        assert_eq!(w.zoom, 2.0, "缩放是当场结算的");
        assert_eq!(w.config.bg_alpha, 128);
    }

    /// 恢复默认设置：显示相关的都回到出厂值，跑动中的计时器不被掐掉，
    /// 并且把"挪回出厂位置"的请求挂上（后端取走一次就该清掉）。
    #[test]
    fn reset_restores_defaults_without_killing_a_running_timer() {
        let cfg = Config {
            duration_secs: 900,
            bg_alpha: 200,
            centiseconds: true,
            time_pad: Pad::Full,
            zoom: 2.5,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, true);
        assert!(!w.handle_cmd(Command::ResetConfig));
        assert_eq!(w.config.bg_alpha, Config::default().bg_alpha);
        assert!(!w.config.centiseconds);
        assert_eq!(w.config.time_pad, Pad::None);
        assert_eq!(w.zoom, 1.0, "缩放跟着配置回到出厂值");
        assert!(
            w.timer.running && w.timer.total == 900,
            "正在跑的倒计时不该被重置打断"
        );
        assert!(w.take_reposition(), "重置要顺带请求挪回出厂位置");
        assert!(!w.take_reposition(), "请求是一次性的");
        // 单独重置位置：只清 window_pos，不动别的
        let mut w2 = Widget::new(
            Config {
                bg_alpha: 200,
                ..Config::default()
            },
            false,
        );
        w2.handle_cmd(Command::ResetPosition);
        assert_eq!(w2.config.window_pos, None);
        assert_eq!(w2.config.bg_alpha, 200, "重置位置不该牵连外观");
        assert!(w2.take_reposition());
    }

    /// 一次性结束命令优先、用完清空，且不相关的事件不许把它吃掉。
    #[test]
    fn armed_finish_command_is_one_shot() {
        let cfg = Config {
            on_finish: Some("echo persistent".into()),
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        assert_eq!(
            w.take_finish_cmd(Finished::Countdown).as_deref(),
            Some("echo persistent")
        );
        w.handle_cmd(Command::ArmFinish("echo once".into()));
        assert_eq!(w.armed.as_deref(), Some("echo once"));
        // 休息结束不是"计时结束"：不该执行，也不该把待执行的那条吃掉
        assert_eq!(w.take_finish_cmd(Finished::PomodoroBreak), None);
        // `pomo_seq` 的中间段同理：它不是"计时结束"
        assert_eq!(w.take_finish_cmd(Finished::PomodoroStep), None);
        assert_eq!(
            w.armed.as_deref(),
            Some("echo once"),
            "不相关的事件不该消费武装"
        );
        // 对上了就用一次性的，用完回到常驻配置
        assert_eq!(
            w.take_finish_cmd(Finished::Countdown).as_deref(),
            Some("echo once")
        );
        assert!(w.armed.is_none());
        assert_eq!(
            w.take_finish_cmd(Finished::PomodoroWork).as_deref(),
            Some("echo persistent")
        );
        // 热加载不该把武装中的命令冲掉：它本来就不在配置里
        w.handle_cmd(Command::ArmFinish("echo twice".into()));
        w.apply_config(Config::default());
        assert_eq!(w.armed.as_deref(), Some("echo twice"));
    }

    /// 到点顶替数字行：自定义文本（含中文）走字形层，`"0"` 是留空，没配就照旧显示 0。
    #[test]
    fn timeout_text_replaces_the_number_row() {
        let t0 = Instant::now();
        let done = |w: &mut Widget| {
            w.timer.tick_at(t0);
            w.timer.tick_at(t0 + Duration::from_secs(2));
            assert!(w.timer.is_done());
            assert!(w.build_frame(200, 100, 1).is_some());
        };
        let cfg = Config {
            duration_secs: 1,
            timeout_text: Some("时间到".into()),
            ..Config::default()
        };
        let mut w = Widget::new(cfg, true);
        done(&mut w);
        assert_eq!(last_text(&w), "时间到");
        assert_eq!(last_status(&w), "DONE", "状态行仍要说清楚是结束了");

        let blank = Config {
            duration_secs: 1,
            timeout_text: Some("0".into()),
            ..Config::default()
        };
        let mut w2 = Widget::new(blank, true);
        done(&mut w2);
        assert_eq!(last_text(&w2), "");

        let mut w3 = Widget::new(
            Config {
                duration_secs: 1,
                ..Config::default()
            },
            true,
        );
        done(&mut w3);
        assert_eq!(last_text(&w3), "0s", "没配就该保持原来的样子");
    }

    /// 走动的百分秒把心跳提到 20ms；停住后回空闲档。时钟挂件现在也吃这一档
    /// （读数自己在走），但只有"显示秒"开着时才提频。
    #[test]
    fn running_centiseconds_shorten_the_heartbeat() {
        let cfg = Config {
            centiseconds: true,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        assert_eq!(w.tick_interval(), IDLE_INTERVAL, "没跑起来就不该白醒");
        w.timer.start();
        assert_eq!(w.tick_interval(), CENTIS_INTERVAL);
        w.timer.pause();
        assert_eq!(w.tick_interval(), IDLE_INTERVAL, "冻住的读数不需要 50fps");
        // 时钟挂件开了百分秒：不需要"开始"，它在走的永远是系统时间
        w.handle_cmd(Command::SetMode(Mode::Clock));
        assert_eq!(
            w.tick_interval(),
            CENTIS_INTERVAL,
            "百分秒现在也覆盖时钟挂件"
        );
        // 关掉显示秒，百分秒一并压掉（Catime 同规则），也就用不着 20ms
        // （直接改字段：Toggle* 会 `config.save()`，单测不许写用户的真配置）
        w.config.clock_seconds = false;
        assert_eq!(
            w.tick_interval(),
            IDLE_INTERVAL,
            "关秒后既不显百分秒也不该提频"
        );
        // 和动画特效同时开着时取最快那一档
        w.config.clock_seconds = true;
        w.handle_cmd(Command::SetMode(Mode::Stopwatch));
        w.config.text_effect = Effect::Liquid;
        w.timer.start();
        assert_eq!(
            w.tick_interval(),
            CENTIS_INTERVAL,
            "20ms 那一档顺带也带动了动画"
        );
    }

    /// 阶梯下探的两档（GAP §一"成本 XS"那行）：走针时钟 250ms，其余静态一律 1s。
    #[test]
    fn heartbeat_ladder_reaches_idle_and_clock_tiers() {
        // 暂停的倒计时：屏幕上什么都没动 → 空闲档
        let w = Widget::new(
            Config {
                duration_secs: 60,
                ..Config::default()
            },
            false,
        );
        assert_eq!(w.tick_interval(), IDLE_INTERVAL);
        // 跑动的倒计时读数在走 → 200ms 原档
        let mut run = Widget::new(
            Config {
                duration_secs: 60,
                ..Config::default()
            },
            true,
        );
        assert_eq!(run.tick_interval(), DRAW_INTERVAL, "跑动中的整秒档不变");
        run.handle_cmd(Command::Pause);
        assert_eq!(run.tick_interval(), IDLE_INTERVAL, "暂停后该降到空闲档");
        // 时钟带秒 → 250ms；时钟关秒 → 一分钟才变一次，空闲档就够
        let mut clock = Widget::new(
            Config {
                mode: Mode::Clock,
                ..Config::default()
            },
            false,
        );
        assert_eq!(clock.tick_interval(), CLOCK_INTERVAL);
        clock.config.clock_seconds = false; // 同左：不走 `Toggle*` 免写盘
        assert_eq!(clock.tick_interval(), IDLE_INTERVAL);
    }

    /// 数字行按 `centiseconds` 换格式。没在跑时百分位冻在 `.00`，读数是确定的。
    #[test]
    fn centiseconds_replace_the_number_row() {
        let cfg = Config {
            centiseconds: true,
            duration_secs: 45,
            ..Config::default()
        };
        let mut w = Widget::new(cfg, false);
        assert!(w.build_frame(200, 100, 1).is_some());
        assert_eq!(last_text(&w), "45.00s");
        w.config.centiseconds = false;
        w.invalidate();
        assert!(w.build_frame(200, 100, 1).is_some());
        assert_eq!(last_text(&w), "45s", "关掉就该回到原来的写法");
    }

    /// 动画特效 / 自动流动的渐变要把心跳提上去，否则 5fps 看不出流动。
    #[test]
    fn animating_content_shortens_the_heartbeat() {
        let mut w = Widget::new(Config::default(), false);
        assert_eq!(w.tick_interval(), IDLE_INTERVAL, "静态内容该睡到空闲档");
        w.config.text_effect = Effect::Glow;
        assert_eq!(w.tick_interval(), IDLE_INTERVAL, "静态特效也不该提频");
        w.config.text_effect = Effect::Liquid;
        assert_eq!(w.tick_interval(), ANIM_INTERVAL);
        w.config.text_effect = Effect::None;
        // 只有超过两个停靠点的渐变才会自动流动
        w.config.color_running = Gradient::parse("#111111_#222222_#333333").unwrap();
        assert_eq!(w.tick_interval(), ANIM_INTERVAL);
        w.config.color_running = Gradient::parse("#111111_#222222").unwrap();
        assert_eq!(w.tick_interval(), IDLE_INTERVAL, "两停靠点是静止渐变");
    }

    /// 外观命令带磁盘写（`config.save()`），不适合在单测里走；这里只测它的另一半：
    /// 改颜色必须 invalidate 才能到屏幕上。
    #[test]
    fn color_change_needs_an_invalidate_to_reach_the_screen() {
        let mut w = Widget::new(Config::default(), false);
        assert!(w.build_frame(200, 100, 2).is_some(), "第一帧总要画");
        assert!(
            w.build_frame(200, 100, 2).is_none(),
            "内容没变就该跳过这一帧"
        );
        // 只改透明度：指纹（文本/状态/尺寸/字号）完全不变
        w.config.bg_alpha = 200;
        assert!(
            w.build_frame(200, 100, 2).is_none(),
            "指纹不含颜色，不 invalidate 就会被吃掉"
        );
        w.invalidate();
        assert!(w.build_frame(200, 100, 2).is_some());
    }
}
