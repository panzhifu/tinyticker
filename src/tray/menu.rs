//! 菜单：节点表 + 托盘自持的那份显示状态（`State`），以及 `com.canonical.dbusmenu` 的读写。

use crate::config::{ALPHA_STEPS, COLOR_OPTIONS, Config, PALETTES, color_label, preset_label};
use crate::effect::{EFFECTS, Effect, Gradient};
use crate::render::{PADS, Pad};
use crate::timer::Mode;
use super::*;

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
            Command::ToggleCentiseconds => Some(Check::Centiseconds),
            Command::SetTimePad(p) => Some(Check::TimePad(p)),
            Command::ToggleClockSeconds => Some(Check::ClockSeconds),
            Command::ToggleHidden => Some(Check::Hidden),
            Command::ToggleEdit => Some(Check::Edit),
            Command::ToggleNotify => Some(Check::Notify),
            Command::ToggleAutostart => Some(Check::Autostart),
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
    /// 数字还是水位（`图标内容` 子菜单末尾那一项）。
    Numbers,
    /// 下面三个是勾选框（各自独立），上面五个是单选组。勾选框只有一项，不必带值。
    Centiseconds,
    TimePad(Pad),
    ClockSeconds,
    Hidden,
    /// 编辑态那一格（同样只影响一行勾选）。
    Edit,
    Notify,
    Autostart,
}

/// 托盘自己维护的一份显示状态。
///
/// 菜单要显示「当前选中的是哪一项」，而这几项只有托盘会改（初值来自配置），
/// 所以在发出命令的同时就地镜像一份，省掉主窗口 → 托盘的反向通道。
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
    /// 时钟挂件显示到秒还是到分。
    pub(super) clock_seconds: bool,
    /// 挂件是否被藏着（只影响那一行的勾选）。
    pub(super) hidden: bool,
    /// 是否处于编辑态（同上，只影响那一行的勾选）。
    pub(super) edit: bool,
    /// 图标里的占用指标画数字还是水位。
    pub(super) numbers: bool,
    /// 到点发不发桌面通知。
    pub(super) notify: bool,
    /// 是否已登记开机自启。这一格不进 Config：磁盘上那个文件本身就是状态。
    pub(super) autostart: bool,
    /// 配了可用的 `tray_gif` 没有；没配的话 GIF 那一档点了也只能退回表盘，索性置灰
    pub(super) gif: bool,
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
            running_color: COLOR_OPTIONS.iter().position(|v| {
                Gradient::parse(v).is_some_and(|g| g == cfg.color_running)
            }),
            centis: cfg.centiseconds,
            pad: cfg.time_pad,
            clock_seconds: cfg.clock_seconds,
            // 挂件总是以"看得见"启动（hidden 不进配置），所以这里恒 false
            hidden: false,
            // 编辑态同理：启动时不在编辑态
            edit: false,
            numbers: cfg.tray_numbers,
            notify: cfg.notify,
            autostart: crate::config::autostart_enabled(),
            gif: cfg.tray_gif.as_deref().is_some_and(|p| !p.trim().is_empty()),
        }
    }

    /// 这一项当前能不能点。
    pub(super) fn enabled(&self, cmd: &Command) -> bool {
        !matches!(cmd, Command::SetIcon(IconMode::Gif)) || self.gif
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
            Check::TimePad(p) => self.pad == p,
            Check::ClockSeconds => self.clock_seconds,
            Check::Hidden => self.hidden,
            Check::Edit => self.edit,
            Check::Numbers => self.numbers,
            Check::Notify => self.notify,
            Check::Autostart => self.autostart,
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
            Command::ToggleCentiseconds => self.centis = !self.centis,
            Command::SetTimePad(p) => self.pad = p,
            Command::ToggleClockSeconds => self.clock_seconds = !self.clock_seconds,
            Command::ToggleHidden => self.hidden = !self.hidden,
            Command::SetHidden(v) => self.hidden = v,
            Command::ToggleEdit => self.edit = !self.edit,
            Command::SetEdit(v) => self.edit = v,
            Command::ToggleNotify => self.notify = !self.notify,
            Command::ToggleAutostart => self.autostart = !self.autostart,
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

pub(super) fn build_nodes(presets: &[u32], gif_configured: bool) -> Vec<Node> {
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
    push_top(&mut n, &mut root, button("▶ 开始", Command::Start));
    push_top(&mut n, &mut root, button("⏸ 暂停", Command::Pause));
    push_top(&mut n, &mut root, button("⟳ 重置", Command::Reset));
    push_top(&mut n, &mut root, button("👻 隐藏挂件", Command::ToggleHidden));
    push_top(&mut n, &mut root, button("🛠 编辑态", Command::ToggleEdit));
    push_top(&mut n, &mut root, button("🔔 弹通知", Command::ToggleNotify));
    push_top(&mut n, &mut root, button("🚀 开机自启", Command::ToggleAutostart));
    push_top(&mut n, &mut root, separator());
    let presets_menu = push_top(&mut n, &mut root, submenu("时长预设"));
    let modes = push_top(&mut n, &mut root, submenu("模式"));
    let look = push_top(&mut n, &mut root, submenu("外观"));
    push_top(&mut n, &mut root, separator());
    push_top(&mut n, &mut root, button("↺ 恢复默认设置", Command::ResetConfig));
    push_top(&mut n, &mut root, button("⌂ 重置窗口位置", Command::ResetPosition));
    push_top(&mut n, &mut root, button("✕ 退出", Command::Quit));

    for secs in presets {
        let id = n.len() as i32;
        n.push(button(&format!("⏱ {}", preset_label(*secs)), Command::Preset(*secs)));
        n[presets_menu as usize].children.push(id);
    }
    for (label, mode) in [
        ("倒计时", Mode::Countdown),
        ("秒表", Mode::Stopwatch),
        ("🍅 番茄钟", Mode::Pomodoro),
        ("🕐 时钟", Mode::Clock),
    ] {
        let id = n.len() as i32;
        n.push(button(label, Command::SetMode(mode)));
        n[modes as usize].children.push(id);
    }
    // 外观：透明度 / 配色 / 文字特效 / 图标内容，各一个二级子菜单
    let alpha = n.len() as i32;
    n.push(submenu("背景透明度"));
    n[look as usize].children.push(alpha);
    let palette = n.len() as i32;
    n.push(submenu("配色预设"));
    n[look as usize].children.push(palette);
    // 30 条取自 Catime 的数字色（含渐变）：只换运行色，不动背景与另两态
    let colors = n.len() as i32;
    n.push(submenu("文字颜色"));
    n[look as usize].children.push(colors);
    let effects = n.len() as i32;
    n.push(submenu("文字特效"));
    n[look as usize].children.push(effects);
    let icon = n.len() as i32;
    n.push(submenu("图标内容"));
    n[look as usize].children.push(icon);
    // 时间格式：补零三档是单选组，显示秒与百分秒各是一个勾选框
    let fmt = n.len() as i32;
    n.push(submenu("时间格式"));
    n[look as usize].children.push(fmt);
    for (label, value) in ALPHA_STEPS {
        let id = n.len() as i32;
        n.push(button(label, Command::SetAlpha(value)));
        n[alpha as usize].children.push(id);
    }
    for (i, p) in PALETTES.iter().enumerate() {
        let id = n.len() as i32;
        n.push(button(p.name, Command::SetPalette(i)));
        n[palette as usize].children.push(id);
    }
    for (i, v) in COLOR_OPTIONS.iter().enumerate() {
        let id = n.len() as i32;
        n.push(button(&color_label(v), Command::SetRunningColor(i)));
        n[colors as usize].children.push(id);
    }
    for e in EFFECTS {
        let id = n.len() as i32;
        n.push(button(e.label(), Command::SetEffect(e)));
        n[effects as usize].children.push(id);
    }
    for (label, m) in [
        ("🕐 时钟表盘", IconMode::Clock),
        ("CPU 占用", IconMode::Cpu),
        ("内存占用", IconMode::Memory),
        ("电池电量", IconMode::Battery),
        ("网络速率", IconMode::Network),
        // 没配路径就直说，否则点了只会静默退回表盘
        (if gif_configured { "GIF 动图" } else { "GIF 动图（未配置 tray_gif）" }, IconMode::Gif),
    ] {
        let id = n.len() as i32;
        n.push(button(label, Command::SetIcon(m)));
        n[icon as usize].children.push(id);
    }
    // 数字还是水位：只对那四档指标有意义，所以挂在同一个子菜单末尾
    let nums = n.len() as i32;
    n.push(button("用数字代替水位", Command::ToggleNumbers));
    n[icon as usize].children.push(nums);
    for p in PADS {
        let id = n.len() as i32;
        n.push(button(p.label(), Command::SetTimePad(p)));
        n[fmt as usize].children.push(id);
    }
    for (label, cmd) in
        [("时钟显示秒", Command::ToggleClockSeconds), ("百分之一秒", Command::ToggleCentiseconds)]
    {
        let id = n.len() as i32;
        n.push(button(label, cmd));
        n[fmt as usize].children.push(id);
    }
    // 根节点的子项顺序即菜单顺序
    n[0].children = root;
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_nodes_are_well_formed() {
        let n = build_nodes(&Config::default().presets, true);
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
        let n = build_nodes(&Config::default().presets, true);
        let look = n.iter().find(|x| x.label == "外观").expect("没有「外观」子菜单");
        assert_eq!(
            look.children.len(),
            6,
            "外观下应是 透明度 / 配色 / 文字颜色 / 文字特效 / 图标内容 / 时间格式 六个子菜单"
        );
        let acts = |parent: i32| -> Vec<Option<Command>> {
            n[parent as usize].children.iter().map(|i| n[*i as usize].command.clone()).collect()
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
            assert_eq!(n[*id as usize].label, color_label(COLOR_OPTIONS[k]), "第 {k} 条标签不对");
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
                Some(Command::SetAlpha(ALPHA_STEPS[k].1)),
                "第 {k} 档透明度接错"
            );
        }
        let palette = acts(palette_id);
        assert_eq!(palette.len(), PALETTES.len());
        for (k, id) in n[palette_id as usize].children.iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetPalette(k)));
            assert_eq!(n[*id as usize].label, PALETTES[k].name, "菜单标签与预设对不上");
        }
        let effects = acts(effect_id);
        // 特效加一档忘了登记就会漏在这里
        assert_eq!(effects.len(), EFFECTS.len());
        for (k, id) in n[effect_id as usize].children.iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetEffect(EFFECTS[k])));
            assert_eq!(n[*id as usize].label, EFFECTS[k].label(), "菜单标签与特效对不上");
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
        assert_eq!(icons.len(), expect.len() + 1, "末尾还有一项「用数字代替水位」");
        for (k, id) in n[icon_id as usize].children[..expect.len()].iter().enumerate() {
            assert_eq!(n[*id as usize].command, Some(Command::SetIcon(expect[k])));
        }
        assert_eq!(
            n[*n[icon_id as usize].children.last().unwrap() as usize].command,
            Some(Command::ToggleNumbers)
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
            assert_eq!(n[*id as usize].label, PADS[k].label(), "补零档的标签对不上");
        }
    }

    /// 预设菜单要照配置生成：条数、秒数、标签都得对上。
    #[test]
    fn preset_submenu_follows_the_config_list() {
        let n = build_nodes(&[90, 1500, 5400], true);
        let submenu = n.iter().find(|x| x.label == "时长预设").expect("没有「时长预设」子菜单");
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

    /// 只有单选组该带勾选；动作按钮（开始/暂停/退出/时长预设）不该画成圆点。
    /// 没配 `tray_gif` 时 GIF 那一档该置灰并在标签上说明原因，其余档不受影响。
    #[test]
    fn gif_item_is_disabled_without_a_path() {
        let cfg = Config::default();
        let s = State::from_config(&cfg);
        assert!(!s.enabled(&Command::SetIcon(IconMode::Gif)), "没配路径该不可点");
        assert!(s.enabled(&Command::SetIcon(IconMode::Clock)));
        assert!(s.enabled(&Command::SetAlpha(96)), "非图标项不该被牵连");

        let with = Config { tray_gif: Some("~/p/s.gif".into()), ..Config::default() };
        assert!(State::from_config(&with).enabled(&Command::SetIcon(IconMode::Gif)));
        // 只有空白也算没配
        let blank = Config { tray_gif: Some("   ".into()), ..Config::default() };
        assert!(!State::from_config(&blank).enabled(&Command::SetIcon(IconMode::Gif)));

        let labelled = build_nodes(&cfg.presets, false);
        let gif = labelled.iter().find(|x| x.label.starts_with("GIF 动图")).expect("菜单里该有 GIF 项");
        assert!(gif.label.contains("tray_gif"), "标签该说明为什么不可用: {}", gif.label);
        let ok = build_nodes(&cfg.presets, true);
        assert_eq!(ok.iter().find(|x| x.label.starts_with("GIF")).unwrap().label, "GIF 动图");
    }

    #[test]
    fn only_radio_items_are_checkable() {
        let n = build_nodes(&Config::default().presets, true);
        for node in &n {
            let Some(cmd) = node.command.as_ref() else { continue };
            let checkable = cmd.check().is_some();
            // 五个单选组，加上百分秒 / 显示秒 / 隐藏挂件 / 编辑态 / 弹通知 / 自启这几档勾选框
            // （SetHidden 与 SetEdit 是命令行走的设定值，菜单上没有它们对应的那一项）
            let in_group = matches!(
                cmd,
                Command::SetMode(_)
                    | Command::SetAlpha(_)
                    | Command::SetPalette(_)
                    | Command::SetRunningColor(_)
                    | Command::SetEffect(_)
                    | Command::SetIcon(_)
                    | Command::SetTimePad(_)
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
        let cfg = Config { mode: Mode::Countdown, bg_alpha: 96, ..Config::default() };
        let mut s = State::from_config(&cfg);
        assert!(s.checked(Check::Mode(Mode::Countdown)));
        assert!(!s.checked(Check::Mode(Mode::Pomodoro)));
        assert!(s.checked(Check::Alpha(96)));
        assert!(s.checked(Check::Icon(IconMode::Clock)));

        s.note(&Command::SetMode(Mode::Pomodoro));
        assert!(s.checked(Check::Mode(Mode::Pomodoro)));
        assert!(!s.checked(Check::Mode(Mode::Countdown)), "旧的那项必须取消勾选");

        s.note(&Command::SetAlpha(0));
        assert!(s.checked(Check::Alpha(0)) && !s.checked(Check::Alpha(96)));

        // 时长预设之类不影响任何勾选，别把它们误登记进去
        s.note(&Command::Preset(600));
        s.note(&Command::Start);
        assert!(s.checked(Check::Mode(Mode::Pomodoro)));

        // 百分秒是勾选框：点一下翻面，且不牵连任何单选组
        assert!(!s.checked(Check::Centiseconds), "默认该关着，开着的代价是心跳");
        s.note(&Command::ToggleCentiseconds);
        assert!(s.checked(Check::Centiseconds));
        assert!(s.checked(Check::Mode(Mode::Pomodoro)), "翻勾选框不该动了模式勾选");
        s.note(&Command::ToggleCentiseconds);
        assert!(!s.checked(Check::Centiseconds));
        let on = Config { centiseconds: true, ..Config::default() };
        assert!(State::from_config(&on).checked(Check::Centiseconds));

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

        let off = Config { color_done: Gradient::solid(0x123456), ..cfg };
        assert_eq!(State::from_config(&off).palette, None);
        let st = State::from_config(&off);
        for i in 0..PALETTES.len() {
            assert!(!st.checked(Check::Palette(i)), "自定义配色不该勾中第 {i} 套");
        }
        // 点一次预设就重新有得勾
        let mut st = st;
        st.note(&Command::SetPalette(4));
        assert!(st.checked(Check::Palette(4)) && !st.checked(Check::Palette(2)));
    }
}
