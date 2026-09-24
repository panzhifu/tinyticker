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
//! - `ipc`     命令行意图解析 + 单实例转发（Unix 套接字；二次启动 = 下命令）
//! - `timer`   计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟）
//! - `render`  像素绘制与时间格式化
//! - `text`    字形层：8x8 点阵优先，点阵没有的码位由 dlopen 的 libfreetype 补
//! - `config`  配置持久化
//! - `parse`   时间解析（"1h30m" 相对 / "14:30" 绝对）
//! - `tray`    托盘图标、菜单与通知
//! - `font8x8` 内置 8x8 位图字体

mod clock;
mod config;
mod effect;
mod font8x8; // 8x8 位图字体（来源: https://github.com/dhepper/font8x8, MIT）
mod gif;
mod ipc;
mod parse;
mod render;
mod sys;
mod sysinfo;
mod text;
mod textsrc;
mod timer;
mod tray;
mod widget;
mod wl;

#[cfg(feature = "x11")]
mod x11;

use std::sync::mpsc::{self, Receiver};

use config::Config;
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
  --hide, --show   隐藏 / 显示挂件（只对已经在跑的那个实例有效）
  -h, --help       显示帮助
  -V, --version    显示版本

交互:
  托盘左键        开始 / 暂停（中键重置，右键完整菜单）
  托盘右键菜单    开始 / 暂停 / 重置 / 隐藏挂件 / 时长预设 / 模式切换（倒计时/秒表/番茄钟/时钟）/ 外观（透明度·配色·特效·图标·时间格式）/ 退出
  托盘图标滚轮    缩放悬浮窗
  悬浮窗          左键按住拖动，右键关闭；滚轮缩放；计时结束弹系统通知

单实例:
  已经在跑的时候再敲一次命令，不会再开一个窗口，而是把同样的参数交给那个实例：
      tinyticker 25m      → 已在跑的挂件立刻开始 25 分钟倒计时
      tinyticker -k       → 切到时钟挂件模式
      tinyticker 14:30 -r → 倒计时到 14:30 并开始
  因此全局快捷键不必我们自己实现：在 KDE / GNOME / niri 的快捷键设置里
  绑一条 `tinyticker 25m` 就行。套接字在 $XDG_RUNTIME_DIR/tinyticker.sock
  （不可用时退到 /tmp/tinyticker-<uid>.sock），权限 0600，只有同一用户能连。

配置（时长 / 模式 / 颜色 / 透明度 / 缩放 / 番茄钟 / 显示精度与补零 / 结束命令 / 窗口位置）存于:
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
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 帮助与版本永远本地处理：二次调用 `-h` 不该把"显示帮助"这件事发给已经在跑的实例。
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("tinyticker {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // 先解析再转发：垃圾参数在这里就被拦下，不会上到套接字上让收端静默丢掉。
    let intent = match ipc::Intent::parse(&args) {
        Ok(intent) => intent,
        Err(msg) => {
            eprintln!("{msg}\n");
            print_usage();
            std::process::exit(2);
        }
    };

    // 已经在跑就只下命令，不再开第二个窗口。用户在 DE 快捷键里绑 `tinyticker 25m`
    // 靠的就是这条路——我们自己不需要实现任何快捷键协议。
    if ipc::forward(&args) {
        return Ok(());
    }

    // 到这里才是"本进程当那个唯一的实例"：命令行覆盖配置里的时长与模式（退出时随状态写回）
    let mut config = Config::load();
    let autostart = intent.apply(&mut config);
    // --hide / --show 是"给在跑的那个下命令"的开关；新起的实例一律可见——
    // 起一个看不见的挂件只会让人以为程序没起来。
    if intent.hidden.is_some() {
        eprintln!("提示：--hide / --show 只对已经在跑的实例有效，本次启动仍然可见。");
    }

    // 趁还没进事件循环先把字形后端准备好：状态行里点阵覆盖不到的码位（中文、
    // Latin-1、符号）要靠 dlopen 出来的 libfreetype 补。开不成只警告不退出——
    // 那时非 ASCII 仍旧留空位，只是要把话说清楚，别让人以为已经支持中文了。
    if let Some(warning) = text::init(&config.text_font) {
        eprintln!("{warning}");
    }

    // 三条单向通道汇进同一个主循环：
    // - cmd_tx:    托盘菜单 / 后续实例的命令行 → 主窗口
    // - handle_tx: 托盘线程就绪后把 TrayHandle 发给主窗口（发通知用）
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (handle_tx, handle_rx) = mpsc::channel();
    // 先起监听再起托盘：绑定失败只意味着本实例不接电话，照常工作。
    ipc::serve(cmd_tx.clone());
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
    // 空值算"没有"：不少启动器会留一个 `WAYLAND_DISPLAY=`，按 is_some 判就会去走
    // Wayland 分支然后连不上——那条路本该退回 X11
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland {
        return wl::run(cmd_rx, handle_rx, config, autostart);
    }
    #[cfg(feature = "x11")]
    return x11::run(cmd_rx, handle_rx, config, autostart);
    #[cfg(not(feature = "x11"))]
    Err("此构建只含 Wayland 后端，而当前会话没有 WAYLAND_DISPLAY".into())
}
