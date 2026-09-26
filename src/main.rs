//! TinyTicker —— 极简悬浮倒计时 / 秒表。
//!
//! 模块划分：
//! - `wl`      Wayland 客户端（直调 libwayland-client，layer-shell overlay 层）
//! - `x11`     X11 悬浮窗口（直调 libX11，仅 `x11` feature）
//! - `widget`  与后端无关的挂件核心（计时推进、帧内容与布局）
//! - `sys`     系统 API 声明层（dlopen + FFI，无第三方 crate）
//! - `clock`   本地时间读取（时钟挂件模式）
//! - `sysinfo` 系统状态采样（CPU / 内存 / 电量，读 /proc 与 /sys）
//! - `anim`    动图帧序列与播放器（GIF / PNG 两容器共用的形状）
//! - `gif`     最小 GIF89a 解码器
//! - `png`     最小 PNG/APNG 解码器（zlib inflate + 行滤波）
//! - `audio`   提示音：WAV 解码 + 合成 beep，ALSA 后台播放（不挡主循环）
//! - `textsrc` 外部文本源（别的进程写的文件，显示在状态行上）
//! - `ipc`     命令行意图解析 + 单实例转发（Unix 套接字；二次启动 = 下命令）
//! - `lang`    托盘文案的双语层（菜单/悬停提示/通知）
//! - `timer`   计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟）
//! - `render`  像素绘制与时间格式化
//! - `text`    字形层：8x8 点阵优先，点阵没有的码位由 dlopen 的 libfreetype 补
//! - `config`  配置持久化
//! - `parse`   时间解析（"1h30m" 相对 / "14:30" 绝对）
//! - `tray`    托盘图标、菜单与通知
//! - `wake`    self-pipe 唤醒器（空闲档 1s 心跳下命令也能即时结算）
//! - `font8x8` 内置 8x8 位图字体

mod anim;
mod audio;
mod clock;
mod config;
mod effect;
mod font8x8; // 8x8 位图字体（来源: https://github.com/dhepper/font8x8, MIT）
mod gif;
mod ipc;
mod lang;
mod parse;
mod png;
mod render;
mod sys;
mod sysinfo;
mod text;
mod textsrc;
mod timer;
mod tray;
mod wake;
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
  纯数字按秒（\"90\"），或数字+单位序列（\"25m\"、\"1h30m\"、\"1h 30m 10s\"、\"2d\"）
  也支持绝对时刻（\"14:30\" / \"14:30:45\"，或 Catime 写法 \"14 30t\" / \"14t\"，已过视为明天）
  不带时长时使用配置文件中的值（默认 60 秒）

选项:
  -s, --stopwatch  以秒表模式启动
  -c, --countdown  以倒计时模式启动（默认）
  -p, --pomodoro   以番茄钟模式启动
  -k, --clock      以时钟挂件模式启动
  -r, --running    启动后立即开始计时
  --hide, --show   隐藏 / 显示挂件（只对已经在跑的那个实例有效）
  --edit, --no-edit
                   开 / 关编辑态：整窗接收输入（拖与滚轮不必瞄准那两行字）、
                   到点只改读数不发通知也不执行结束命令、右键改为退出编辑态。
                   只存在于内存里，冷启动时同样只对已经在跑的那个实例有效
  --input          在挂件上打开输入行：键入 25m / 1h 30m 10s / 14:30 / 14 30t，
                   回车按预设语义开始、Esc 取消、失焦即收。绑进 DE 快捷键
                   就是\"全局输个倒计时\"（键盘只借这几秒，关行即还）
  -C, --config-dir 指定配置目录（或设 TINYTICKER_CONFIG_DIR）：配置与套接字都落在
                   那个目录里，所以可以同时跑两套互不相干的挂件
  --and <命令>     武装一条一次性结束命令（别名 --then）：下次计时结束执行一次就失效，
                   不写进配置——常驻的 on_finish 误留一次就每次结束都触发，这条不会
  --pause          暂停在跑的计时器（无时长语义的动作，同族还有下面两条）
  --toggle         运行中则暂停、否则开始——快捷键最爱绑的就是这一条
  --reset          把读数拨回起点（不开始）
  --centis / --no-centis
                   开 / 关百分之一秒。它是设定值不是翻面，而且**进配置**：
                   快捷键重复绑同一条，结果永远一样
  -h, --help       显示帮助
  -V, --version    显示版本

交互:
  托盘左键        开始 / 暂停（中键重置，右键完整菜单）
  托盘右键菜单    开始 / 暂停 / 重置 / 隐藏挂件 / 编辑态 / 输入时长 / 弹通知 / 开机自启 / 时长预设 / 模式切换 / 外观（透明度·配色·文字颜色·特效·图标（含数字/水位切换）·时间格式）/ 恢复默认设置 / 重置窗口位置 / 退出
  托盘图标滚轮    缩放悬浮窗
  悬浮窗          左键按住拖动，右键关闭；滚轮缩放；中键切编辑态（编辑态下右键只退出编辑态）；计时结束弹系统通知
  输入行          托盘「⌨ 输入时长」或 `tinyticker --input` 打开：键入时长回车开始，
                  Esc 取消，点到别处即收（键盘只在输入行开着的那几秒归挂件）。
                  同一行还有三种模式，入口在各自的子菜单里：「时长预设 ▸ ✏ 编辑预设」
                  键入 `90,1500,5400`、「🍅 番茄分段 ▸ ✏ 编辑分段」键入 `25m,5m,15m`
                  （序列为空也进得去，那是设序列的门）、「外观 ▸ 文字颜色 ▸ ✏ 输入颜色」
                  键入 `1a2b3c` / CSS 名 / `rgb()`——回车生效写盘，非法缀 `?` 不关行

单实例:
  已经在跑的时候再敲一次命令，不会再开一个窗口，而是把同样的参数交给那个实例：
      tinyticker 25m      → 已在跑的挂件立刻开始 25 分钟倒计时
      tinyticker -k       → 切到时钟挂件模式
      tinyticker 14:30 -r → 倒计时到 14:30 并开始
      tinyticker --toggle → 暂停/继续在跑的那个（还有 --pause / --reset）
      tinyticker 25m --and \"loginctl lock-session\"
                          → 这次跑完锁屏，仅此一次；不写配置，下次结束不再锁
  因此全局快捷键不必我们自己实现：在 KDE / GNOME / niri 的快捷键设置里
  绑一条 `tinyticker 25m` 就行。套接字在 $XDG_RUNTIME_DIR/tinyticker.sock
  （不可用时退到 /tmp/tinyticker-<uid>.sock），权限 0600，只有同一用户能连。
  加 `--config-dir <目录>` 时配置与套接字都搬进那个目录，于是可以同时跑两套互不相干的挂件。

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

/// 摘出 `--config-dir <路径>` / `--config-dir=<路径>` / `-C <路径>`，返回剩下的参数。
///
/// 它改的是"本进程去哪儿找配置与套接字"，所以**不进转发列表**：剩下的参数交给那个
/// 目录下的实例。指定之后套接字也落在那个目录里，两套配置因此互不相干。
fn take_config_dir(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut dir = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--config-dir=") {
            dir = Some(v.to_string());
        } else if a == "--config-dir" || a == "-C" {
            dir = it.next().cloned();
            if dir.is_none() {
                eprintln!("--config-dir 后面要跟一个路径");
                std::process::exit(2);
            }
        } else {
            rest.push(a.clone());
        }
    }
    (dir, rest)
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
    // 配置目录必须在任何一次路径解析之前定下来（`config_path` 与 `socket_path` 都读它）
    let (config_dir, args) = take_config_dir(&args);
    if let Some(dir) = config_dir {
        config::set_config_dir(&dir);
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
    // 编辑态也一样：新实例总该从普通态开始，一上手就"右键关不掉"只会让人以为卡住了
    if intent.edit.is_some() {
        eprintln!("提示：--edit / --no-edit 只对已经在跑的实例有效，本次启动是普通态。");
    }

    // 趁还没进事件循环先把字形后端准备好：状态行里点阵覆盖不到的码位（中文、
    // Latin-1、符号）要靠 dlopen 出来的 libfreetype 补。开不成只警告不退出——
    // 那时非 ASCII 仍旧留空位，只是要把话说清楚，别让人以为已经支持中文了。
    if let Some(warning) = text::init(&config.text_font) {
        eprintln!("{warning}");
    }
    // 状态行字号与托盘文案的语言也在进循环前落定（之后只有热加载/菜单能改）
    text::set_px_per_cell(config.status_font_px);
    lang::set(config.language);

    // 三条单向通道汇进同一个主循环：
    // - cmd_tx:    托盘菜单 / 后续实例的命令行 → 主窗口
    // - handle_tx: 托盘线程就绪后把 TrayHandle 发给主窗口（发通知用）
    // - wake:      发完命令摸一下管道——空闲档心跳睡到 1s，命令不能等下一拍
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (handle_tx, handle_rx) = mpsc::channel();
    let (wake_tx, wake_rx) = wake::pair();
    // --input 冷启动也开输入行：转发那条路已把参数翻成 InputTime，这里补上
    // "自己就是第一个实例"的情形——命令躺在通道里，事件循环第一拍就会消费
    if intent.input {
        let _ = cmd_tx.send(Command::InputTime);
    }
    // 先起监听再起托盘：绑定失败只意味着本实例不接电话，照常工作。
    ipc::serve(cmd_tx.clone(), wake_tx.clone());
    tray::spawn(cmd_tx, handle_tx, &config, wake_tx);

    run_backend(&cmd_rx, &handle_rx, config, autostart, &wake_rx)
}

/// 按会话挑后端：Wayland 优先走自研 layer-shell —— xdg-shell 不给客户端任何指定
/// 层级的途径，普通窗口必然被全屏窗口盖住，overlay 层是唯一办法。
fn run_backend(
    cmd_rx: &Receiver<Command>,
    handle_rx: &Receiver<TrayHandle>,
    config: Config,
    autostart: bool,
    wake_rx: &wake::WakeReader,
) -> Result<(), Box<dyn std::error::Error>> {
    // 空值算"没有"：不少启动器会留一个 `WAYLAND_DISPLAY=`，按 is_some 判就会去走
    // Wayland 分支然后连不上——那条路本该退回 X11
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland {
        return wl::run(cmd_rx, handle_rx, config, autostart, wake_rx);
    }
    #[cfg(feature = "x11")]
    return x11::run(cmd_rx, handle_rx, config, autostart, wake_rx);
    #[cfg(not(feature = "x11"))]
    Err("此构建只含 Wayland 后端，而当前会话没有 WAYLAND_DISPLAY".into())
}

#[cfg(test)]
mod tests {
    use super::take_config_dir;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// `--config-dir` 三种写法都要被摘干净：它不该出现在转发给另一个实例的参数里。
    #[test]
    fn config_dir_is_taken_out_of_the_args() {
        let (dir, rest) = take_config_dir(&v(&["--config-dir", "/tmp/x", "25m"]));
        assert_eq!(dir.as_deref(), Some("/tmp/x"));
        assert_eq!(rest, v(&["25m"]), "路径本身不算时长参数");
        let (dir, rest) = take_config_dir(&v(&["--config-dir=/tmp/y", "-r"]));
        assert_eq!(dir.as_deref(), Some("/tmp/y"));
        assert_eq!(rest, v(&["-r"]));
        assert_eq!(
            take_config_dir(&v(&["-C", "/tmp/z"])).0.as_deref(),
            Some("/tmp/z")
        );
        let (dir, rest) = take_config_dir(&v(&["25m", "-s"]));
        assert_eq!(dir, None, "没给就不该造出一个目录");
        assert_eq!(rest, v(&["25m", "-s"]));
    }
}
