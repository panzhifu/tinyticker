//! 菜单：节点表 + 托盘自持的那份显示状态（`State`），以及 `com.canonical.dbusmenu` 的读写。
//!
//! 标签全部走 `lang::tr_in` 的**显式语言版**：`build_nodes` 收一个 `Language` 参数，
//! 单测断言具体字面量时不碰进程级全局，并行测试也不会互相掰语言。切换语言由
//! `TrayMsg::SyncConfig` 那条路重建节点（托盘线程重建，主循环只负责写回配置）。

use super::*;
use crate::config::{
    ALPHA_STEPS, COLOR_OPTIONS, Config, PALETTES, PRESET_PAGE, color_label, preset_label_in,
};
use crate::effect::{EFFECTS, Effect, Gradient};
use crate::lang::{Language, tr_in};
use crate::render::{PADS, Pad};
use crate::timer::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Root,
    Button,
    Separator,
    Submenu,
}

pub(super) struct Node {
    pub(super) kind: Kind,
    pub(super) label: String,
    pub(super) command: Option<Command>,
    pub(super) children: Vec<i32>,
}

impl Command {
    /// 这一项在单选组里的选中判据；非单选项返回 `None`。
    ///
    /// 从命令本身推导而不是另存一份字段，加按钮时不可能忘记登记，也不会接错线。
    pub(super) fn check(&self) -> Option<Check> {
        match *self {
            Command::SetMode(m) => Some(Check::Mode(m)),
            Command::SetAlpha(a) => Some(Check::Alpha(a)),
            Command::SetPalette(i) => Some(Check::Palette(i)),
            Command::SetRunningColor(i) => Some(Check::RunningColor(i)),
            Command::SetEffect(e) => Some(Check::Effect(e)),
            Command::SetIcon(m) => Some(Check::Icon(m)),
            Command::ToggleNumbers => Some(Check::Numbers),
            Command::SetThrottle(t) => Some(Check::Throttle(t)),
            Command::ToggleCentiseconds => Some(Check::Centiseconds),
            Command::SetCentiseconds(v) => Some(Check::Centi(v)),
            Command::SetTimePad(p) => Some(Check::TimePad(p)),
            Command::ToggleClockSeconds => Some(Check::ClockSeconds),
            Command::ToggleHidden => Some(Check::Hidden),
            Command::ToggleEdit => Some(Check::Edit),
            Command::ToggleNotify => Some(Check::Notify),
            Command::ToggleAutostart => Some(Check::Autostart),
            Command::SetLanguage(l) => Some(Check::Language(l)),
            Command::SetPomoStep(i) => Some(Check::PomoStep(i)),
            Command::SetHidden(_) | Command::SetEdit(_) => None,
            _ => None,
        }
    }
}

/// `checked` 取自托盘自持状态的哪一格。
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Check {
    Mode(Mode),
    Alpha(u8),
    Palette(usize),
    RunningColor(usize),
    Effect(Effect),
    Icon(IconMode),
    Throttle(Throttle),
    /// 数字还是水位（`图标内容` 子菜单末尾那一项）。
    Numbers,
    /// 下面三个是勾选框（各自独立），上面五个是单选组。勾选框只有一项，不必带值。
    Centiseconds,
    /// 设定值版的百分秒（套接字那条路），勾与勾选框共用同一格状态。
    Centi(bool),
    TimePad(Pad),
    ClockSeconds,
    Hidden,
    /// 编辑态那一格（同样只影响一行勾选）。
    Edit,
    Notify,
    Autostart,
    /// 语言单选三档（`language` 的**配置值**，不是 resolve 之后的——`auto` 是一档独立的选择）。
    Language(Language),
    /// `pomo_seq` 的每一段一项，当前段打勾。
    PomoStep(usize),
}

/// 托盘自己维护的一份显示状态。
///
/// 菜单要显示「当前选中的是哪一项」，而这几项只有托盘会改（初值来自配置），
/// 所以在发出命令的同时就地镜像一份。热加载与套接字那两条不经过菜单的路，
/// 由主循环经 `TrayMsg::SyncConfig` 回填（GAP §七 那条"勾选态漂移"）。
pub(super) struct State {
    pub(super) mode: Mode,
    pub(super) alpha: u8,
    /// 配置里的四色恰好等于某套预设时才是 `Some`；用户手改过颜色就是 `None`
    pub(super) palette: Option<usize>,
    /// 当前运行色恰好是 `COLOR_OPTIONS` 里的那 30 条之一时才是 `Some`
    pub(super) running_color: Option<usize>,
    pub(super) effect: Effect,
    pub(super) icon: IconMode,
    /// 数字行有没有带百分秒。
    pub(super) centis: bool,
    /// 计时数字行的补零档位。
    pub(super) pad: Pad,
    /// 时钟挂件显示到分还是到秒。
    pub(super) clock_seconds: bool,
    /// 挂件是否被藏着（只影响那一行的勾选）。
    pub(super) hidden: bool,
    /// 是否处于编辑态（同上，只影响那一行的勾选）。
    pub(super) edit: bool,
    /// 图标里的占用指标画数字还是水位。
    pub(super) numbers: bool,
    /// 动图限速看哪个指标。
    pub(super) throttle: Throttle,
    /// 到点发不发桌面通知。
    pub(super) notify: bool,
    /// 是否已登记开机自启。这一格不进 Config：磁盘上那个文件本身就是状态。
    pub(super) autostart: bool,
    /// 配了可用的 `tray_gif` 没有（动图档，GIF/PNG 都算）；没配的话那一档点了也只能
    /// 退回表盘，索性置灰
    pub(super) gif: bool,
    /// 配了 `alarm_sound` 没有；没配的话「试听音效」无从播起，同样置灰
    pub(super) sound: bool,
    /// 键盘可用性（后端启动时探明：Wayland 座位键盘能力 + libxkbcommon）。
    /// 拿不到键盘时「⌨ 输入时长」置灰并写明原因——点了没动作却不说明，
    /// 比置灰更难查。不进 Config：它是会话能力，不是用户选择。
    pub(super) kb_ok: bool,
    /// 语言子菜单的勾选看**配置值**（auto/zh/en 三档单选）。
    pub(super) language: Language,
    /// `pomo_seq` 当前段号，由主循环回填；不在序列番茄钟上就是 `None`。
    pub(super) pomo_step: Option<usize>,
}

impl State {
    pub(super) fn from_config(cfg: &Config) -> Self {
        Self {
            mode: cfg.mode,
            alpha: cfg.bg_alpha,
            palette: PALETTES.iter().position(|p| {
                p.bg == cfg.color_bg
                    && Gradient::solid(p.running) == cfg.color_running
                    && Gradient::solid(p.paused) == cfg.color_paused
                    && Gradient::solid(p.done) == cfg.color_done
            }),
            effect: cfg.text_effect,
            icon: cfg.tray_icon,
            running_color: COLOR_OPTIONS
                .iter()
                .position(|v| Gradient::parse(v).is_some_and(|g| g == cfg.color_running)),
            centis: cfg.centiseconds,
            pad: cfg.time_pad,
            clock_seconds: cfg.clock_seconds,
            // 挂件总是以"看得见"启动（hidden 不进配置），所以这里恒 false
            hidden: false,
            // 编辑态同理：启动时不在编辑态
            edit: false,
            numbers: cfg.tray_numbers,
            throttle: cfg.tray_throttle,
            notify: cfg.notify,
            autostart: crate::config::autostart_enabled(),
            gif: cfg
                .tray_gif
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty()),
            sound: cfg.alarm_sound.is_some(),
            // 键盘按"可用"乐观起：后端探明拿不到才经 SyncKb 推一次置灰
            kb_ok: true,
            language: cfg.language,
            pomo_step: None,
        }
    }

    /// 热加载/套接字回填：配置派生的格子全部对齐到新值，**运行态的三格保留**
    /// （hidden / edit 不进配置，pomo_step 是计时器实时状态，都不是 Config 的投影）。
    /// `mode` 特殊：配置里的 `mode` 是启动值，勾该跟着在跑的计时器走，
    /// 由主循环把实时值盖进 `cfg.mode` 再传进来（见 `Widget::sync_tray`）。
    pub(super) fn resync(&mut self, cfg: &Config) {
        let hidden = std::mem::take(&mut self.hidden);
        let edit = std::mem::take(&mut self.edit);
        let step = self.pomo_step;
        let kb = self.kb_ok;
        *self = Self::from_config(cfg);
        self.hidden = hidden;
        self.edit = edit;
        self.pomo_step = step;
        // 键盘可用性是会话能力不是配置投影：热加载不许把它冲回乐观值
        self.kb_ok = kb;
    }

    /// 这一项当前能不能点。置灰的两档都在标签里写了原因（点了没动作却不说明，
    /// 比置灰更难查）。
    pub(super) fn enabled(&self, cmd: &Command) -> bool {
        match cmd {
            Command::SetIcon(IconMode::Gif) => self.gif,
            Command::PreviewSound => self.sound,
            Command::InputTime => self.kb_ok,
            _ => true,
        }
    }

    pub(super) fn checked(&self, c: Check) -> bool {
        match c {
            Check::Mode(m) => self.mode == m,
            Check::Alpha(a) => self.alpha == a,
            Check::Palette(i) => self.palette == Some(i),
            Check::RunningColor(i) => self.running_color == Some(i),
            Check::Effect(e) => self.effect == e,
            Check::Icon(m) => self.icon == m,
            Check::Centiseconds => self.centis,
            Check::Centi(want) => self.centis == want,
            Check::TimePad(p) => self.pad == p,
            Check::ClockSeconds => self.clock_seconds,
            Check::Hidden => self.hidden,
            Check::Edit => self.edit,
            Check::Numbers => self.numbers,
            Check::Throttle(t) => self.throttle == t,
            Check::Notify => self.notify,
            Check::Autostart => self.autostart,
            Check::Language(l) => self.language == l,
            Check::PomoStep(i) => self.pomo_step == Some(i),
        }
    }

    /// 发出命令后同步镜像。只有会影响 `checked` 的命令在此登记，其余忽略。
    pub(super) fn note(&mut self, cmd: &Command) {
        match *cmd {
            Command::SetMode(m) => self.mode = m,
            Command::SetAlpha(a) => {
                self.alpha = a;
                // 透明度不改变配色，但自定义过的配色不该再算命中任何预设
            }
            Command::SetPalette(i) => {
                self.palette = Some(i);
                // 整套四色预设的运行色也可能正好是那 30 条之一，两组的勾要一起对
                self.running_color = COLOR_OPTIONS.iter().position(|v| {
                    Gradient::parse(v).is_some_and(|g| g == Gradient::solid(PALETTES[i].running))
                });
            }
            Command::SetRunningColor(i) => {
                self.running_color = Some(i);
                // 只换运行色，那"四色一套"的勾就不该再亮着（除非新值恰好仍命中某套，
                // 而托盘自持的那份状态算不出这件事，宁可留空）
                self.palette = None;
            }
            Command::SetEffect(e) => self.effect = e,
            Command::SetIcon(m) => self.icon = m,
            Command::ToggleNumbers => self.numbers = !self.numbers,
            Command::SetThrottle(t) => self.throttle = t,
            Command::ToggleCentiseconds => self.centis = !self.centis,
            Command::SetCentiseconds(v) => self.centis = v,
            Command::SetTimePad(p) => self.pad = p,
            Command::ToggleClockSeconds => self.clock_seconds = !self.clock_seconds,
            Command::ToggleHidden => self.hidden = !self.hidden,
            Command::SetHidden(v) => self.hidden = v,
            Command::ToggleEdit => self.edit = !self.edit,
            Command::SetEdit(v) => self.edit = v,
            Command::ToggleNotify => self.notify = !self.notify,
            Command::ToggleAutostart => self.autostart = !self.autostart,
            Command::SetLanguage(l) => self.language = l,
            // SetPomoStep 不改这里：段号由主循环按计时器的真值回填（SyncPomo），
            // 点了哪一段不代表当前段——跳段之后当前段才跟着走，那才是勾该待的地方
            _ => {}
        }
    }
}

/// 往节点表追加一个节点并登记为根菜单的一项，返回它的 id。
///
/// 用函数而不是闭包：闭包会一直占住 `n` 的可变借用，而下面还要按下标往子菜单里塞孩子。
pub(super) fn push_top(n: &mut Vec<Node>, root: &mut Vec<i32>, node: Node) -> i32 {
    let id = n.len() as i32;
    n.push(node);
    root.push(id);
    id
}

pub(super) fn build_nodes(
    presets: &[u32],
    gif_configured: bool,
    sound_configured: bool,
    kb_ok: bool,
    pomo_seq: &[u32],
    lang: Language,
) -> Vec<Node> {
    let button = |label: &str, command: Command| Node {
        kind: Kind::Button,
        label: label.into(),
        command: Some(command),
        children: Vec::new(),
    };
    let separator = || Node {
        kind: Kind::Separator,
        label: String::new(),
        command: None,
        children: Vec::new(),
    };
    let submenu = |label: &str| Node {
        kind: Kind::Submenu,
        label: label.into(),
        command: None,
        children: Vec::new(),
    };

    let mut n = vec![Node {
        kind: Kind::Root,
        label: String::new(),
        command: None,
        children: Vec::new(),
    }];
    let mut root = Vec::new();
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "▶ 开始", "▶ Start"), Command::Start),
    );
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "⏸ 暂停", "⏸ Pause"), Command::Pause),
    );
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "⟳ 重置", "⟳ Reset"), Command::Reset),
    );
    push_top(
        &mut n,
        &mut root,
        button(
            tr_in(lang, "👻 隐藏挂件", "👻 Hide widget"),
            Command::ToggleHidden,
        ),
    );
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "🛠 编辑态", "🛠 Edit mode"), Command::ToggleEdit),
    );
    // 输入行：键盘不可用（座位没键盘能力 / 缺 libxkbcommon）时置灰并写明原因，
    // 与「试听音效」未配时同一规矩
    push_top(
        &mut n,
        &mut root,
        button(
            if kb_ok {
                tr_in(lang, "⌨ 输入时长", "⌨ Type a duration")
            } else {
                tr_in(
                    lang,
                    "⌨ 输入时长（无键盘）",
                    "⌨ Type a duration (no keyboard)",
                )
            },
            Command::InputTime,
        ),
    );
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "🔔 弹通知", "🔔 Notify"), Command::ToggleNotify),
    );
    // 试听：没配提示音就置灰并把原因写进标签（与 GIF 那一档同一规矩）
    push_top(
        &mut n,
        &mut root,
        button(
            if sound_configured {
                tr_in(lang, "🔊 试听音效", "🔊 Test sound")
            } else {
                tr_in(
                    lang,
                    "🔊 试听音效（未配 alarm_sound）",
                    "🔊 Test sound (no alarm_sound)",
                )
            },
            Command::PreviewSound,
        ),
    );
    push_top(
        &mut n,
        &mut root,
        button(
            tr_in(lang, "🚀 开机自启", "🚀 Autostart"),
            Command::ToggleAutostart,
        ),
    );
    push_top(&mut n, &mut root, separator());
    let presets_menu = push_top(
        &mut n,
        &mut root,
        submenu(tr_in(lang, "时长预设", "Presets")),
    );
    // 番茄钟分段：只在配了 `pomo_seq` 时出现——经典配方没有"段"可列
    let pomo_menu = (!pomo_seq.is_empty()).then(|| {
        push_top(
            &mut n,
            &mut root,
            submenu(tr_in(lang, "🍅 番茄分段", "🍅 Pomodoro steps")),
        )
    });
    let modes = push_top(&mut n, &mut root, submenu(tr_in(lang, "模式", "Mode")));
    let look = push_top(
        &mut n,
        &mut root,
        submenu(tr_in(lang, "外观", "Appearance")),
    );
    push_top(&mut n, &mut root, separator());
    push_top(
        &mut n,
        &mut root,
        button(
            tr_in(lang, "↺ 恢复默认设置", "↺ Restore defaults"),
            Command::ResetConfig,
        ),
    );
    push_top(
        &mut n,
        &mut root,
        button(
            tr_in(lang, "⌂ 重置窗口位置", "⌂ Reset position"),
            Command::ResetPosition,
        ),
    );
    let language = push_top(&mut n, &mut root, submenu(tr_in(lang, "语言", "Language")));
    push_top(
        &mut n,
        &mut root,
        button(tr_in(lang, "✕ 退出", "✕ Quit"), Command::Quit),
    );

    // 预设超过一页时折进「更多 ▸」：Catime 按 2/3 屏高自动分页（`tray_menu_pagination.c`），
    // 我们拿不到菜单高度，按条数分——前 `PRESET_PAGE` 项平铺，余下的进子菜单。
    let mut preset_slot = presets_menu as usize;
    let mut paginate = presets.len() > PRESET_PAGE;
    for (k, secs) in presets.iter().enumerate() {
        if paginate && k == PRESET_PAGE {
            let more = n.len() as i32;
            n.push(submenu(tr_in(lang, "更多 ▸", "More ▸")));
            n[presets_menu as usize].children.push(more);
            preset_slot = more as usize;
            paginate = false; // 只分一页：50 档上限里 20 + 30 已经念得清
        }
        let id = n.len() as i32;
        let label = preset_label_in(*secs, lang);
        n.push(button(&format!("⏱ {label}"), Command::Preset(*secs)));
        n[preset_slot].children.push(id);
    }
    if let Some(menu) = pomo_menu {
        // 每段一项、当前段打勾（GAP §四："托盘那半边没做"的那半句）。点了跳去那段：
        // 与时长预设同一个手感——立刻把读数换成本段时长，运行与否不变。
        for (i, secs) in pomo_seq.iter().enumerate() {
            let id = n.len() as i32;
            let label = preset_label_in(*secs, lang);
            let name = if lang.resolve() == Language::En {
                format!("Step {} · {}", i + 1, label)
            } else {
                format!("第 {} 段 · {}", i + 1, label)
            };
            n.push(button(&name, Command::SetPomoStep(i)));
            n[menu as usize].children.push(id);
        }
    }
    for (label_zh, label_en, mode) in [
        ("倒计时", "Countdown", Mode::Countdown),
        ("秒表", "Stopwatch", Mode::Stopwatch),
        ("🍅 番茄钟", "🍅 Pomodoro", Mode::Pomodoro),
        ("🕐 时钟", "🕐 Clock", Mode::Clock),
    ] {
        let id = n.len() as i32;
        n.push(button(
            tr_in(lang, label_zh, label_en),
            Command::SetMode(mode),
        ));
        n[modes as usize].children.push(id);
    }
    // 外观：透明度 / 配色 / 文字特效 / 图标内容，各一个二级子菜单
    let alpha = n.len() as i32;
    n.push(submenu(tr_in(lang, "背景透明度", "Background opacity")));
    n[look as usize].children.push(alpha);
    let palette = n.len() as i32;
    n.push(submenu(tr_in(lang, "配色预设", "Palettes")));
    n[look as usize].children.push(palette);
    // 30 条取自 Catime 的数字色（含渐变）：只换运行色，不动背景与另两态
    let colors = n.len() as i32;
    n.push(submenu(tr_in(lang, "文字颜色", "Text color")));
    n[look as usize].children.push(colors);
    let effects = n.len() as i32;
    n.push(submenu(tr_in(lang, "文字特效", "Text effects")));
    n[look as usize].children.push(effects);
    let icon = n.len() as i32;
    n.push(submenu(tr_in(lang, "图标内容", "Tray icon")));
    n[look as usize].children.push(icon);
    // 时间格式：补零三档是单选组，显示秒与百分秒各是一个勾选框
    let fmt = n.len() as i32;
    n.push(submenu(tr_in(lang, "时间格式", "Time format")));
    n[look as usize].children.push(fmt);
    for (zh, en, value) in ALPHA_STEPS {
        let id = n.len() as i32;
        n.push(button(tr_in(lang, zh, en), Command::SetAlpha(value)));
        n[alpha as usize].children.push(id);
    }
    for (i, p) in PALETTES.iter().enumerate() {
        let id = n.len() as i32;
        n.push(button(
            tr_in(lang, p.name, p.name_en),
            Command::SetPalette(i),
        ));
        n[palette as usize].children.push(id);
    }
    // 颜色预设的标签就是值串本身（两种语言一样），具名五条额外带上名字
    for (i, v) in COLOR_OPTIONS.iter().enumerate() {
        let id = n.len() as i32;
        n.push(button(&color_label(v), Command::SetRunningColor(i)));
        n[colors as usize].children.push(id);
    }
    for e in EFFECTS {
        let id = n.len() as i32;
        n.push(button(e.label_in(lang), Command::SetEffect(e)));
        n[effects as usize].children.push(id);
    }
    for (zh, en, m) in [
        ("🕐 时钟表盘", "🕐 Clock dial", IconMode::Clock),
        ("CPU 占用", "CPU usage", IconMode::Cpu),
        ("内存占用", "Memory usage", IconMode::Memory),
        ("电池电量", "Battery", IconMode::Battery),
        ("网络速率", "Network rate", IconMode::Network),
        // 没配路径就直说，否则点了只会静默退回表盘。容器写"动图"不写格式名：
        // GIF 与 PNG/APNG 共用同一个 `tray_gif` 键，由文件头分发解码器（`anim::decode`）。
        (
            if gif_configured {
                "🌀 动图（GIF/PNG）"
            } else {
                "🌀 动图（未配置 tray_gif）"
            },
            if gif_configured {
                "🌀 Animation (GIF/PNG)"
            } else {
                "🌀 Animation (no tray_gif)"
            },
            IconMode::Gif,
        ),
    ] {
        let id = n.len() as i32;
        n.push(button(tr_in(lang, zh, en), Command::SetIcon(m)));
        n[icon as usize].children.push(id);
    }
    // 数字还是水位：只对那四档指标有意义，所以挂在同一个子菜单末尾
    let nums = n.len() as i32;
    n.push(button(
        tr_in(lang, "用数字代替水位", "Numbers instead of gauge"),
        Command::ToggleNumbers,
    ));
    n[icon as usize].children.push(nums);
    // 动图限速：单选五档（对齐 Catime 的 ANIMATION_SPEED_METRIC），挂在图标内容下面
    let throttle = n.len() as i32;
    n.push(submenu(tr_in(lang, "动图限速", "Animation throttle")));
    n[icon as usize].children.push(throttle);
    for (zh, en, t) in [
        ("不限速", "Off", Throttle::Off),
        ("看 CPU", "By CPU", Throttle::Cpu),
        ("看内存", "By memory", Throttle::Memory),
        ("看倒计时进度", "By countdown", Throttle::Timer),
        ("固定倍率", "Fixed rate", Throttle::Fixed),
    ] {
        let id = n.len() as i32;
        n.push(button(tr_in(lang, zh, en), Command::SetThrottle(t)));
        n[throttle as usize].children.push(id);
    }
    for p in PADS {
        let id = n.len() as i32;
        n.push(button(p.label_in(lang), Command::SetTimePad(p)));
        n[fmt as usize].children.push(id);
    }
    for (zh, en, cmd) in [
        (
            "时钟显示秒",
            "Clock shows seconds",
            Command::ToggleClockSeconds,
        ),
        ("百分之一秒", "Centiseconds", Command::ToggleCentiseconds),
    ] {
        let id = n.len() as i32;
        n.push(button(tr_in(lang, zh, en), cmd));
        n[fmt as usize].children.push(id);
    }
    // 语言：三档单选，标签恒用母语名（"简体中文" 就该写成 "简体中文"，
    // 这正是 Catime 每行用自己语言写的用意——选错了也认得回去的那一行）
    for l in [Language::Auto, Language::Zh, Language::En] {
        let id = n.len() as i32;
        n.push(button(l.label(), Command::SetLanguage(l)));
        n[language as usize].children.push(id);
    }
    // 根节点的子项顺序即菜单顺序
    n[0].children = root;
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认按中文构建（与 `Config::default().language` 的 resolve 结果同侧）。
    /// 键盘按可用建：多数测试不关心那一项的置灰。
    fn nodes(presets: &[u32], gif: bool) -> Vec<Node> {
        build_nodes(presets, gif, false, true, &[], Language::Zh)
    }

    #[test]
    fn menu_nodes_are_well_formed() {
        let n = nodes(&Config::default().presets, true);
        assert_eq!(n[0].kind, Kind::Root);
        for (id, node) in n.iter().enumerate().skip(1) {
            match node.kind {
                Kind::Button => {
                    assert!(node.command.is_some(), "按钮 {id} 没绑命令");
                    assert!(!node.label.is_empty(), "按钮 {id} 没有标签");
                    assert!(node.children.is_empty(), "按钮 {id} 不该有孩子");
                }
                Kind::Separator => {
                    assert!(node.command.is_none() && node.children.is_empty());
                }
                Kind::Submenu => {
                    assert!(!node.children.is_empty(), "子菜单 {id} 是空的");
                    assert!(node.command.is_none(), "子菜单 {id} 不该绑命令");
                }
                Kind::Root => panic!("只有 0 号节点是 Root"),
            }
            for c in &node.children {
                assert!((*c as usize) < n.len(), "{id} 的孩子 {c} 越界");
                assert_ne!(*c, id as i32, "{id} 把自己列为孩子");
            }
        }
        // 手工算下标挂孩子，最容易错的就是同一个 id 被登记两次
        let mut seen: Vec<i32> = n.iter().flat_map(|x| x.children.clone()).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "有节点被挂了两个父亲");
    }

    #[test]
    fn appearance_submenu_reaches_every_preset() {
        let n = nodes(&Config::default().presets, true);
        let look = n
            .iter()
            .find(|x| x.label == "外观")
            .expect("没有「外观」子菜单");
        assert_eq!(
            look.children.len(),
            6,
            "外观下应是 透明度 / 配色 / 文字颜色 / 文字特效 / 图标内容 / 时间格式 六个子菜单"
        );
        let acts = |parent: i32| -> Vec<Option<Command>> {
            n[parent as usize]
                .children
                .iter()
                .map(|i| n[*i as usize].command.clone())
                .collect()
        };
        let (alpha_id, palette_id, colors_id, effect_id, icon_id, fmt_id) = (
            look.children[0],
            look.children[1],
            look.children[2],
            look.children[3],
            look.children[4],
            look.children[5],
        );
        assert_eq!(n[alpha_id as usize].label, "背景透明度");
        assert_eq!(n[palette_id as usize].label, "配色预设");
        assert_eq!(n[colors_id as usize].label, "文字颜色");
        assert_eq!(n[effect_id as usize].label, "文字特效");
        assert_eq!(n[icon_id as usize].label, "图标内容");
        assert_eq!(n[fmt_id as usize].label, "时间格式");

        // 那 30 条取自 Catime 的数字色要一条条排进菜单，标签就是值串本身
        let colors = acts(colors_id);
        assert_eq!(colors.len(), COLOR_OPTIONS.len());
        for (k, id) in n[colors_id as usize].children.iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetRunningColor(k)));
            assert_eq!(
                n[*id as usize].label,
                color_label(COLOR_OPTIONS[k]),
                "第 {k} 条标签不对"
            );
        }
        // 每条都得能被 Gradient 解析，否则点了就是静默无效
        for v in COLOR_OPTIONS {
            assert!(Gradient::parse(v).is_some(), "{v} 解析不出渐变");
        }

        let alpha = acts(alpha_id);
        assert_eq!(alpha.len(), ALPHA_STEPS.len());
        for (k, act) in alpha.iter().enumerate() {
            assert_eq!(
                *act,
                Some(Command::SetAlpha(ALPHA_STEPS[k].2)),
                "第 {k} 档透明度接错"
            );
        }
        let palette = acts(palette_id);
        assert_eq!(palette.len(), PALETTES.len());
        for (k, id) in n[palette_id as usize].children.iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetPalette(k)));
            assert_eq!(
                n[*id as usize].label, PALETTES[k].name,
                "菜单标签与预设对不上"
            );
        }
        let effects = acts(effect_id);
        // 特效加一档忘了登记就会漏在这里
        assert_eq!(effects.len(), EFFECTS.len());
        for (k, id) in n[effect_id as usize].children.iter().enumerate() {
            assert_eq!(
                n[*id as usize].command,
                Some(Command::SetEffect(EFFECTS[k]))
            );
            assert_eq!(
                n[*id as usize].label,
                EFFECTS[k].label_in(Language::Zh),
                "菜单标签与特效对不上"
            );
        }
        let icons = acts(icon_id);
        // 前六项与 IconMode 的全部取值一一对应：加一档忘了登记就漏在这里
        let expect = [
            IconMode::Clock,
            IconMode::Cpu,
            IconMode::Memory,
            IconMode::Battery,
            IconMode::Network,
            IconMode::Gif,
        ];
        assert_eq!(
            icons.len(),
            expect.len() + 2,
            "末尾还有「用数字代替水位」与「动图限速」"
        );
        for (k, id) in n[icon_id as usize].children[..expect.len()]
            .iter()
            .enumerate()
        {
            assert_eq!(n[*id as usize].command, Some(Command::SetIcon(expect[k])));
        }
        assert_eq!(
            n[n[icon_id as usize].children[expect.len()] as usize].command,
            Some(Command::ToggleNumbers)
        );
        // 限速那一档是挂在图标内容下面的单选五档
        let throttle_id = n[icon_id as usize].children[expect.len() + 1];
        assert_eq!(n[throttle_id as usize].label, "动图限速");
        assert_eq!(
            acts(throttle_id),
            vec![
                Some(Command::SetThrottle(Throttle::Off)),
                Some(Command::SetThrottle(Throttle::Cpu)),
                Some(Command::SetThrottle(Throttle::Memory)),
                Some(Command::SetThrottle(Throttle::Timer)),
                Some(Command::SetThrottle(Throttle::Fixed)),
            ]
        );
        // 时间格式：三档补零按 PADS 顺序，后面跟两个勾选框
        let fmt_cmds = acts(fmt_id);
        let want = [
            Some(Command::SetTimePad(Pad::None)),
            Some(Command::SetTimePad(Pad::Zero)),
            Some(Command::SetTimePad(Pad::Full)),
            Some(Command::ToggleClockSeconds),
            Some(Command::ToggleCentiseconds),
        ];
        assert_eq!(fmt_cmds, want, "时间格式子菜单与预期对不上");
        for (k, id) in n[fmt_id as usize].children[..PADS.len()].iter().enumerate() {
            assert_eq!(
                n[*id as usize].label,
                PADS[k].label_in(Language::Zh),
                "补零档的标签对不上"
            );
        }
    }

    /// 预设菜单要照配置生成：条数、秒数、标签都得对上。
    #[test]
    fn preset_submenu_follows_the_config_list() {
        let n = nodes(&[90, 1500, 5400], true);
        let submenu = n
            .iter()
            .find(|x| x.label == "时长预设")
            .expect("没有「时长预设」子菜单");
        let items: Vec<(String, Option<Command>)> = submenu
            .children
            .iter()
            .map(|i| (n[*i as usize].label.clone(), n[*i as usize].command.clone()))
            .collect();
        assert_eq!(
            items,
            vec![
                ("⏱ 1 分 30 秒".to_string(), Some(Command::Preset(90))),
                ("⏱ 25 分".to_string(), Some(Command::Preset(1500))),
                ("⏱ 1 小时 30 分".to_string(), Some(Command::Preset(5400))),
            ]
        );
    }

    /// 超过一页的预设折进「更多 ▸」：平铺 20 项 + 一个子菜单装下余下的，
    /// 一条不少、一条不多，且子菜单本身不再是按钮。
    #[test]
    fn long_preset_list_paginates() {
        let many: Vec<u32> = (1..=30u32).map(|m| m * 60).collect();
        let n = nodes(&many, true);
        let submenu_idx = n
            .iter()
            .position(|x| x.label == "时长预设")
            .expect("没有「时长预设」子菜单");
        assert_eq!(
            n[submenu_idx].children.len(),
            PRESET_PAGE + 1,
            "平铺 20 项 + 一个「更多 ▸」"
        );
        let more_idx = n[submenu_idx].children[PRESET_PAGE] as usize;
        assert_eq!(n[more_idx].label, "更多 ▸");
        assert_eq!(n[more_idx].kind, Kind::Submenu);
        assert_eq!(n[more_idx].children.len(), many.len() - PRESET_PAGE);
        // 每条预设都还在，且都绑着自己的秒数
        let mut count = 0;
        for node in &n {
            for c in &node.children {
                if matches!(n[*c as usize].command, Some(Command::Preset(_))) {
                    count += 1;
                }
            }
        }
        assert_eq!(count, many.len(), "分页把某条预设弄丢了");
        // 没超限就一个子菜单都不该造出来
        let few = nodes(&many[..PRESET_PAGE], true);
        let sub = few.iter().find(|x| x.label == "时长预设").unwrap();
        assert_eq!(sub.children.len(), PRESET_PAGE);
        assert!(!few.iter().any(|x| x.label == "更多 ▸"));
    }

    /// 英文构建：标签换血，命令一个不换。
    #[test]
    fn english_labels_same_commands() {
        let n = build_nodes(&Config::default().presets, true, false, true, &[], Language::En);
        assert!(
            n.iter()
                .any(|x| x.command == Some(Command::Start) && x.label == "▶ Start")
        );
        assert!(n.iter().any(|x| x.label == "Appearance"));
        assert!(
            n.iter().any(|x| x.label == "⏱ 25m"),
            "英文预设标签走紧凑单位"
        );
        // 中文档里找得到的命令，英文档里也必须找得到——词条只改皮不改骨
        let zh = nodes(&Config::default().presets, true);
        let cmds = |v: &Vec<Node>| {
            let mut c: Vec<_> = v.iter().filter_map(|x| x.command.clone()).collect();
            c.sort_by_key(|c| format!("{c:?}"));
            c
        };
        assert_eq!(cmds(&n), cmds(&zh));
    }

    /// 番茄分段：配了 `pomo_seq` 才有子菜单，每段一项且按 `SetPomoStep` 绑段号；
    /// 打勾跟着主循环回填的段号走，不跟点击走。
    #[test]
    fn pomodoro_steps_submenu_follows_the_sequence() {
        let n = build_nodes(&[60], true, false, true, &[1500, 300, 900], Language::Zh);
        let menu = n
            .iter()
            .find(|x| x.label == "🍅 番茄分段")
            .expect("配了序列就该有子菜单");
        assert_eq!(menu.children.len(), 3);
        for (i, id) in menu.children.iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetPomoStep(i)));
            assert!(n[*id as usize].label.contains(&format!("第 {} 段", i + 1)));
        }
        // 没配序列就整个子菜单不存在（空子菜单会被 well_formed 测试拒绝）
        let none = nodes(&[60], true);
        assert!(!none.iter().any(|x| x.label == "🍅 番茄分段"));

        // 勾选态：只有回填过的那一段亮着
        let cfg = Config::default();
        let mut s = State::from_config(&cfg);
        assert_eq!(s.pomo_step, None);
        assert!(!s.checked(Check::PomoStep(0)));
        s.pomo_step = Some(1);
        assert!(s.checked(Check::PomoStep(1)) && !s.checked(Check::PomoStep(0)));
        // 点另一项不翻勾——段号只认计时器的真值
        s.note(&Command::SetPomoStep(2));
        assert_eq!(s.pomo_step, Some(1));
    }

    /// 语言三档是单选组；回填配置不牵连运行态的三格。
    #[test]
    fn language_radio_and_resync_preserves_runtime_state() {
        let n = nodes(&[60], true);
        let menu = n
            .iter()
            .find(|x| x.label == "语言")
            .expect("没有「语言」子菜单");
        assert_eq!(menu.children.len(), 3);
        assert_eq!(
            n[menu.children[0] as usize].command,
            Some(Command::SetLanguage(Language::Auto))
        );

        let cfg = Config {
            language: Language::Zh,
            ..Config::default()
        };
        let mut s = State::from_config(&cfg);
        assert!(s.checked(Check::Language(Language::Zh)));
        s.note(&Command::SetLanguage(Language::En));
        assert!(
            s.checked(Check::Language(Language::En)) && !s.checked(Check::Language(Language::Zh))
        );

        // resync：配置派生的格子跟文件走，hidden/edit/pomo_step 留在原地
        s.note(&Command::SetAlpha(96));
        s.hidden = true;
        s.edit = true;
        s.pomo_step = Some(2);
        let mut next = cfg.clone();
        next.bg_alpha = 0;
        next.centiseconds = true;
        s.resync(&next);
        assert!(s.checked(Check::Alpha(0)), "透明度该跟回填走");
        assert!(s.checked(Check::Centiseconds), "百分秒同理");
        assert_eq!(
            s.language,
            Language::Zh,
            "note 过的语言会被回填成配置值（它本就写回配置）"
        );
        assert!(s.hidden && s.edit, "运行态的格子不许被回填抹掉");
        assert_eq!(s.pomo_step, Some(2));
    }

    /// 只有单选组该带勾选；动作按钮（开始/暂停/退出/时长预设/试听）不该画成圆点。
    /// 没配 `tray_gif` 时 GIF 那一档该置灰并在标签上说明原因，其余档不受影响；
    /// 没配 `alarm_sound` 时「试听音效」同此规矩。
    #[test]
    fn gif_item_is_disabled_without_a_path() {
        let cfg = Config::default();
        let s = State::from_config(&cfg);
        assert!(
            !s.enabled(&Command::SetIcon(IconMode::Gif)),
            "没配路径该不可点"
        );
        assert!(s.enabled(&Command::SetIcon(IconMode::Clock)));
        assert!(s.enabled(&Command::SetAlpha(96)), "非图标项不该被牵连");
        assert!(!s.enabled(&Command::PreviewSound), "没配提示音不该能试听");
        assert!(
            State::from_config(&Config {
                alarm_sound: Some("beep".into()),
                ..Config::default()
            })
            .enabled(&Command::PreviewSound),
            "配了 beep 就该能试听"
        );
        // 置灰的试听项标签该把原因写出来
        let labelled = nodes(&[60], false);
        let preview = labelled
            .iter()
            .find(|x| x.command == Some(Command::PreviewSound))
            .expect("菜单里该有试听项");
        assert!(
            preview.label.contains("alarm_sound"),
            "标签该说明为什么不可用: {}",
            preview.label
        );

        let with = Config {
            tray_gif: Some("~/p/s.gif".into()),
            ..Config::default()
        };
        assert!(State::from_config(&with).enabled(&Command::SetIcon(IconMode::Gif)));
        // 只有空白也算没配
        let blank = Config {
            tray_gif: Some("   ".into()),
            ..Config::default()
        };
        assert!(!State::from_config(&blank).enabled(&Command::SetIcon(IconMode::Gif)));

        let labelled = nodes(&cfg.presets, false);
        let gif = labelled
            .iter()
            .find(|x| x.label.starts_with("🌀 动图"))
            .expect("菜单里该有动图项");
        assert!(
            gif.label.contains("tray_gif"),
            "标签该说明为什么不可用: {}",
            gif.label
        );
        let ok = nodes(&cfg.presets, true);
        assert_eq!(
            ok.iter().find(|x| x.label.starts_with("🌀")).unwrap().label,
            "🌀 动图（GIF/PNG）"
        );
    }

    #[test]
    fn only_radio_items_are_checkable() {
        let n = build_nodes(
            &Config::default().presets,
            true,
            false,
            true,
            &[1500, 300],
            Language::Zh,
        );
        for node in &n {
            let Some(cmd) = node.command.as_ref() else {
                continue;
            };
            let checkable = cmd.check().is_some();
            // 单选组加上各档勾选框（SetHidden / SetEdit / SetPomoStep 的语义见右列）
            let in_group = matches!(
                cmd,
                Command::SetMode(_)
                    | Command::SetAlpha(_)
                    | Command::SetPalette(_)
                    | Command::SetRunningColor(_)
                    | Command::SetEffect(_)
                    | Command::SetIcon(_)
                    | Command::SetThrottle(_)
                    | Command::SetTimePad(_)
                    | Command::SetLanguage(_)
                    | Command::SetPomoStep(_)
                    | Command::SetCentiseconds(_)
                    | Command::ToggleCentiseconds
                    | Command::ToggleClockSeconds
                    | Command::ToggleHidden
                    | Command::ToggleEdit
                    | Command::ToggleNumbers
                    | Command::ToggleNotify
                    | Command::ToggleAutostart
            );
            assert_eq!(checkable, in_group, "{} 的勾选属性推错了", node.label);
        }
    }

    #[test]
    fn checked_state_mirrors_menu_clicks() {
        let cfg = Config {
            mode: Mode::Countdown,
            bg_alpha: 96,
            ..Config::default()
        };
        let mut s = State::from_config(&cfg);
        assert!(s.checked(Check::Mode(Mode::Countdown)));
        assert!(!s.checked(Check::Mode(Mode::Pomodoro)));
        assert!(s.checked(Check::Alpha(96)));
        assert!(s.checked(Check::Icon(IconMode::Clock)));

        s.note(&Command::SetMode(Mode::Pomodoro));
        assert!(s.checked(Check::Mode(Mode::Pomodoro)));
        assert!(
            !s.checked(Check::Mode(Mode::Countdown)),
            "旧的那项必须取消勾选"
        );

        s.note(&Command::SetAlpha(0));
        assert!(s.checked(Check::Alpha(0)) && !s.checked(Check::Alpha(96)));

        // 时长预设之类不影响任何勾选，别把它们误登记进去
        s.note(&Command::Preset(600));
        s.note(&Command::Start);
        assert!(s.checked(Check::Mode(Mode::Pomodoro)));

        // 百分秒是勾选框：点一下翻面，且不牵连任何单选组
        assert!(
            !s.checked(Check::Centiseconds),
            "默认该关着，开着的代价是心跳"
        );
        s.note(&Command::ToggleCentiseconds);
        assert!(s.checked(Check::Centiseconds));
        assert!(
            s.checked(Check::Mode(Mode::Pomodoro)),
            "翻勾选框不该动了模式勾选"
        );
        s.note(&Command::ToggleCentiseconds);
        assert!(!s.checked(Check::Centiseconds));
        // 设定值版与勾选框共用同一格状态：`--centis` 进来也能把勾摆正
        s.note(&Command::SetCentiseconds(true));
        assert!(s.checked(Check::Centiseconds) && s.checked(Check::Centi(true)));
        assert!(!s.checked(Check::Centi(false)));
        s.note(&Command::SetCentiseconds(true));
        assert!(s.checked(Check::Centiseconds), "重复设定不该翻回去");
        let on = Config {
            centiseconds: true,
            ..Config::default()
        };
        assert!(State::from_config(&on).checked(Check::Centiseconds));
        s.note(&Command::SetCentiseconds(false)); // 回到默认档再往下测

        // 补零是单选组：换档要把旧那档取消
        assert!(s.checked(Check::TimePad(Pad::None)));
        s.note(&Command::SetTimePad(Pad::Full));
        assert!(s.checked(Check::TimePad(Pad::Full)));
        assert!(!s.checked(Check::TimePad(Pad::None)), "旧档必须取消勾选");
        assert!(!s.checked(Check::Centiseconds), "换档不该牵连百分秒");
        // 显示秒默认开着，点一下关掉
        assert!(s.checked(Check::ClockSeconds));
        s.note(&Command::ToggleClockSeconds);
        assert!(!s.checked(Check::ClockSeconds));
    }

    /// 限速三档是单选组：换档要取消旧那档的勾。
    #[test]
    fn throttle_is_a_radio_group_starting_at_off() {
        let on = Config {
            tray_throttle: Throttle::Cpu,
            ..Config::default()
        };
        let mut s = State::from_config(&on);
        assert!(s.checked(Check::Throttle(Throttle::Cpu)));
        assert!(!s.checked(Check::Throttle(Throttle::Off)));
        s.note(&Command::SetThrottle(Throttle::Memory));
        assert!(s.checked(Check::Throttle(Throttle::Memory)));
        assert!(
            !s.checked(Check::Throttle(Throttle::Cpu)),
            "旧档必须取消勾选"
        );
        assert!(
            s.checked(Check::Icon(IconMode::Clock)),
            "换限速不该动了图标档"
        );
    }

    /// 编辑态那一格：启动时恒关着（它不进配置），托盘点一下翻面，
    /// 而套接字那条设定值命令可以重复发。
    #[test]
    fn edit_check_starts_off_and_mirrors_both_command_shapes() {
        let mut s = State::from_config(&Config::default());
        assert!(!s.checked(Check::Edit), "新实例总该从普通态开始");
        s.note(&Command::ToggleEdit);
        assert!(s.checked(Check::Edit));
        s.note(&Command::SetEdit(true));
        assert!(s.checked(Check::Edit), "重复设定不该翻回去");
        s.note(&Command::SetEdit(false));
        assert!(!s.checked(Check::Edit));
        // 隐藏与编辑是两格，互不牵连
        s.note(&Command::ToggleHidden);
        assert!(s.checked(Check::Hidden) && !s.checked(Check::Edit));
    }

    /// 配色勾选只有在四色与某套预设完全一致时才算命中；用户手改过就一项都不勾。
    #[test]
    fn palette_matches_only_when_all_four_colors_agree() {
        let p = &PALETTES[2];
        let cfg = Config {
            color_bg: p.bg,
            color_running: Gradient::solid(p.running),
            color_paused: Gradient::solid(p.paused),
            color_done: Gradient::solid(p.done),
            ..Config::default()
        };
        assert_eq!(State::from_config(&cfg).palette, Some(2));

        let off = Config {
            color_done: Gradient::solid(0x123456),
            ..cfg
        };
        assert_eq!(State::from_config(&off).palette, None);
        let st = State::from_config(&off);
        for i in 0..PALETTES.len() {
            assert!(
                !st.checked(Check::Palette(i)),
                "自定义配色不该勾中第 {i} 套"
            );
        }
        // 点一次预设就重新有得勾
        let mut st = st;
        st.note(&Command::SetPalette(4));
        assert!(st.checked(Check::Palette(4)) && !st.checked(Check::Palette(2)));
    }
}
