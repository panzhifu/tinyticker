//! TinyTicker —— 极简悬浮倒计时 / 秒表。
//!
//! 模块划分：
//! - `app`     悬浮窗口（winit 事件循环 + 绘制调度）
//! - `surface` 呈现后端抽象（Wayland ARGB shm / X11 softbuffer）
//! - `wayland` Wayland ARGB 呈现（支持透明背景）
//! - `clock`   本地时间读取（时钟挂件模式）
//! - `timer`   计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟）
//! - `render`  像素绘制与时间格式化
//! - `config`  配置持久化
//! - `parse`   时间解析（"1h30m" 相对 / "14:30" 绝对）
//! - `tray`    托盘菜单与通知
//! - `font8x8` 内置 8x8 位图字体

mod app;
mod clock;
mod config;
mod font8x8; // 8x8 位图字体（来源: https://github.com/dhepper/font8x8, MIT）
mod parse;
mod render;
mod surface;
mod timer;
mod tray;
mod wayland;

use std::sync::mpsc;

use app::App;
use config::Config;
use timer::Mode;

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
  托盘右键菜单    开始 / 暂停 / 重置 / 时长预设 / 模式切换（倒计时/秒表/番茄钟/时钟）/ 退出
  悬浮窗          左键按住拖动，右键关闭；滚轮缩放；计时结束弹系统通知

配置文件: ~/.config/tinyticker/config.conf（时长 / 模式 / 颜色 / 透明度 / 缩放 / 番茄钟 / 结束命令 / 窗口位置）\
";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::load();

    // 命令行参数：覆盖配置中的时长与模式（退出时随状态写回）
    let mut duration: Option<u32> = None;
    let mut mode: Option<Mode> = None;
    let mut autostart = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-s" | "--stopwatch" => mode = Some(Mode::Stopwatch),
            "-c" | "--countdown" => mode = Some(Mode::Countdown),
            "-p" | "--pomodoro" => mode = Some(Mode::Pomodoro),
            "-k" | "--clock" => mode = Some(Mode::Clock),
            "-r" | "--running" => autostart = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("tinyticker {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                if let Some(secs) = parse::parse_duration(other) {
                    duration = Some(secs);
                } else if let Some((h, m, s)) = parse::parse_absolute(other) {
                    // 绝对时刻 → 距现在的秒数（已过则视为明天同一时刻）
                    let (nh, nm, ns) = clock::now_hms();
                    let now = nh * 3600 + nm * 60 + ns;
                    let target = h * 3600 + m * 60 + s;
                    let mut diff = (target + 86_400 - now) % 86_400;
                    if diff == 0 {
                        diff = 86_400;
                    }
                    duration = Some(diff);
                } else {
                    eprintln!("无法识别的参数: {other}\n");
                    print!("{USAGE}");
                    std::process::exit(2);
                }
            }
        }
    }
    if let Some(secs) = duration {
        config.duration_secs = secs;
    }
    if let Some(mode) = mode {
        config.mode = mode;
    }

    // 托盘与窗口之间两条单向通道：
    // - cmd_tx:    托盘菜单 → 主窗口
    // - handle_tx: 托盘线程就绪后把 TrayHandle 发给主窗口（发通知用）
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (handle_tx, handle_rx) = mpsc::channel();
    tray::spawn(cmd_tx, handle_tx);

    // 主线程运行悬浮窗口（winit 要求事件循环在主线程）
    let event_loop = winit::event_loop::EventLoop::with_user_event().build()?;
    let mut app = App::new(cmd_rx, handle_rx, config, autostart);
    event_loop.run_app(&mut app)?;

    Ok(())
}
