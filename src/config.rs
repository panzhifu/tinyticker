//! 配置持久化：`config.conf`，放在 `$XDG_CONFIG_HOME/tinyticker/`，
//! 该变量缺失或非绝对路径时退回 `$HOME/.config/tinyticker/`。
//!
//! 纯 std 实现（无 serde / dirs），行式 `key = value` 格式，`#` 开头为注释；
//! 未知键与非法值静默忽略并回落默认值。

use std::fs;
use std::path::PathBuf;

use crate::parse::parse_duration;
use crate::render::{parse_color, rgb};
use crate::timer::{Mode, Pomo};
use crate::tray::IconMode;

const CONFIG_FILE: &str = "config.conf";

/// 单段时长的上限：24 小时。再长更可能是写错了单位而不是有意为之。
const MAX_SPAN: u32 = 86_400;

/// 出厂时长预设（秒）。菜单标签由 [`preset_label`] 现算，不再手写。
pub const DEFAULT_PRESETS: [u32; 6] = [60, 300, 900, 1500, 2700, 3600];

/// 预设条数上限：菜单再长就该分页了，而我们不分页。
const MAX_PRESETS: usize = 24;

/// 番茄钟轮数与组数的上限，免得一个手滑配出几千轮。
const MAX_ROUNDS: u32 = 100;

/// `presets = 25m 45m 1h` → 秒数列表。
///
/// 逗号或空格分隔均可。任一段非法（解析不出、为 0、超过 24 小时）或总段数超限，
/// 整条作废并回落到默认值——静默丢掉一项会让菜单悄悄少一格，更难查。
/// 因为空格就是分隔符，带空格的写法（`"1h 30m"`）不能用作单段，写 `"1h30m"`。
fn parse_preset_list(value: &str) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for token in value.split([',', ' ', '\t']) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let secs = parse_duration(token).filter(|s| *s > 0 && *s <= MAX_SPAN)?;
        out.push(secs);
    }
    (!out.is_empty() && out.len() <= MAX_PRESETS).then_some(out)
}

/// 番茄钟的时长键：越界（含 0，`long_break` / `cycles` 除外）返回 `None` 让调用方回落。
fn parse_span(value: &str, min: u32) -> Option<u32> {
    parse_duration(value).filter(|s| (min..=MAX_SPAN).contains(s))
}

/// 把秒数写成菜单上的中文时长。
///
/// 只念非零的位：`3600 → "1 小时"`、`5400 → "1 小时 30 分"`、`90 → "1 分 30 秒"`。
pub fn preset_label(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} 小时"));
    }
    if m > 0 {
        parts.push(format!("{m} 分"));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{s} 秒"));
    }
    parts.join(" ")
}

/// 托盘「外观 → 配色」子菜单套用的一组四色。
///
/// 只给预设、不做调色板：本项目没有对话框，改色要么点预设要么编辑配置文件。
pub struct Palette {
    pub name: &'static str,
    pub bg: u32,
    pub running: u32,
    pub paused: u32,
    pub done: u32,
}

/// 覆盖暗色 / 亮色 / 高对比三类场景；第一项就是出厂默认。
pub const PALETTES: [Palette; 6] = [
    Palette { name: "默认", bg: 0x0F0F14, running: 0xFFFFFF, paused: 0xFFC850, done: 0x50DC78 },
    Palette { name: "海洋", bg: 0x0A1420, running: 0x9AD4FF, paused: 0x5AA9FF, done: 0x7CF0C4 },
    Palette { name: "落日", bg: 0x1A0F0A, running: 0xFFD28A, paused: 0xFF9E3D, done: 0xFF5C7A },
    Palette { name: "紫罗兰", bg: 0x140A18, running: 0xE0B3FF, paused: 0xB366FF, done: 0x66FFB3 },
    Palette { name: "高对比", bg: 0x000000, running: 0xFFFFFF, paused: 0xFFFF00, done: 0x00FF00 },
    Palette { name: "纸白", bg: 0xF2F0EB, running: 0x1A1A1A, paused: 0x8A6D1A, done: 0x1F6F32 },
];

/// 托盘「外观 → 透明度」子菜单的档位（`bg_alpha`，0-255）。
///
/// 文字始终不透明，所以 0 档不是「看不见」而是「只剩文字」。
pub const ALPHA_STEPS: [(&str, u8); 5] = [
    ("全透明（只剩文字）", 0),
    ("淡", 48),
    ("中", 96),
    ("浓", 160),
    ("不透明", 255),
];

// zoom 为 f32（非 Eq），整体只做 PartialEq 比较
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// 倒计时默认总时长（秒）。
    pub duration_secs: u32,
    /// 启动时的计时模式。
    pub mode: Mode,
    /// 背景色。
    pub color_bg: u32,
    /// 背景不透明度（0=全透明，255=不透明）。文字始终不透明。
    pub bg_alpha: u8,
    /// 运行中数字颜色。
    pub color_running: u32,
    /// 暂停时数字颜色。
    pub color_paused: u32,
    /// 计时结束数字颜色。
    pub color_done: u32,
    /// 窗口缩放倍数（滚轮调节，0.5-3.0；与 `Widget::zoom_by` 的 clamp 范围保持一致）。
    /// 下限 0.5 是因窗口按 LOGICAL_SIZE×zoom 计算，再小会裁切数字。
    pub zoom: f32,
    /// 透明区域是否让鼠标穿透：开启后只有文字范围接收点击（不再挡住下方窗口），
    /// 代价是拖动必须点中文字；关闭（默认）则整个矩形都可拖动。
    pub click_through: bool,
    /// 时钟挂件是否用 12 小时制（带 AM/PM）；false 为 24 小时制。
    pub clock_12h: bool,
    /// 托盘图标显示什么：真实时表盘、CPU / 内存 / 电量的水位占用表，或动图。
    pub tray_icon: IconMode,
    /// 动图图标的路径（`tray_icon = gif` 时才有意义）。支持开头的 `~/`。
    pub tray_gif: Option<String>,
    /// 番茄钟节奏（专注 / 短休 / 长休 / 每几轮一长休 / 跑几组）。
    pub pomo: Pomo,
    /// 托盘「时长预设」子菜单的档位（秒，按配置顺序），点击即重置并开始。
    pub presets: Vec<u32>,
    /// 倒计时归零 / 番茄钟专注完成时执行的命令（经 `sh -c` 解释）。
    /// 例如锁屏 `loginctl lock-session`、关机 `systemctl poweroff`。
    pub on_finish: Option<String>,
    /// 外部文本源文件（可选）。非空时它的第一行会顶替状态行内容；
    /// 支持开头的 `~/`。文件缺失/为空/超限时状态行回到挂件自己的内容。
    pub text_source: Option<String>,
    /// 上次退出时的窗口位置（逻辑像素）：layer-shell 的 margin 与 X11 的窗口坐标。
    pub window_pos: Option<(i32, i32)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            duration_secs: 60,
            mode: Mode::Countdown,
            color_bg: rgb(15, 15, 20),
            bg_alpha: 0, // 默认完全透明：只悬浮文字；需要底色可在配置中调大（0-255）
            color_running: rgb(255, 255, 255),
            color_paused: rgb(255, 200, 80),
            color_done: rgb(80, 220, 120),
            zoom: 1.0,
            click_through: false,
            clock_12h: false,
            tray_icon: IconMode::Clock,
            tray_gif: None,
            pomo: Pomo::default(),
            presets: DEFAULT_PRESETS.to_vec(),
            on_finish: None,
            text_source: None,
            window_pos: None,
        }
    }
}

/// 展开配置里开头的 `~/`。std 不做这件事，而让用户在配置文件里写绝对路径太难看。
/// 中间出现的 `~` 不展开；`HOME` 不在时原样返回。
pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    let raw = raw.trim();
    let Some(rest) = raw.strip_prefix("~/").or(if raw == "~" { Some("") } else { None }) else {
        return PathBuf::from(raw);
    };
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::from(raw);
    };
    if rest.is_empty() { PathBuf::from(home) } else { PathBuf::from(home).join(rest) }
}

/// 环境变量值只有是非空绝对路径时才算数：相对路径会让配置跟着当前工作目录漂移。
fn absolute(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let value = value?;
    let path = PathBuf::from(&value);
    (!value.is_empty() && path.is_absolute()).then_some(path)
}

fn env_dir(name: &str) -> Option<PathBuf> {
    absolute(std::env::var_os(name))
}

/// 配置文件完整路径。XDG 优先，缺省 `$HOME/.config`；两个环境变量都不合格时返回
/// `None`，此时 `load` / `save` 静默走默认值（不读写文件）。
pub fn config_path() -> Option<PathBuf> {
    let dir =
        env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|home| home.join(".config")))?;
    Some(dir.join(env!("CARGO_PKG_NAME")).join(CONFIG_FILE))
}

impl Config {
    /// 读取配置；文件不存在或读取失败时返回默认值。
    pub fn load() -> Config {
        let Some(path) = config_path() else {
            return Config::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => Config::from_str(&text),
            Err(_) => Config::default(),
        }
    }

    /// 写回配置；失败只打印警告（不影响运行）。
    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        let write = |text: &str| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, text)
        };
        if let Err(e) = write(&self.serialize()) {
            eprintln!("⚠️ 无法保存配置 {}: {e}", path.display());
        }
    }

    fn from_str(text: &str) -> Config {
        let mut cfg = Config::default();
        let mut window_x: Option<i32> = None;
        let mut window_y: Option<i32> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "duration" => {
                    if let Some(secs) = parse_duration(value) {
                        cfg.duration_secs = secs;
                    }
                }
                "mode" => {
                    if let Some(mode) = Mode::from_name(value) {
                        cfg.mode = mode;
                    }
                }
                "tray_icon" => {
                    if let Some(m) = IconMode::from_name(value) {
                        cfg.tray_icon = m;
                    }
                }
                "tray_gif" => {
                    if !value.is_empty() {
                        cfg.tray_gif = Some(value.to_string());
                    }
                }
                "bg_alpha" => {
                    if let Ok(a) = value.parse::<u8>() {
                        cfg.bg_alpha = a;
                    }
                }
                "zoom" => {
                    if let Ok(z) = value.parse::<f32>()
                        && (0.5..=3.0).contains(&z)
                    {
                        cfg.zoom = z;
                    }
                }
                "click_through" | "clock_12h" => {
                    let on = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes" | "on"
                    );
                    if key == "click_through" {
                        cfg.click_through = on;
                    } else {
                        cfg.clock_12h = on;
                    }
                }
                "presets" => {
                    if let Some(list) = parse_preset_list(value) {
                        cfg.presets = list;
                    }
                }
                "pomo_work" => {
                    if let Some(s) = parse_span(value, 1) {
                        cfg.pomo.work = s;
                    }
                }
                "pomo_break" => {
                    if let Some(s) = parse_span(value, 1) {
                        cfg.pomo.short_break = s;
                    }
                }
                "pomo_long_break" => {
                    // 0 是合法值：不安排长休息
                    if let Some(s) = parse_span(value, 0) {
                        cfg.pomo.long_break = s;
                    }
                }
                "pomo_rounds" => {
                    // 一轮至少一轮
                    if let Ok(n) = value.parse::<u32>()
                        && (1..=MAX_ROUNDS).contains(&n)
                    {
                        cfg.pomo.rounds = n;
                    }
                }
                "pomo_cycles" => {
                    // 0 是合法值：不限组数，一直轮转
                    if let Ok(n) = value.parse::<u32>() && n <= MAX_ROUNDS {
                        cfg.pomo.cycles = n;
                    }
                }
                "on_finish" => {
                    if !value.is_empty() {
                        cfg.on_finish = Some(value.to_string());
                    }
                }
                "text_source" => {
                    if !value.is_empty() {
                        cfg.text_source = Some(value.to_string());
                    }
                }
                "color_bg" | "color_running" | "color_paused" | "color_done" => {
                    if let Some(c) = parse_color(value) {
                        match key {
                            "color_bg" => cfg.color_bg = c,
                            "color_running" => cfg.color_running = c,
                            "color_paused" => cfg.color_paused = c,
                            _ => cfg.color_done = c,
                        }
                    }
                }
                "window_x" => window_x = value.parse().ok(),
                "window_y" => window_y = value.parse().ok(),
                _ => {}
            }
        }
        if let (Some(x), Some(y)) = (window_x, window_y) {
            cfg.window_pos = Some((x, y));
        }
        cfg
    }

    fn serialize(&self) -> String {
        let mut out = String::from("# tinyticker 配置（手动编辑后重启生效）\n");
        out.push_str(&format!("duration = {}\n", self.duration_secs));
        let presets: Vec<String> = self.presets.iter().map(u32::to_string).collect();
        out.push_str(&format!("presets = {}\n", presets.join(", ")));
        out.push_str(&format!("mode = {}\n", self.mode.name()));
        out.push_str(&format!("bg_alpha = {}\n", self.bg_alpha));
        out.push_str(&format!("zoom = {:.2}\n", self.zoom));
        out.push_str(&format!("click_through = {}\n", self.click_through));
        out.push_str(&format!("clock_12h = {}\n", self.clock_12h));
        out.push_str(&format!("tray_icon = {}\n", self.tray_icon.name()));
        if let Some(path) = &self.tray_gif {
            out.push_str(&format!("tray_gif = {path}\n"));
        }
        out.push_str(&format!("pomo_work = {}\n", self.pomo.work));
        out.push_str(&format!("pomo_break = {}\n", self.pomo.short_break));
        out.push_str(&format!("pomo_long_break = {}\n", self.pomo.long_break));
        out.push_str(&format!("pomo_rounds = {}\n", self.pomo.rounds));
        out.push_str(&format!("pomo_cycles = {}\n", self.pomo.cycles));
        if let Some(cmd) = &self.on_finish {
            out.push_str(&format!("on_finish = {cmd}\n"));
        }
        if let Some(path) = &self.text_source {
            out.push_str(&format!("text_source = {path}\n"));
        }
        for (key, color) in [
            ("color_bg", self.color_bg),
            ("color_running", self.color_running),
            ("color_paused", self.color_paused),
            ("color_done", self.color_done),
        ] {
            out.push_str(&format!("{key} = {:06x}\n", color & 0xFFFFFF));
        }
        if let Some((x, y)) = self.window_pos {
            out.push_str(&format!("window_x = {x}\nwindow_y = {y}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_all_fields() {
        let cfg = Config {
            duration_secs: 1500,
            mode: Mode::Pomodoro,
            color_bg: 0x112233,
            color_done: 0xAABBCC,
            bg_alpha: 120,
            zoom: 1.75,
            tray_icon: IconMode::Gif,
            tray_gif: Some("~/pics/spin.gif".into()),
            pomo: Pomo { work: 1800, short_break: 600, long_break: 1200, rounds: 3, cycles: 2 },
            presets: vec![90, 600, 5400],
            on_finish: Some("loginctl lock-session".into()),
            text_source: Some("~/tmp/tinyticker-out.txt".into()),
            window_pos: Some((-10, 200)),
            ..Config::default()
        };
        let parsed = Config::from_str(&cfg.serialize());
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn garbage_falls_back_to_defaults() {
        let cfg = Config::from_str("这不是配置\nfoo=bar\nduration=abc\ncolor_bg=zzz\n");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn tilde_expansion_only_applies_at_the_front() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        assert_eq!(expand_tilde("~/x/y"), home.join("x/y"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("  ~/a  "), home.join("a"));
        assert_eq!(expand_tilde("/tmp/~/a"), PathBuf::from("/tmp/~/a"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn palette_presets_are_distinct_and_in_gamut() {
        assert_eq!(PALETTES[0].name, "默认");
        // 第一套必须就是出厂默认，否则「默认」菜单项会改变外观
        let d = Config::default();
        assert_eq!(PALETTES[0].bg, d.color_bg);
        assert_eq!(PALETTES[0].running, d.color_running);
        assert_eq!(PALETTES[0].paused, d.color_paused);
        assert_eq!(PALETTES[0].done, d.color_done);
        for p in &PALETTES {
            for c in [p.bg, p.running, p.paused, p.done] {
                assert!(c <= 0xFFFFFF, "{} 的颜色超出 24 位: {c:#08x}", p.name);
            }
        }
        // 名字不能重复，菜单里靠它辨认
        for (i, a) in PALETTES.iter().enumerate() {
            for b in &PALETTES[i + 1..] {
                assert_ne!(a.name, b.name);
            }
        }
    }

    #[test]
    fn alpha_steps_ascend_and_roundtrip() {
        assert_eq!(ALPHA_STEPS[0].1, 0);
        assert_eq!(ALPHA_STEPS[ALPHA_STEPS.len() - 1].1, 255);
        assert!(ALPHA_STEPS.windows(2).all(|w| w[0].1 < w[1].1));
        // 每一档都得能被序列化-解析原样取回
        for &(label, value) in &ALPHA_STEPS {
            let cfg = Config::from_str(&format!("bg_alpha = {value}\n"));
            assert_eq!(cfg.bg_alpha, value, "{label} 档没走通");
        }
    }

    #[test]
    fn partial_file_overrides_only_present_keys() {
        let cfg = Config::from_str("# 注释\nduration = 25m\ncolor_bg = #332211\n");
        assert_eq!(cfg.duration_secs, 1500);
        assert_eq!(cfg.color_bg, 0x332211);
        assert_eq!(cfg.mode, Mode::Countdown);
        assert_eq!(cfg.window_pos, None); // 只有 x 没有 y → 不生效
    }

    /// 预设既认逗号也认空格；序列化后要能原样读回。
    #[test]
    fn presets_parse_both_separators_and_roundtrip() {
        assert_eq!(Config::from_str("presets = 1m,5m,1h\n").presets, vec![60, 300, 3600]);
        assert_eq!(Config::from_str("presets = 90 300\n").presets, vec![90, 300]);
        let cfg = Config { presets: vec![90, 5400], ..Config::default() };
        assert_eq!(Config::from_str(&cfg.serialize()).presets, cfg.presets);
        // 出厂那几档必须原样写回，否则升级一次配置就把用户菜单换了
        let d = Config::default();
        assert_eq!(Config::from_str(&d.serialize()).presets, DEFAULT_PRESETS.to_vec());
    }

    /// 一段非法就整条作废：宁可回到默认 6 档，也不要一个悄悄少了一格的菜单。
    #[test]
    fn one_bad_preset_token_rejects_the_whole_line() {
        let bad = [
            "presets = 1m,abc\n", // 解析不出
            "presets = 1m,0\n",   // 0 秒的预设没有意义
            "presets = 1m,25h\n", // 超过 24 小时
            "presets = ,,\n",     // 一段都不剩
        ];
        for line in bad {
            assert_eq!(
                Config::from_str(line).presets,
                DEFAULT_PRESETS.to_vec(),
                "非法预设没被整条拒绝: {line}"
            );
        }
        // 条数超限同样整条作废
        let too_many = format!("presets = {}\n", vec!["1m"; MAX_PRESETS + 1].join(","));
        assert_eq!(Config::from_str(&too_many).presets, DEFAULT_PRESETS.to_vec());
        assert_eq!(
            Config::from_str(&format!("presets = {}\n", vec!["1m"; MAX_PRESETS].join(",")))
                .presets
                .len(),
            MAX_PRESETS,
            "刚好 24 段该收"
        );
    }

    /// 空格是分隔符，所以 "1h 30m" 是两段而不是一小时半——连写请用 "1h30m"。
    #[test]
    fn spaces_split_presets_rather_than_compose() {
        assert_eq!(Config::from_str("presets = 1h 30m\n").presets, vec![3600, 1800]);
        assert_eq!(Config::from_str("presets = 1h30m\n").presets, vec![5400]);
    }

    #[test]
    fn preset_label_reads_like_chinese() {
        let cases = [
            (45, "45 秒"),
            (60, "1 分"),
            (90, "1 分 30 秒"),
            (300, "5 分"),
            (1500, "25 分"),
            (3600, "1 小时"),
            (5400, "1 小时 30 分"),
            (86_400, "24 小时"),
        ];
        for (secs, want) in cases {
            assert_eq!(preset_label(secs), want, "{secs} 秒的标签不对");
        }
    }

    /// 番茄钟默认值：4 轮一长休、不限组数——不限是为了保住旧版"永远轮转"的行为。
    #[test]
    fn pomo_defaults_keep_the_old_endless_behavior() {
        let p = Pomo::default();
        assert_eq!((p.work, p.short_break), (1500, 300));
        assert_eq!((p.rounds, p.cycles, p.long_break), (4, 0, 900));
        assert_eq!(Config::default().pomo, p);
    }

    #[test]
    fn pomo_new_keys_parse_and_reject_out_of_range() {
        let cfg = Config::from_str("pomo_long_break = 20m\npomo_rounds = 6\npomo_cycles = 2\n");
        assert_eq!((cfg.pomo.long_break, cfg.pomo.rounds, cfg.pomo.cycles), (1200, 6, 2));
        // long_break = 0 是"关闭"、cycles = 0 是"不限"，都得收
        let off = Config::from_str("pomo_long_break = 0\npomo_cycles = 0\n");
        assert_eq!((off.pomo.long_break, off.pomo.cycles), (0, 0));
        // rounds = 0 会除零似地数不出组，超限和越界的时长一样都该拒
        let bad = Config::from_str("pomo_rounds = 0\npomo_cycles = 101\npomo_long_break = 25h\n");
        assert_eq!(bad.pomo, Pomo::default());
    }

    #[test]
    fn click_through_parses_boolean_aliases() {
        for on in ["true", "1", "yes", "on", "TRUE", "Yes"] {
            assert!(
                Config::from_str(&format!("click_through = {on}\n")).click_through,
                "{on} 应解析为 true"
            );
        }
        for off in ["false", "0", "no", "off", "", "whatever"] {
            assert!(
                !Config::from_str(&format!("click_through = {off}\n")).click_through,
                "{off} 应解析为 false"
            );
        }
        assert!(!Config::default().click_through); // 默认整窗可拖动
    }

    #[test]
    fn window_position_needs_both_axes() {
        let cfg = Config::from_str("window_x = 5\nwindow_y = -7\n");
        assert_eq!(cfg.window_pos, Some((5, -7)));
    }

    #[test]
    fn empty_or_relative_env_values_are_ignored() {
        assert_eq!(absolute(None), None);
        assert_eq!(absolute(Some("".into())), None);
        assert_eq!(absolute(Some("relative/dir".into())), None);
        // temp_dir 总是绝对路径，正向用例因此与主机无关
        let abs = std::env::temp_dir();
        assert_eq!(absolute(Some(abs.clone().into_os_string())), Some(abs));
    }

    #[test]
    fn config_path_ends_with_app_and_file() {
        let Some(path) = config_path() else {
            return; // 环境里连 HOME 都没有，load/save 会静默走默认值
        };
        assert_eq!(path.file_name(), Some(std::ffi::OsStr::new(CONFIG_FILE)));
        assert_eq!(
            path.parent().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new(env!("CARGO_PKG_NAME")))
        );
    }
}
