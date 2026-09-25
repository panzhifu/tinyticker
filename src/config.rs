//! 配置持久化：`config.conf`，放在 `$XDG_CONFIG_HOME/tinyticker/`，
//! 该变量缺失或非绝对路径时退回 `$HOME/.config/tinyticker/`。
//!
//! 纯 std 实现（无 serde / dirs），行式 `key = value` 格式，`#` 开头为注释；
//! 未知键与非法值静默忽略并回落默认值。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::effect::{Effect, Gradient};
use crate::parse::parse_duration;
use crate::render::{Pad, parse_color, rgb};
use crate::timer::{Mode, Pomo};
use crate::tray::IconMode;

const CONFIG_FILE: &str = "config.conf";

/// 单段时长的上限：24 小时。再长更可能是写错了单位而不是有意为之。
const MAX_SPAN: u32 = 86_400;

/// 缩放倍数的上下限。做成常量是因为这两头以前各写一份字面量（配置解析里一份、
/// `Widget::zoom_by` 里一份），改上限必然漏掉一边。
///
/// 下限 0.5：窗口按 `LOGICAL_SIZE × zoom` 算，再小会裁切数字。
/// 上限 6.0：200×100 的逻辑画布放到 1200×600，够当一块小投影屏用了。
pub const ZOOM_MIN: f32 = 0.5;
pub const ZOOM_MAX: f32 = 6.0;

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

/// 数字行颜色的 30 条预设，**逐条取自 Catime** 的 `DEFAULT_COLOR_OPTIONS_INI`
/// （`include/config/config_constants.h:46-52`）：9 个纯色 + 20 条两段渐变 +
/// 1 条五段渐变。值串格式与我们自己的 `color_running` 完全一致（`_` 分隔、
/// 超过两个停靠点会自动流动），所以这里是原值照抄而不是另编一套。
///
/// 与 [`PALETTES`] 的分工：那一组是"四色一套"的整体气质预设（含背景与三态），
/// 这一组只换运行中的数字色，给想要单个颜色的人。
pub const COLOR_OPTIONS: [&str; 30] = [
    "#FFFFFF",
    "#E3E3E5",
    "#000000",
    "#FF5F5F",
    "#F6ABB7",
    "#FB7FA4",
    "#F59E0B",
    "#22C55E",
    "#8771C6",
    "#FF9A9E_#FECFEF",
    "#FEA5B7_#FFDE9B",
    "#A8EDEA_#FED6E3",
    "#D299C2_#FEF9D7",
    "#FF9966_#FF5E62",
    "#ED4264_#FFEDBC",
    "#F6D365_#FDA085",
    "#FFE985_#FA742B",
    "#FF9A56_#56CCBA",
    "#10BD92_#8CE442",
    "#11998E_#38EF7D",
    "#43E97B_#38F9D7",
    "#FFFFFF_#00FFFF",
    "#89F7FE_#66A6FF",
    "#00C9FF_#92FE9D",
    "#648CFF_#64DC78",
    "#1F92A9_#EEE0D5",
    "#8E9EF3_#F774A0",
    "#FF5E96_#56C6FF",
    "#30CFD0_#330867",
    "#FFA745_#FE869F_#EF7AC8_#A083ED_#43AEFF",
];

/// 预设颜色在菜单上的标签：去掉 `#`、把 `_` 换成 `→`，读起来就是"这串值长什么样"。
///
/// 不给它们编中文名：30 个名字是我造出来的数据，而这一串恰好就是配置文件里要写的值。
pub fn color_label(value: &str) -> String {
    value.replace('#', "").replace('_', "→")
}

/// 托盘「外观 → 配色」子菜单套用的一组四色。
///
/// 只给预设、不做调色板对话框：改色要么点这一组、要么点「文字颜色」那 30 条、
/// 要么编辑配置文件（工作区版本起改了即时生效）。
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

/// 状态行里点阵覆盖不到的码位（中文、Latin-1、符号）用什么字形补。
///
/// 见 [`crate::text`]：内置 8x8 点阵始终优先，这个开关只决定"点阵没有的那些字"
/// 要不要去问一次 libfreetype。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TextFont {
    /// 自动：运行时 `dlopen` libfreetype，并按 [`crate::text`] 里的偏好找一块中文字体。
    #[default]
    Auto,
    /// 完全不用外部字体：非 ASCII 一律留空位（v0.5.0 之前的行为）。
    Off,
    /// 指定字体文件，支持开头的 `~/`。打不开会警告并退回自动发现。
    Path(String),
}

impl TextFont {
    /// 值串与 `Effect` / `IconMode` 同族：小写关键字，其余一律当作路径。
    fn from_value(value: &str) -> Self {
        match value {
            "" | "auto" => TextFont::Auto,
            "off" | "none" => TextFont::Off,
            other => TextFont::Path(other.to_string()),
        }
    }

    fn to_value(&self) -> String {
        match self {
            TextFont::Auto => "auto".to_string(),
            TextFont::Off => "off".to_string(),
            TextFont::Path(p) => p.clone(),
        }
    }
}

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
    /// 运行中数字颜色（可写 `_` 分隔的渐变）。
    pub color_running: Gradient,
    /// 暂停时数字颜色（可写渐变）。
    pub color_paused: Gradient,
    /// 计时结束数字颜色（可写渐变）。
    pub color_done: Gradient,
    /// 文字特效。渐变与特效都只作用于文字，背景不受影响。
    pub text_effect: Effect,
    /// 窗口缩放倍数（滚轮调节，`ZOOM_MIN`-`ZOOM_MAX`；两端共用同一对常量）。
    /// 下限 0.5 是因窗口按 LOGICAL_SIZE×zoom 计算，再小会裁切数字。
    pub zoom: f32,
    /// 透明区域是否让鼠标穿透：开启后只有文字范围接收点击（不再挡住下方窗口），
    /// 代价是拖动必须点中文字；关闭（默认）则整个矩形都可拖动。
    pub click_through: bool,
    /// 时钟挂件是否用 12 小时制（带 AM/PM）；false 为 24 小时制。
    pub clock_12h: bool,
    /// 时钟挂件要不要显示秒；false 则只到分（`HH:MM`）。
    pub clock_seconds: bool,
    /// 计时数字行的补零档位（`none` / `zero` / `full`）。
    pub time_pad: Pad,
    /// 数字行是否显示百分之一秒（`45.32s` / `m:ss.cc`）。
    /// 开启且计时器在跑时心跳从 200ms 提到 20ms——实测单核 0.9%，整秒档 < 0.1%。
    pub centiseconds: bool,
    /// 托盘图标显示什么：真实时表盘、CPU / 内存 / 电量的水位占用表，或动图。
    pub tray_icon: IconMode,
    /// 动图图标的路径（`tray_icon = gif` 时才有意义）。支持开头的 `~/`。
    pub tray_gif: Option<String>,
    /// 番茄钟节奏（专注 / 短休 / 长休 / 每几轮一长休 / 跑几组）。
    pub pomo: Pomo,
    /// 托盘「时长预设」子菜单的档位（秒，按配置顺序），点击即重置并开始。
    pub presets: Vec<u32>,
    /// 倒计时归零 / 番茄钟跑完时执行的命令（经 `sh -c` 解释）。
    /// 例如锁屏 `loginctl lock-session`、关机 `systemctl poweroff`。
    pub on_finish: Option<String>,
    /// 到点之后数字行显示什么（对齐 Catime 的 `CLOCK_TIMEOUT_TEXT`）。
    ///
    /// 空 = 保持现状（显示 `0s` / `0.00s`）；`"0"` = 整行留空；其它文本（可中文）
    /// 直接顶替数字。状态行仍会写 `DONE`，所以留空不会让人以为程序没了。
    pub timeout_text: Option<String>,
    /// 计时结束要不要发桌面通知。false = 一声不响（`on_finish` 命令照旧执行）。
    pub notify: bool,
    /// 通知正文的自定义写法。空 = 按事件用默认文案（专注完成 / 休息结束 …各说各的）。
    pub notify_text: Option<String>,
    /// 外部文本源文件（可选）。非空时它的第一行会顶替状态行内容；
    /// 支持开头的 `~/`。文件缺失/为空/超限时状态行回到挂件自己的内容。
    pub text_source: Option<String>,
    /// 点阵补不出的码位（中文等）用什么字形。
    pub text_font: TextFont,
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
            color_running: Gradient::solid(rgb(255, 255, 255)),
            color_paused: Gradient::solid(rgb(255, 200, 80)),
            color_done: Gradient::solid(rgb(80, 220, 120)),
            text_effect: Effect::None,
            zoom: 1.0,
            click_through: false,
            clock_12h: false,
            clock_seconds: true,
            time_pad: Pad::default(),
            centiseconds: false,
            tray_icon: IconMode::Clock,
            tray_gif: None,
            pomo: Pomo::default(),
            presets: DEFAULT_PRESETS.to_vec(),
            on_finish: None,
            timeout_text: None,
            notify: true,
            notify_text: None,
            text_source: None,
            text_font: TextFont::default(),
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

/// `--config-dir` 指定的目录。启动时设一次，之后配置路径与单实例套接字都从这里取。
static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 记下 `--config-dir` 的值。只在启动时调用一次，重复调用以第一次为准（`OnceLock`）。
pub fn set_config_dir(path: &str) {
    let _ = CONFIG_DIR.set(PathBuf::from(path));
}

/// 配置目录是否被显式指定过（`--config-dir` 或环境变量）。
///
/// 套接字路径要问这个而不是问 [`config_dir`]：正常的 XDG 目录里放套接字是错的，
/// 只有"用户明确要另一套独立实例"时套接字才该跟着搬进那个目录。
pub fn config_dir_override() -> Option<PathBuf> {
    CONFIG_DIR.get().cloned().or_else(|| {
        std::env::var_os("TINYTICKER_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

/// 配置根目录（`$XDG_CONFIG_HOME`，缺省 `$HOME/.config`）的纯函数部分。
///
/// 值只有是非空**绝对路径**时才算数：相对路径会让配置跟着当前工作目录漂移。
fn base_dir(xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    absolute(xdg.map(std::ffi::OsString::from))
        .or_else(|| absolute(home.map(std::ffi::OsString::from)).map(|h| h.join(".config")))
}

/// 路径优先级的纯函数部分，单测直接喂三档值进来（进程级环境变量在并行测试里不可靠）。
///
/// `explicit` = `--config-dir` / `$TINYTICKER_CONFIG_DIR`；它给的是**相对路径**时按
/// 当前工作目录解释——本进程不会 chdir，所以这是稳定的。
fn resolve_config_dir(explicit: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(base_dir(xdg, home)?.join(env!("CARGO_PKG_NAME")))
}

/// `$XDG_CONFIG_HOME` 或 `$HOME/.config`。自启条目与配置目录共用这一个根。
pub fn config_base() -> Option<PathBuf> {
    base_dir(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// 当前配置目录：`--config-dir` / `$TINYTICKER_CONFIG_DIR` > `$XDG_CONFIG_HOME/tinyticker`
/// > `$HOME/.config/tinyticker`。三档都不合格时 `None`，此时不读写文件。
pub fn config_dir() -> Option<PathBuf> {
    let explicit = config_dir_override().map(|p| p.to_string_lossy().into_owned());
    resolve_config_dir(
        explicit.as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// 配置文件完整路径。XDG 优先，缺省 `$HOME/.config`；两个环境变量都不合格时返回
/// `None`，此时 `load` / `save` 静默走默认值（不读写文件）。
pub fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join(CONFIG_FILE))
}

/// 开机自启条目。文件名与打包的 desktop 文件**同名**——freedesktop 的自启机制就是
/// 往 `~/.config/autostart/` 放一份同名副本，用户想关掉直接删这个文件即可。
const AUTOSTART_FILE: &str = "io.github.panzhifu.tinyticker.desktop";

/// 自启条目的路径（没有合格的配置根时 `None`）。
fn autostart_path() -> Option<PathBuf> {
    Some(config_base()?.join("autostart").join(AUTOSTART_FILE))
}

/// `.desktop` 的内容。`Exec` 用当前可执行文件的真实路径：开发构建下它会指向
/// `target/release/tinyticker`，那是诚实的行为而不是 bug。
fn autostart_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=TinyTicker\n\
         Comment=极简悬浮计时器\n\
         Exec={exec}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// 当前是不是已经登记了开机自启（判据就是那个文件在不在）。
pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|p| p.exists())
}

/// 翻面：没有就写一份，有就删掉。返回翻转后的状态。
pub fn toggle_autostart() -> std::io::Result<bool> {
    let Some(path) = autostart_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "环境里没有合格的 HOME / XDG_CONFIG_HOME",
        ));
    };
    if path.exists() {
        fs::remove_file(&path)?;
        return Ok(false);
    }
    let exec = std::env::current_exe()?.to_string_lossy().into_owned();
    write_atomic(&path, &autostart_entry(&exec))?;
    Ok(true)
}

/// 原子写：先写同目录的临时文件、`sync_all` 落盘，再 `rename` 覆盖目标。
///
/// 同目录 `rename` 在 POSIX 下是原子的，所以掉电或进程被杀最多丢掉这一次写回，不会留下
/// 半截配置——那会让下次启动读到截断的键值。（目录项本身没有 fsync，所以极端掉电下的
/// 后果是"退回上一版配置"，而不是"配置被写坏"。）
fn write_atomic(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // 临时名带上 pid：同时有两个进程在写（例如另一份 HOME 起了第二个实例）也不互相踩
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    let out = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        // 数据先落盘再 rename：反过来做，掉电后可能剩下一个空的正式文件
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if out.is_err() {
        let _ = fs::remove_file(&tmp); // 半截的临时文件不该留在配置目录里
    }
    out
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
        if let Err(e) = write_atomic(&path, &self.serialize()) {
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
                        && (ZOOM_MIN..=ZOOM_MAX).contains(&z)
                    {
                        cfg.zoom = z;
                    }
                }
                "click_through" | "clock_12h" | "clock_seconds" | "centiseconds" | "notify" => {
                    let on = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes" | "on"
                    );
                    match key {
                        "click_through" => cfg.click_through = on,
                        "clock_12h" => cfg.clock_12h = on,
                        "clock_seconds" => cfg.clock_seconds = on,
                        "notify" => cfg.notify = on,
                        _ => cfg.centiseconds = on,
                    }
                }
                "time_pad" => {
                    // 认不出来的值留默认档：这一档写错只是不补零，不像路径那样需要警告
                    if let Some(p) = Pad::from_name(value) {
                        cfg.time_pad = p;
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
                "timeout_text" => {
                    if !value.is_empty() {
                        cfg.timeout_text = Some(value.to_string());
                    }
                }
                "notify_text" => {
                    if !value.is_empty() {
                        cfg.notify_text = Some(value.to_string());
                    }
                }
                "text_source" => {
                    if !value.is_empty() {
                        cfg.text_source = Some(value.to_string());
                    }
                }
                // 除 off/auto 外一律当路径收下：写错路径由 text::init 在启动时警告，
                // 不在解析阶段悄悄丢掉——那样用户无从知道自己拼错了。
                "text_font" => {
                    cfg.text_font = TextFont::from_value(value);
                }
                "color_bg" => {
                    if let Some(c) = parse_color(value) {
                        cfg.color_bg = c;
                    }
                }
                // 文字三色可写成 `_` 分隔的渐变；背景保持单色——渐变是按横向铺满
                // 文字采样的，背景再来一层只会和文字互相干扰
                "color_running" | "color_paused" | "color_done" => {
                    if let Some(g) = Gradient::parse(value) {
                        match key {
                            "color_running" => cfg.color_running = g,
                            "color_paused" => cfg.color_paused = g,
                            _ => cfg.color_done = g,
                        }
                    }
                }
                "text_effect" => {
                    if let Some(e) = Effect::from_name(value) {
                        cfg.text_effect = e;
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
        let mut out = String::from("# tinyticker 配置（手动编辑后即时生效，见 README「配置」一节）\n");
        out.push_str(&format!("duration = {}\n", self.duration_secs));
        let presets: Vec<String> = self.presets.iter().map(u32::to_string).collect();
        out.push_str(&format!("presets = {}\n", presets.join(", ")));
        out.push_str(&format!("mode = {}\n", self.mode.name()));
        out.push_str(&format!("bg_alpha = {}\n", self.bg_alpha));
        out.push_str(&format!("zoom = {:.2}\n", self.zoom));
        out.push_str(&format!("click_through = {}\n", self.click_through));
        out.push_str(&format!("clock_12h = {}\n", self.clock_12h));
        out.push_str(&format!("clock_seconds = {}\n", self.clock_seconds));
        out.push_str(&format!("time_pad = {}\n", self.time_pad.name()));
        out.push_str(&format!("centiseconds = {}\n", self.centiseconds));
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
        if let Some(text) = &self.timeout_text {
            out.push_str(&format!("timeout_text = {text}\n"));
        }
        out.push_str(&format!("notify = {}\n", self.notify));
        if let Some(text) = &self.notify_text {
            out.push_str(&format!("notify_text = {text}\n"));
        }
        if let Some(path) = &self.text_source {
            out.push_str(&format!("text_source = {path}\n"));
        }
        out.push_str(&format!("text_font = {}\n", self.text_font.to_value()));
        out.push_str(&format!("color_bg = {:06x}\n", self.color_bg & 0xFFFFFF));
        for (key, grad) in [
            ("color_running", &self.color_running),
            ("color_paused", &self.color_paused),
            ("color_done", &self.color_done),
        ] {
            out.push_str(&format!("{key} = {}\n", grad.to_config_string()));
        }
        out.push_str(&format!("text_effect = {}\n", self.text_effect.name()));
        if let Some((x, y)) = self.window_pos {
            out.push_str(&format!("window_x = {x}\nwindow_y = {y}\n"));
        }
        out
    }
}

/// 配置文件的变更探测：与 `textsrc` 同一手法，每拍一次 `stat`，靠 `(大小, mtime)`
/// 判断要不要重读。
///
/// 为什么不用 inotify：Catime 那边其实是"目录 watcher + stat 兜底"两套并行
/// （`config_watcher_thread.c` 与 `config_ini_read.c` 的 100 ms 节流兜底），我们只做
/// 兜底那一套——它已经把延迟压进一个心跳以内，代价是一个 syscall，而 inotify 要新开
/// 一组 libc FFI、一条阻塞线程和它的 fd 生命周期。手改配置的延迟敏感不到哪里去。
pub struct Watch {
    path: Option<PathBuf>,
    /// 上次看到的 (大小, 修改时间)；`None` = 还没基准（文件当时读不到）
    stamp: Option<(u64, SystemTime)>,
    at: Instant,
    every: Duration,
}

impl Watch {
    /// 以当前磁盘状态为基准：启动时刚读过，不该立刻又算一次"变了"。
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut w = Self { path, stamp: None, at: Instant::now(), every: Duration::from_millis(250) };
        w.sync();
        w
    }

    /// 把基准对齐到文件的当前状态。我们自己刚写完配置时用它，免得把自己的写入
    /// 当成外部编辑再绕一圈回来。
    pub fn sync(&mut self) {
        self.stamp = self.stat_now();
    }

    fn stat_now(&self) -> Option<(u64, SystemTime)> {
        let meta = fs::metadata(self.path.as_ref()?).ok()?;
        Some((meta.len(), meta.modified().ok()?))
    }

    /// 文件被外部改过（或删掉又建回来）时返回 true。`now` 由调用方给，测试里注入假时间。
    pub fn changed(&mut self, now: Instant) -> bool {
        // 20ms 心跳那一档不该每秒 stat 五十次
        if now.duration_since(self.at) < self.every {
            return false;
        }
        self.at = now;
        match self.stat_now() {
            Some(stamp) if Some(stamp) == self.stamp => false,
            Some(stamp) => {
                self.stamp = Some(stamp);
                true
            }
            // 编辑器正在 rename：这一拍不认，基准留着，下一拍再看
            None => false,
        }
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
            color_done: Gradient::solid(0xAABBCC),
            bg_alpha: 120,
            zoom: 1.75,
            centiseconds: true,
            clock_seconds: false,
            time_pad: Pad::Full,
            tray_icon: IconMode::Gif,
            tray_gif: Some("~/pics/spin.gif".into()),
            pomo: Pomo { work: 1800, short_break: 600, long_break: 1200, rounds: 3, cycles: 2 },
            presets: vec![90, 600, 5400],
            on_finish: Some("loginctl lock-session".into()),
            timeout_text: Some("时间到".into()),
            notify: false,
            notify_text: Some("该起来了".into()),
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
        assert_eq!(Gradient::solid(PALETTES[0].running), d.color_running);
        assert_eq!(Gradient::solid(PALETTES[0].paused), d.color_paused);
        assert_eq!(Gradient::solid(PALETTES[0].done), d.color_done);
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

    /// 文字三色可写成渐变、背景不行；`text_effect` 认 Catime 的那几个值串。
    #[test]
    fn gradient_colors_and_text_effect_parse_and_roundtrip() {
        let cfg = Config::from_str(
            "color_running = #FF5E96_#56C6FF\ncolor_done = 50dc78\ntext_effect = neon\n",
        );
        assert_eq!(cfg.color_running, Gradient::parse("#FF5E96_#56C6FF").unwrap());
        assert_eq!(cfg.color_done, Gradient::solid(0x50DC78));
        assert_eq!(cfg.text_effect, Effect::Neon);
        let text = cfg.serialize();
        // 单色仍写成裸 16 进制，别把老配置重写出一堆噪声
        assert!(text.contains("color_done = 50dc78\n"), "单色写法变了: {text}");
        assert_eq!(Config::from_str(&text), cfg);
        // 背景不接受渐变；非法特效名回落 none
        let bad = Config::from_str("color_bg = #111111_#222222\ntext_effect = bloom\n");
        assert_eq!(bad.color_bg, Config::default().color_bg);
        assert_eq!(bad.text_effect, Effect::None);
    }

    /// 颜色的四种写法要能穿过**配置文件**这条路（不只是 `parse_color` 单测）：
    /// 配置文件里带空格与括号，而行解析先要按 `=` 拆键值。
    #[test]
    fn color_syntaxes_survive_the_line_parser() {
        let cfg = Config::from_str(
            "color_paused = #f57\ncolor_done = rgb(80, 220, 120)\ncolor_running = teal\n",
        );
        assert_eq!(cfg.color_paused, Gradient::solid(0xFF5577), "三位简写");
        assert_eq!(cfg.color_done, Gradient::solid(0x50DC78), "rgb() 带空格");
        assert_eq!(cfg.color_running, Gradient::solid(0x008080), "CSS 名");
        // 名与三元组可以混在一条渐变里
        let mixed = Config::from_str("color_running = gold_rgb(0, 128, 128)\n");
        assert_eq!(mixed.color_running, Gradient::parse("#FFD700_#008080").unwrap());
        // 认不出的写法整条作废，回落默认值而不是画半截颜色
        let bad = Config::from_str("color_running = notacolor\n");
        assert_eq!(bad.color_running, Config::default().color_running);
    }

    /// 每个特效的值串都要能写回配置再读回来，否则用户改了配置就丢。
    #[test]
    fn every_effect_name_survives_the_config_roundtrip() {
        for e in crate::effect::EFFECTS {
            let cfg = Config::from_str(&format!("text_effect = {}\n", e.name()));
            assert_eq!(cfg.text_effect, e, "{} 没走通", e.name());
            let again = Config::from_str(&cfg.serialize());
            assert_eq!(again.text_effect, e, "{} 序列化后丢了", e.name());
        }
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

    /// 布尔开关共用同一套值串。默认值也一起钉住：百分秒与"显示秒"里前者默认关
    /// （代价是心跳）、后者默认开（v0.5.0 以来的行为），"发通知"同样默认开。
    #[test]
    fn boolean_keys_parse_the_same_aliases() {
        let get = |cfg: &Config, key: &str| match key {
            "click_through" => cfg.click_through,
            "clock_12h" => cfg.clock_12h,
            "clock_seconds" => cfg.clock_seconds,
            "notify" => cfg.notify,
            _ => cfg.centiseconds,
        };
        for key in ["click_through", "clock_12h", "clock_seconds", "notify", "centiseconds"] {
            for on in ["true", "1", "yes", "on", "TRUE", "Yes"] {
                let cfg = Config::from_str(&format!("{key} = {on}\n"));
                assert!(get(&cfg, key), "{key} = {on} 应解析为 true");
            }
            for off in ["false", "0", "no", "off", "", "whatever"] {
                let cfg = Config::from_str(&format!("{key} = {off}\n"));
                assert!(!get(&cfg, key), "{key} = {off} 应解析为 false");
            }
        }
        assert!(!Config::default().click_through); // 默认整窗可拖动
        assert!(!Config::default().centiseconds);
        assert!(Config::default().clock_seconds);
        assert!(Config::default().notify);
    }

    /// `time_pad` 只认那三个值串，写错留默认档而不是把配置丢掉。
    #[test]
    fn time_pad_parses_and_rejects_unknown() {
        assert_eq!(Config::from_str("time_pad = zero\n").time_pad, Pad::Zero);
        assert_eq!(Config::from_str("time_pad = full\n").time_pad, Pad::Full);
        assert_eq!(Config::from_str("time_pad = half\n").time_pad, Pad::None);
        for p in [Pad::None, Pad::Zero, Pad::Full] {
            let cfg = Config { time_pad: p, ..Config::default() };
            assert_eq!(Config::from_str(&cfg.serialize()).time_pad, p);
        }
    }

    /// `text_font` 的三态要能原样往返：路径值一旦在写回时被规范化或丢掉，
    /// 用户指定的字体就再也找不回来了。
    #[test]
    fn text_font_round_trips_all_three_states() {
        assert_eq!(Config::from_str("text_font = off\n").text_font, TextFont::Off);
        assert_eq!(Config::from_str("text_font = none\n").text_font, TextFont::Off);
        assert_eq!(Config::from_str("text_font = auto\n").text_font, TextFont::Auto);
        // 缺键与空值都回到 auto，而不是把上一次的显式设置吃掉
        assert_eq!(Config::from_str("").text_font, TextFont::Auto);
        assert_eq!(Config::from_str("text_font =\n").text_font, TextFont::Auto);
        let path = "~/fonts/My Han.ttf";
        let cfg = Config::from_str(&format!("text_font = {path}\n"));
        assert_eq!(cfg.text_font, TextFont::Path(path.to_string()));
        // 写回再读一次，必须还是同一个值
        let again = Config::from_str(&cfg.serialize());
        assert_eq!(again.text_font, cfg.text_font);
        assert!(cfg.serialize().contains("text_font = ~/fonts/My Han.ttf\n"));
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

    /// 原子写：内容正确、能覆盖已有文件、不在配置目录留下临时文件。
    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("tinyticker-atomic-{}", std::process::id()));
        let path = dir.join("config.conf");
        let _ = fs::remove_dir_all(&dir);
        // 目录不存在也要能写（首次启动就是这个情况）
        write_atomic(&path, "a = 1\n").expect("首次写该成功");
        write_atomic(&path, "a = 2\n").expect("覆盖写该成功");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        let extras: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "config.conf")
            .collect();
        assert!(extras.is_empty(), "临时文件没清掉: {extras:?}");
        assert!(fs::remove_dir_all(&dir).is_ok());
    }

    /// 配置变更探测：靠 `(大小, mtime)` 认外部编辑，节流窗口内不重复 stat，
    /// 自己的写入可以用 `sync()` 抹掉。
    #[test]
    fn watch_notices_external_edits() {
        let dir = std::env::temp_dir().join(format!("tinyticker-watch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.conf");
        fs::write(&path, "duration = 60\n").unwrap();
        let mut w = Watch::new(Some(path.clone()));
        let t0 = Instant::now();
        assert!(!w.changed(t0), "刚建立基准不该立刻算变了");
        fs::write(&path, "duration = 1500\n").unwrap();
        assert!(!w.changed(t0), "节流窗口内不该去 stat");
        assert!(w.changed(t0 + Duration::from_millis(300)), "文件变了要认出来");
        assert!(!w.changed(t0 + Duration::from_millis(600)), "同一份内容不该报第二次");
        fs::write(&path, "duration = 900\n").unwrap();
        w.sync();
        assert!(!w.changed(t0 + Duration::from_millis(1000)), "自己的写入该被抹掉");
        // 文件正被编辑器 rename 走：不认、不 panic，基准留着等下一拍
        fs::remove_file(&path).unwrap();
        assert!(!w.changed(t0 + Duration::from_millis(1300)));
        // 没有合格路径（HOME / XDG_CONFIG_HOME 都不在）时整个探测静默
        assert!(!Watch::new(None).changed(t0 + Duration::from_millis(2000)));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// 路径优先级：显式指定 > XDG > HOME/.config，且 XDG 是相对路径时要能落到 HOME。
    #[test]
    fn config_dir_precedence() {
        let app = env!("CARGO_PKG_NAME");
        // 显式那档最优先，且原值照用（相对路径按当前工作目录解释，本进程不 chdir）
        assert_eq!(
            resolve_config_dir(Some("/tmp/tt-a"), Some("/xdg"), Some("/home")),
            Some(PathBuf::from("/tmp/tt-a"))
        );
        assert_eq!(
            resolve_config_dir(Some("relative/dir"), None, None),
            Some(PathBuf::from("relative/dir"))
        );
        assert_eq!(
            resolve_config_dir(None, Some("/xdg"), Some("/home")),
            Some(PathBuf::from(format!("/xdg/{app}")))
        );
        assert_eq!(
            resolve_config_dir(None, None, Some("/home")),
            Some(PathBuf::from(format!("/home/.config/{app}")))
        );
        // XDG 是相对路径时按老规矩不算数，回落到 HOME
        assert_eq!(
            resolve_config_dir(None, Some("relative/xdg"), Some("/home")),
            Some(PathBuf::from(format!("/home/.config/{app}")))
        );
        // 三档都没有：不读写文件，静默走默认值
        assert_eq!(resolve_config_dir(None, None, None), None);
        assert_eq!(resolve_config_dir(Some(""), Some(""), Some("")), None);
    }

    /// 自启条目：freedesktop 认的那几个键都要在，`Exec` 用真实路径。
    #[test]
    fn autostart_entry_has_the_keys_hosts_read() {
        let s = autostart_entry("/usr/bin/tinyticker");
        for key in [
            "[Desktop Entry]",
            "Type=Application",
            "Name=TinyTicker",
            "Exec=/usr/bin/tinyticker",
            "Terminal=false",
            "X-GNOME-Autostart-enabled=true",
        ] {
            assert!(s.contains(key), "条目里缺 {key}:\n{s}");
        }
        // 文件名与打包的 desktop 文件同名，用户想手动关掉就是删这个文件
        assert_eq!(AUTOSTART_FILE, "io.github.panzhifu.tinyticker.desktop");
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
