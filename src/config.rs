//! 配置持久化：`config.conf`，放在 `$XDG_CONFIG_HOME/tinyticker/`，
//! 该变量缺失或非绝对路径时退回 `$HOME/.config/tinyticker/`。
//!
//! 纯 std 实现（无 serde / dirs），行式 `key = value` 格式，`#` 开头为注释；
//! 未知键与非法值静默忽略并回落默认值。

use std::fs;
use std::path::PathBuf;

use crate::parse::parse_duration;
use crate::render::{parse_color, rgb};
use crate::timer::Mode;
use crate::tray::IconMode;

const CONFIG_FILE: &str = "config.conf";

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
    /// 托盘图标显示什么：真实时表盘，或 CPU / 内存 / 电量的水位占用表。
    pub tray_icon: IconMode,
    /// 番茄钟专注时长（秒）。
    pub pomo_work: u32,
    /// 番茄钟休息时长（秒）。
    pub pomo_break: u32,
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
            pomo_work: 1500,
            pomo_break: 300,
            on_finish: None,
            text_source: None,
            window_pos: None,
        }
    }
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
                "pomo_work" => {
                    if let Some(s) = parse_duration(value) {
                        cfg.pomo_work = s;
                    }
                }
                "pomo_break" => {
                    if let Some(s) = parse_duration(value) {
                        cfg.pomo_break = s;
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
        out.push_str(&format!("mode = {}\n", self.mode.name()));
        out.push_str(&format!("bg_alpha = {}\n", self.bg_alpha));
        out.push_str(&format!("zoom = {:.2}\n", self.zoom));
        out.push_str(&format!("click_through = {}\n", self.click_through));
        out.push_str(&format!("clock_12h = {}\n", self.clock_12h));
        out.push_str(&format!("tray_icon = {}\n", self.tray_icon.name()));
        out.push_str(&format!("pomo_work = {}\n", self.pomo_work));
        out.push_str(&format!("pomo_break = {}\n", self.pomo_break));
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
            tray_icon: IconMode::Battery,
            pomo_work: 1800,
            pomo_break: 600,
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
