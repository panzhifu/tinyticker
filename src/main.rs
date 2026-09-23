//! TinyTicker —— 极简悬浮倒计时 / 秒表。
//!
//! 模块划分：
//! - `wl`      Wayland 客户端（直调 libwayland-client，layer-shell overlay 层）
//! - `x11`     X11 悬浮窗口（直调 libX11，仅 `x11` feature）
//! - `widget`  与后端无关的挂件核心（计时推进、帧内容与布局）
//! - `sys`     系统 API 声明层（dlopen + FFI，无第三方 crate）
//! - `clock`   本地时间读取（时钟挂件模式）
//! - `sysinfo` 系统状态采样（CPU / 内存 / 电量，读 /proc 与 /sys）
//! - `gif`     最小 GIF89a 解码器（托盘动图图标用）
//! - `textsrc` 外部文本源（别的进程写的文件，显示在状态行上）
//! - `timer`   计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟）
//! - `render`  像素绘制与时间格式化
//! - `config`  配置持久化
//! - `parse`   时间解析（"1h30m" 相对 / "14:30" 绝对）
//! - `tray`    托盘图标、菜单与通知
//! - `font8x8` 内置 8x8 位图字体

mod clock;
mod config;
mod font8x8; // 8x8 位图字体（来源: https://github.com/dhepper/font8x8, MIT）
mod gif;
mod parse;
mod render;
mod sys;
mod sysinfo;
mod textsrc;
mod timer;
mod tray;
mod widget;
mod wl;

#[cfg(feature = "x11")]
mod x11;

use std::sync::mpsc::{self, Receiver};

use config::Config;
use timer::Mode;
use tray::{Command, TrayHandle};

const USAGE: &str = "\
TinyTicker —— 极简悬浮倒计时 / 秒表

用法: tinyticker [选项] [时长]

时长:
  纯数字按秒（\"90\"），或数字+单位序列（\"25m\"、\"1h30m\"、\"1h 30m 10s\"）
  也支持绝对时刻（\"14:30\" / \"14:30:45\"，已过则视为明天）
  不带时长时使用配置文件中的值（默认 60 秒）

选项:
  -s, --stopwatch  以秒表模式启动
  -c, --countdown  以倒计时模式启动（默认）
  -p, --pomodoro   以番茄钟模式启动
  -k, --clock      以时钟挂件模式启动
  -r, --running    启动后立即开始计时
  -h, --help       显示帮助
  -V, --version    显示版本

交互:
  托盘左键        开始 / 暂停（中键重置，右键完整菜单）
  托盘右键菜单    开始 / 暂停 / 重置 / 时长预设 / 模式切换（倒计时/秒表/番茄钟/时钟）/ 外观 / 退出
  托盘图标滚轮    缩放悬浮窗
  悬浮窗          左键按住拖动，右键关闭；滚轮缩放；计时结束弹系统通知

配置（时长 / 模式 / 颜色 / 透明度 / 缩放 / 番茄钟 / 结束命令 / 窗口位置）存于:
";

/// 打印帮助。配置文件位置按平台惯例不同，因此路径不写死在 `USAGE` 里。
fn print_usage() {
    print!("{USAGE}");
    match config::config_path() {
        Some(path) => println!("  {}", path.display()),
        None => println!("  （环境里没有可用的 HOME / XDG_CONFIG_HOME，本次运行不读写配置文件）"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::load();
    let mut autostart = false;

    // 命令行参数：直接覆盖配置里的时长与模式（退出时随状态写回）
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-s" | "--stopwatch" => config.mode = Mode::Stopwatch,
            "-c" | "--countdown" => config.mode = Mode::Countdown,
            "-p" | "--pomodoro" => config.mode = Mode::Pomodoro,
            "-k" | "--clock" => config.mode = Mode::Clock,
            "-r" | "--running" => autostart = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("tinyticker {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                // 时长：相对写法（"25m"）或绝对时刻（"14:30"，已过算明天同一时刻）
                let secs = parse::parse_duration(other).or_else(|| {
                    parse::parse_absolute(other).map(|t| parse::secs_until(t, clock::now_hms()))
                });
                match secs {
                    Some(secs) => config.duration_secs = secs,
                    None => {
                        eprintln!("无法识别的参数: {other}\n");
                        print_usage();
                        std::process::exit(2);
                    }
                }
            }
        }
    }

    // 托盘与窗口之间两条单向通道：
    // - cmd_tx:    托盘菜单 → 主窗口
    // - handle_tx: 托盘线程就绪后把 TrayHandle 发给主窗口（发通知用）
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (handle_tx, handle_rx) = mpsc::channel();
    tray::spawn(cmd_tx, handle_tx, &config);

    run_backend(&cmd_rx, &handle_rx, config, autostart)
}

/// 按会话挑后端：Wayland 优先走自研 layer-shell —— xdg-shell 不给客户端任何指定
/// 层级的途径，普通窗口必然被全屏窗口盖住，overlay 层是唯一办法。
fn run_backend(
    cmd_rx: &Receiver<Command>,
    handle_rx: &Receiver<TrayHandle>,
    config: Config,
    autostart: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return wl::run(cmd_rx, handle_rx, config, autostart);
    }
    #[cfg(feature = "x11")]
    return x11::run(cmd_rx, handle_rx, config, autostart);
    #[cfg(not(feature = "x11"))]
    Err("此构建只含 Wayland 后端，而当前会话没有 WAYLAND_DISPLAY".into())
}
