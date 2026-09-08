//! 配置持久化：`~/.config/tinyticker/config.conf`。
//!
//! 纯 std 实现（无 serde），行式 `key = value` 格式，`#` 开头为注释；
//! 未知键与非法值静默忽略并回落默认值。

use std::fs;
use std::path::PathBuf;

use crate::parse::parse_duration;
use crate::render::{parse_color, rgb};
use crate::timer::Mode;

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
    /// 窗口缩放倍数（滚轮调节，0.5-3.0；与 app.rs 滚轮 clamp 范围保持一致）。
    /// 下限 0.5 是因窗口按 LOGICAL_SIZE×zoom 计算，再小会裁切数字。
    pub zoom: f32,
    /// 透明区域是否让鼠标穿透：开启后只有文字范围接收点击（不再挡住下方窗口），
    /// 代价是拖动必须点中文字；关闭（默认）则整个矩形都可拖动。
    pub click_through: bool,
    /// 时钟挂件是否用 12 小时制（带 AM/PM）；false 为 24 小时制。
    pub clock_12h: bool,
    /// 番茄钟专注时长（秒）。
    pub pomo_work: u32,
    /// 番茄钟休息时长（秒）。
    pub pomo_break: u32,
    /// 倒计时归零 / 番茄钟专注完成时执行的 shell 命令
    /// （锁屏：`loginctl lock-session`，关机：`systemctl poweroff` 等）。
    pub on_finish: Option<String>,
    /// 上次退出时的窗口位置（X11/Win/macOS 生效，Wayland 忽略）。
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
            pomo_work: 1500,
            pomo_break: 300,
            on_finish: None,
            window_pos: None,
        }
    }
}

/// `$XDG_CONFIG_HOME/tinyticker/config.conf`，否则 `$HOME/.config/...`。
fn config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && PathBuf::from(&dir).is_absolute() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("tinyticker").join("config.conf"))
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
                "color_bg" => {
                    if let Some(c) = parse_color(value) {
                        cfg.color_bg = c;
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
                "click_through" => {
                    cfg.click_through = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes" | "on"
                    );
                }
                "clock_12h" => {
                    cfg.clock_12h = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes" | "on"
                    );
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
                "color_running" => {
                    if let Some(c) = parse_color(value) {
                        cfg.color_running = c;
                    }
                }
                "color_paused" => {
                    if let Some(c) = parse_color(value) {
                        cfg.color_paused = c;
                    }
                }
                "color_done" => {
                    if let Some(c) = parse_color(value) {
                        cfg.color_done = c;
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
        out.push_str(&format!("pomo_work = {}\n", self.pomo_work));
        out.push_str(&format!("pomo_break = {}\n", self.pomo_break));
        if let Some(cmd) = &self.on_finish {
            out.push_str(&format!("on_finish = {cmd}\n"));
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
            pomo_work: 1800,
            pomo_break: 600,
            on_finish: Some("loginctl lock-session".into()),
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
}
