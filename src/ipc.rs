//! 命令行意图与单实例转发。
//!
//! # 为什么要有这个模块
//!
//! 二次启动 `tinyticker 25m` 不该再开一个窗口——它应该是"给已经在跑的那个下命令"。
//! Catime 就是这么做的（它的 CLI 把命令经 `WM_COPYDATA` 转给已存在的窗口）。
//!
//! 这一条顺带解决掉 GAP.md 里评为 L 的**全局快捷键**：Wayland 没有统一的快捷键协议，
//! 我们不去抢这个活；只要 `tinyticker 25m` 是"下命令"而不是"起新进程"，用户就能在
//! KDE / GNOME / niri 自己的快捷键设置里绑一条命令了事。所以我们只需要一个套接字，
//! 不需要任何协议扩展。
//!
//! # 只有一条解析路径
//!
//! [`Intent::parse`] 是唯一理解命令行的地方。首次启动时它把结果写进 [`Config`]；
//! 二次启动时**原始参数**原样送过套接字、由收端再跑一遍同一个 parser——绝对时刻因此
//! 按收端的钟算，不可能出现两份语义。
//!
//! # 权限
//!
//! 套接字优先落在 `$XDG_RUNTIME_DIR`（该目录本身按用户隔离，0700），退回
//! `/tmp/tinyticker-<uid>.sock`；两条路都把套接字文件 chmod 成 0600，所以只有同一
//! 用户能连。

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::Sender;

use crate::clock;
use crate::config::Config;
use crate::parse;
use crate::sys;
use crate::timer::Mode;
use crate::tray::Command;
use crate::wake::WakeSender;

/// 参数之间的分隔符。不能用空格：`"1h 30m 10s"` 本身就是一个参数。
const SEP: char = '\u{1f}';
/// 一行命令的上限。正常调用远不到这个数，超出的一律丢弃。
const MAX_LINE: usize = 4096;
/// 收端回给发端的一个字节：确认命令已经投递进主循环的通道。
const ACK: u8 = b'K';
/// 发端等 ACK 的上限。
const ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// 收端等一行的上限。真实客户端连上就立刻把一行写完，所以这个值可以很短；
/// 只有一条接受循环，一个连上却不说话的客户端会把后面的转发挡在这里这么久。
const ACCEPT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// 一次命令行想干什么。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Intent {
    /// `-s` / `-c` / `-p` / `-k` 指定的模式。
    pub mode: Option<Mode>,
    /// 时长参数（相对写法或绝对时刻换算出的秒数）。
    pub duration: Option<u32>,
    /// `-r`：立即开始。
    pub autostart: bool,
    /// `--hide` / `--show`：把挂件的可见性设成给定值。
    /// 只对**已在跑的那个实例**有意义，冷启动时新实例总是可见的（见 `main.rs` 的提示）。
    pub hidden: Option<bool>,
    /// `--and <命令>`：武装一条一次性结束命令。只走转发这条路，因此**永不落盘**。
    pub and_then: Option<String>,
    /// `--edit` / `--no-edit`：把编辑态设成给定值。同样**只在内存里**，冷启动不生效
    /// （见 `main.rs` 的提示）——留下一个"右键关不掉"的挂件是灾难。
    pub edit: Option<bool>,
    /// `--centis` / `--no-centis`：把百分秒设成给定值。它**进配置**（与模式同族），
    /// 所以下一次冷启动也该记得——设定值而非翻面，快捷键重复绑同一条命令结果不变。
    pub centiseconds: Option<bool>,
    /// `--pause` / `--toggle` / `--reset`：无时长语义的动作，排在时长之后执行。
    pub pause: bool,
    pub toggle: bool,
    pub reset: bool,
    /// `--input`：打开输入行（在挂件上键入时长）。与编辑态同族——界面动作
    /// 不进配置；冷启动时主循环自己补发这条命令（见 `main.rs`），所以
    /// 转发与冷启动两条路行为一致。
    pub input: bool,
}

impl Intent {
    /// 解析命令行。返回 `Err(说明)` 表示有认不出的参数——发送端就地把错误抛给用户，
    /// 这样垃圾参数根本不会上到套接字上。
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut intent = Intent::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-s" | "--stopwatch" => intent.mode = Some(Mode::Stopwatch),
                "-c" | "--countdown" => intent.mode = Some(Mode::Countdown),
                "-p" | "--pomodoro" => intent.mode = Some(Mode::Pomodoro),
                "-k" | "--clock" => intent.mode = Some(Mode::Clock),
                "-r" | "--running" => intent.autostart = true,
                "--hide" => intent.hidden = Some(true),
                "--show" => intent.hidden = Some(false),
                "--edit" => intent.edit = Some(true),
                "--no-edit" => intent.edit = Some(false),
                "--centis" => intent.centiseconds = Some(true),
                "--no-centis" => intent.centiseconds = Some(false),
                "--pause" => intent.pause = true,
                "--toggle" => intent.toggle = true,
                "--reset" => intent.reset = true,
                "--input" => intent.input = true,
                // 带值参数：`--and 锁屏.cmd` / `--and=...` 两种写法
                "--and" | "--then" => {
                    intent.and_then = Some(it.next().cloned().ok_or("--and 后面要跟一条命令")?);
                }
                other => {
                    if let Some(v) = other.strip_prefix("--and=") {
                        intent.and_then = Some(v.to_string());
                        continue;
                    }
                    // 时长：相对写法（"25m"）或绝对时刻（"14:30"，已过算明天同一时刻）
                    let secs = parse::parse_duration(other).or_else(|| {
                        parse::parse_absolute(other).map(|t| parse::secs_until(t, clock::now_hms()))
                    });
                    match secs {
                        Some(secs) => intent.duration = Some(secs),
                        None => return Err(format!("无法识别的参数: {other}")),
                    }
                }
            }
        }
        Ok(intent)
    }

    /// 首次启动：把意图落到配置上，返回是否要立即开始。
    /// 没给的项保持配置里的值不动——命令行是覆盖，不是重建。
    pub fn apply(&self, config: &mut Config) -> bool {
        if let Some(mode) = self.mode {
            config.mode = mode;
        }
        if let Some(secs) = self.duration {
            config.duration_secs = secs;
        }
        if let Some(on) = self.centiseconds {
            config.centiseconds = on;
        }
        self.autostart
    }

    /// 收到转发的命令行后翻成命令。顺序要紧：先换模式、再定时长、最后才开始，
    /// 于是 `tinyticker -p` 落到番茄钟上，而 `tinyticker 25m` 是一条会自己开始的倒计时。
    pub fn commands(&self) -> Vec<Command> {
        let mut out = Vec::new();
        // 可见性排在最前：`--hide` 之后不管换成什么模式，结果都是"藏着跑"
        if let Some(hidden) = self.hidden {
            out.push(Command::SetHidden(hidden));
        }
        // 编辑态紧随其后：它只决定输入区域和右键含义，不碰读数
        if let Some(edit) = self.edit {
            out.push(Command::SetEdit(edit));
        }
        // 输入行再往后：先摆好界面（编辑态 / 输入行），再谈计时
        if self.input {
            out.push(Command::InputTime);
        }
        if let Some(mode) = self.mode {
            out.push(Command::SetMode(mode));
        }
        if let Some(secs) = self.duration {
            out.push(Command::Preset(secs));
        } else if self.autostart {
            // 只给 `-r` 时是"把当前这个跑起来"，而不是重新按一个预设
            out.push(Command::Start);
        }
        // 百分秒是设定值：排在时长之后，这样 `-p 25m --centis` 两条都落得住
        if let Some(on) = self.centiseconds {
            out.push(Command::SetCentiseconds(on));
        }
        // 无时长语义的动作排在最后：它们作于"刚定下来的那个计时器"上。
        // 同时给好几个时取最后一个——参数写重了是用户的笔误，不是我们的歧义。
        if self.pause {
            out.push(Command::Pause);
        }
        if self.toggle {
            out.push(Command::Toggle);
        }
        if self.reset {
            out.push(Command::Reset);
        }
        // 一次性结束命令放最后：它武装的是"这一次跑完干什么"，得等计时器先定下来
        if let Some(cmd) = &self.and_then {
            out.push(Command::ArmFinish(cmd.clone()));
        }
        out
    }
}

/// 把参数拼成一行。
fn encode(args: &[String]) -> String {
    let joined: Vec<String> = args.iter().map(|a| a.replace(SEP, " ")).collect();
    format!("{}\n", joined.join(&SEP.to_string()))
}

/// 从一行还原参数。空段（尾随换行留下的）丢掉。
fn decode(line: &str) -> Vec<String> {
    line.trim_end_matches(['\n', '\r'])
        .split(SEP)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 本用户的套接字完整路径。`$XDG_RUNTIME_DIR` 可用时天然按用户隔离；
/// 退回 temp 目录时必须带上 uid，否则多用户机器上会互相踢。
#[must_use]
pub fn socket_path() -> std::path::PathBuf {
    // 显式指定过配置目录 = 用户要的是"另一套互不相干的实例"，套接字也跟着搬进那个目录；
    // 否则第二个实例会把命令转给第一个，两个配置目录就分不开了
    if let Some(dir) = crate::config::config_dir_override() {
        return dir.join("tinyticker.sock");
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir());
    match runtime {
        Some(dir) => dir.join("tinyticker.sock"),
        // temp_dir() 永远给得出一个值（没有就是 "/tmp"），所以这里不需要 Option
        None => std::env::temp_dir().join(format!("tinyticker-{}.sock", sys::getuid())),
    }
}

/// 尝试把命令行转交给已经在跑的实例。返回 `true` 表示交出去了，调用方应当直接退出。
///
/// 交出去时会在 stderr 留一行说明：静默退出会让人以为"启动失败了"或"我拖的是
/// 自己刚起的那个窗口"——实际上他看到的是早就在跑的那一个。
pub fn forward(args: &[String]) -> bool {
    let path = socket_path();
    if !forward_to(&path, args) {
        return false;
    }
    eprintln!(
        "已把这条命令交给正在运行的实例 {}（本进程不另开窗口）",
        path.display()
    );
    true
}

/// [`forward`] 的可指定路径版本，测试用。
fn forward_to(path: &Path, args: &[String]) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    // 读必须带超时：对端不回 ACK 时（比如它是不懂这个协议的老进程），没有超时的
    // `read` 会把这次调用永久挂住。超时按"没交出去"处理，调用方退回自己起一个实例。
    let _ = stream.set_read_timeout(Some(ACK_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ACK_TIMEOUT));
    let payload = encode(args);
    if payload.len() > MAX_LINE
        || stream.write_all(payload.as_bytes()).is_err()
        || stream.flush().is_err()
    {
        return false;
    }
    // 等一个回音再退出：没有它就分不清"对方收下了"和"连上了但对方早死了"。
    let mut ack = [0u8; 1];
    matches!(stream.read(&mut ack), Ok(1)) && ack[0] == ACK
}

/// 起一个线程接后续实例的电话。命令经既有的 `cmd_tx` 通道进主循环——两个后端都在
/// 自己的 poll 里 `try_recv` 排空它，所以后端一行都不用改；排空之外的时间主循环可能
/// 睡在最长 1 秒的空闲心跳上，所以每投递一条都要摸一下唤醒管道。
///
/// 绑定失败只意味着本实例不接电话，照常工作，因此不向上抛错。
pub fn serve(cmd_tx: Sender<Command>, wake: WakeSender) {
    serve_at(&socket_path(), cmd_tx, wake);
}

/// [`serve`] 的可指定路径版本，测试用。
fn serve_at(path: &Path, cmd_tx: Sender<Command>, wake: WakeSender) {
    let Ok(listener) = bind(path) else {
        return;
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // 只有一条接受循环，所以一个连上却不说话的客户端就能把后面所有转发永久
            // 卡死。给它一个读超时，哑巴连接会被丢掉而不是拖死整条队列。
            match handle_conn(stream, &cmd_tx, &wake) {
                Conn::Done => {}
                Conn::ChannelGone => return,
            }
        }
    });
}

enum Conn {
    Done,
    ChannelGone,
}

/// 处理一次连接。**任何**走到终点的分支都要回一个 ACK——发端正阻塞等它，
/// 不回就等于把对方挂死（无参数的 `tinyticker` 只发一个换行，正好落进"没有命令"
/// 那条分支，这就是原来那个挂死的成因）。
fn handle_conn(mut stream: UnixStream, cmd_tx: &Sender<Command>, wake: &WakeSender) -> Conn {
    let _ = stream.set_read_timeout(Some(ACCEPT_TIMEOUT));
    if let Some(text) = read_line(&mut stream) {
        let mut sent = false;
        for cmd in Intent::parse(&decode(&text))
            .map(|i| i.commands())
            .unwrap_or_default()
        {
            if cmd_tx.send(cmd).is_err() {
                return Conn::ChannelGone;
            }
            sent = true;
        }
        // 投递过东西才唤醒：空参数那条命令没有，不必白摸一次管道
        if sent {
            wake.wake();
        }
    }
    let _ = stream.write_all(&[ACK]);
    let _ = stream.flush();
    Conn::Done
}

/// 绑定，并在"文件在但没人听"时接管它。
fn bind(path: &Path) -> std::io::Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(listener) => {
            chmod_owner_only(path);
            Ok(listener)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // 上一个实例被 kill 掉时来不及 unlink，留下一个没人接的死文件。
            // 先确认确实没人听才敢删——否则会踢掉活着的实例。
            if UnixStream::connect(path).is_ok() {
                return Err(e);
            }
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path)?;
            chmod_owner_only(path);
            Ok(listener)
        }
        Err(e) => Err(e),
    }
}

/// 0600：只有同一用户能连。`bind` 的权限位受 umask 影响，所以显式盖一次。
fn chmod_owner_only(path: &Path) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// 读到换行为止，返回不含换行的那一段。空输入或超长都返回 `None`。
fn read_line(stream: &mut UnixStream) -> Option<String> {
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    while buf.len() < MAX_LINE {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => buf.push(byte[0]),
            Err(_) => return None,
        }
    }
    if buf.is_empty() || buf.len() >= MAX_LINE {
        return None;
    }
    String::from_utf8(buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    /// 测试里造一对唤醒管道：`serve_at` 要发送端，接收端得活着握住（握丢就 EOF）。
    fn wake_pair() -> (WakeSender, crate::wake::WakeReader) {
        crate::wake::pair()
    }

    #[test]
    fn flags_map_to_modes() {
        for (flag, mode) in [
            ("-s", Mode::Stopwatch),
            ("-c", Mode::Countdown),
            ("-p", Mode::Pomodoro),
            ("-k", Mode::Clock),
        ] {
            let intent = Intent::parse(&[flag.to_string()]).unwrap();
            assert_eq!(intent.mode, Some(mode), "{flag} 没映射到模式");
        }
        assert_eq!(Intent::parse(&[]).unwrap(), Intent::default());
    }

    /// 顺序必须是 模式 → 时长，否则 `-p 600` 会先起一个倒计时再换模式。
    #[test]
    fn command_order_is_mode_then_duration_then_start() {
        let intent = Intent::parse(&["-p".into(), "600".into(), "-r".into()]).unwrap();
        assert_eq!(
            intent.commands(),
            vec![Command::SetMode(Mode::Pomodoro), Command::Preset(600)]
        );
        // 只给 -r：是"开始当前这个"，不是按一个预设
        assert_eq!(
            Intent::parse(&["-r".into()]).unwrap().commands(),
            vec![Command::Start]
        );
        assert!(Intent::parse(&[]).unwrap().commands().is_empty());
    }

    /// `--and` 带一条命令，两种写法都认；缺值要报错而不是把下一条参数吃掉当默认。
    #[test]
    fn and_arms_a_one_shot_finish_command() {
        let i =
            Intent::parse(&["25m".into(), "--and".into(), "loginctl lock-session".into()]).unwrap();
        assert_eq!(i.duration, Some(1500));
        assert_eq!(i.and_then.as_deref(), Some("loginctl lock-session"));
        assert_eq!(
            i.commands(),
            vec![
                Command::Preset(1500),
                Command::ArmFinish("loginctl lock-session".into())
            ],
            "武装命令排在时长之后"
        );
        // 等号写法：整条命令里带空格也不用拆成两个参数
        assert_eq!(
            Intent::parse(&["--and=notify-send 好了".into()])
                .unwrap()
                .and_then
                .as_deref(),
            Some("notify-send 好了")
        );
        // 别名
        assert!(
            Intent::parse(&["--then".into(), "x".into()])
                .unwrap()
                .and_then
                .is_some()
        );
        // 缺值：报错，不能静默把后面的时长参数吞掉
        assert!(Intent::parse(&["--and".into()]).is_err());
        assert!(
            Intent::parse(&["--and".into(), "25m".into()])
                .unwrap()
                .and_then
                .as_deref()
                == Some("25m"),
            "带值参数吃掉的那一条不该再被当成时长"
        );
    }

    /// `--hide` / `--show` 是设定值而不是翻面，所以重复执行同一个命令结果不变。
    #[test]
    fn visibility_flags_map_to_set_hidden() {
        assert_eq!(
            Intent::parse(&["--hide".into()]).unwrap().hidden,
            Some(true)
        );
        assert_eq!(
            Intent::parse(&["--show".into()]).unwrap().hidden,
            Some(false)
        );
        assert_eq!(Intent::parse(&[]).unwrap().hidden, None);
        assert_eq!(
            Intent::parse(&["--hide".into()]).unwrap().commands(),
            vec![Command::SetHidden(true)]
        );
        assert_eq!(
            Intent::parse(&["--hide".into(), "25m".into()])
                .unwrap()
                .commands(),
            vec![Command::SetHidden(true), Command::Preset(1500)],
            "可见性该排在其它命令之前"
        );
    }

    /// `--edit` / `--no-edit`：与 `--hide` 同一条路，都是"给在跑的那个下命令"，
    /// 且都是设定值而非翻转值——快捷键重复绑同一条命令也得到同样的结果。
    #[test]
    fn edit_flags_map_to_set_edit() {
        assert_eq!(Intent::parse(&["--edit".into()]).unwrap().edit, Some(true));
        assert_eq!(
            Intent::parse(&["--no-edit".into()]).unwrap().edit,
            Some(false)
        );
        assert_eq!(Intent::parse(&[]).unwrap().edit, None);
        assert_eq!(
            Intent::parse(&["--edit".into()]).unwrap().commands(),
            vec![Command::SetEdit(true)]
        );
        assert_eq!(
            Intent::parse(&["--no-edit".into(), "25m".into()])
                .unwrap()
                .commands(),
            vec![Command::SetEdit(false), Command::Preset(1500)],
        );
        // 冷启动时它不进配置：`apply` 只碰 Config 上真有的那几个键
        let mut cfg = Config::default();
        Intent::parse(&["--edit".into()]).unwrap().apply(&mut cfg);
        assert_eq!(cfg, Config::default(), "编辑态不该落到配置里");
    }

    /// 无时长语义的动作：各自翻成既有命令，与 --edit 同族都是"给在跑的那个下命令"。
    #[test]
    fn action_flags_map_to_timer_commands() {
        assert_eq!(
            Intent::parse(&["--pause".into()]).unwrap().commands(),
            vec![Command::Pause]
        );
        assert_eq!(
            Intent::parse(&["--toggle".into()]).unwrap().commands(),
            vec![Command::Toggle]
        );
        assert_eq!(
            Intent::parse(&["--reset".into()]).unwrap().commands(),
            vec![Command::Reset]
        );
        // 排在时长之后：先定读数再停表
        assert_eq!(
            Intent::parse(&["25m".into(), "--pause".into()])
                .unwrap()
                .commands(),
            vec![Command::Preset(1500), Command::Pause]
        );
    }

    /// `--input` 翻成 `Command::InputTime`，排在编辑态之后、模式之前：
    /// 先摆好界面（编辑态 / 输入行）再谈计时。
    #[test]
    fn input_flag_maps_to_the_input_command() {
        assert!(Intent::parse(&["--input".into()]).unwrap().input);
        assert_eq!(
            Intent::parse(&["--input".into()]).unwrap().commands(),
            vec![Command::InputTime]
        );
        assert_eq!(
            Intent::parse(&["--edit".into(), "--input".into(), "-s".into()])
                .unwrap()
                .commands(),
            vec![
                Command::SetEdit(true),
                Command::InputTime,
                Command::SetMode(Mode::Stopwatch)
            ]
        );
        // 不进配置：输入行是界面动作，冷启动不该记住它
        let mut cfg = Config::default();
        Intent::parse(&["--input".into()]).unwrap().apply(&mut cfg);
        assert_eq!(cfg, Config::default());
    }

    /// `--centis` 是设定值不是翻面：冷启动时它进配置（与模式同族），
    /// 转发时它是可重复的 `SetCentiseconds`。
    #[test]
    fn centis_flag_is_a_set_not_a_toggle() {
        let i = Intent::parse(&["--centis".into()]).unwrap();
        assert_eq!(i.centiseconds, Some(true));
        assert_eq!(i.commands(), vec![Command::SetCentiseconds(true)]);
        assert_eq!(
            Intent::parse(&["--no-centis".into()]).unwrap().commands(),
            vec![Command::SetCentiseconds(false)]
        );
        let mut cfg = Config::default();
        i.apply(&mut cfg);
        assert!(cfg.centiseconds, "它该进配置，下次冷启动也记得");
    }

    #[test]
    fn apply_only_overwrites_what_was_given() {
        let mut cfg = Config::default();
        let before = cfg.duration_secs;
        let autostart = Intent::parse(&["-k".into()]).unwrap().apply(&mut cfg);
        assert_eq!(cfg.mode, Mode::Clock);
        assert_eq!(cfg.duration_secs, before, "没给时长就不该动它");
        assert!(!autostart);
    }

    #[test]
    fn garbage_arg_is_an_error_not_a_silent_drop() {
        assert!(Intent::parse(&["-x".into()]).is_err());
        assert!(Intent::parse(&["banana".into()]).is_err());
        assert!(Intent::parse(&["25m".into()]).is_ok());
    }

    /// 带空格的时长必须原样回来：按空格拆开会把它变成两个参数，第二个直接解析失败。
    #[test]
    fn encode_decode_preserves_spaces_in_arguments() {
        for args in [
            vec!["25m".to_string()],
            vec!["1h 30m 10s".to_string()],
            vec!["-p".to_string(), "-r".to_string()],
            vec!["14:30".to_string()],
            vec![],
        ] {
            let line = encode(&args);
            assert!(line.ends_with('\n'));
            assert_eq!(decode(&line), args, "{args:?} 往返不一致");
        }
    }

    /// 分隔符本身混进参数里不能把一条命令拆成两条。
    #[test]
    fn separator_inside_an_argument_is_neutralised() {
        let back = decode(&encode(&[format!("a{SEP}b")]));
        assert_eq!(back, vec!["a b".to_string()]);
    }

    #[test]
    fn decode_drops_empty_fields() {
        assert_eq!(decode("25m\n"), vec!["25m".to_string()]);
        assert_eq!(decode("25m\r\n"), vec!["25m".to_string()]);
        assert!(decode("\n").is_empty());
        assert!(decode("").is_empty());
    }

    #[test]
    fn socket_path_is_absolute() {
        let p = socket_path();
        assert!(p.is_absolute(), "{}", p.display());
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tinyticker")
        );
    }

    /// 端到端：起一个 listener，走第二次调用那条路，确认命令真的落进了通道。
    /// 用独立临时路径，不碰真机那个套接字，也不依赖本机有没有开着实例。
    #[test]
    fn forward_round_trips_into_the_channel() {
        let (tx, rx) = channel();
        let (wake, _keep) = wake_pair();
        let path = std::env::temp_dir().join(format!(
            "tt-ipc-test-{}-{}.sock",
            std::process::id(),
            unique()
        ));
        serve_at(&path, tx, wake);
        // bind 是同步的，serve_at 返回时已经能连
        assert!(
            forward_to(&path, &["-p".to_string(), "600".to_string()]),
            "转发没成功"
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::SetMode(Mode::Pomodoro)
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Preset(600)
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 没人听的时候 forward 必须干净地返回 false——首次启动那条路靠它判断。
    #[test]
    fn forward_fails_cleanly_with_no_listener() {
        let path = std::env::temp_dir().join(format!(
            "tt-ipc-none-{}-{}.sock",
            std::process::id(),
            unique()
        ));
        assert!(!forward_to(&path, &["25m".to_string()]));
    }

    /// 死套接字文件（上个实例被 kill 留下的）要能接管，否则本用户永远起不来。
    #[test]
    fn bind_takes_over_a_stale_socket() {
        let path = std::env::temp_dir().join(format!(
            "tt-ipc-stale-{}-{}.sock",
            std::process::id(),
            unique()
        ));
        std::fs::write(&path, b"not a socket").unwrap();
        let listener = bind(&path).expect("死文件应该被接管");
        let _ = std::fs::remove_file(&path);
        drop(listener);
    }

    /// 无参数调用必须也要能顺利交出去。曾经的 bug：`encode([])` 只发出一个换行，
    /// 收端把空行判成"没有内容"直接 `continue`，于是永远不回 ACK，而发端在
    /// 没有超时的 `read` 上永久挂住——`tinyticker`（不带参数）在已有实例时就是这样。
    #[test]
    fn empty_invocation_is_acked_not_hung() {
        let (tx, rx) = channel();
        let (wake, _keep) = wake_pair();
        let path = std::env::temp_dir().join(format!(
            "tt-ipc-empty-{}-{}.sock",
            std::process::id(),
            unique()
        ));
        serve_at(&path, tx, wake);
        let started = std::time::Instant::now();
        assert!(forward_to(&path, &[]), "空参数没被接住");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "空参数把调用方挂住了"
        );
        // 没有任何命令产生，但调用方已经拿到回音、可以安心退出
        assert!(rx.try_recv().is_err(), "空参数不该产生命令");
        let _ = std::fs::remove_file(&path);
    }

    /// 只有一条接受循环，所以一个"连上但不说话也不断开"的客户端会把后面的转发挡在
    /// 读超时上。超时给得很短（ACCEPT_TIMEOUT），因此下一次转发仍然能按时被服务，
    /// 而不是像没有超时那样永久钉死。
    #[test]
    fn silent_client_does_not_wedge_the_listener() {
        let (tx, rx) = channel();
        let (wake, _keep) = wake_pair();
        let path = std::env::temp_dir().join(format!(
            "tt-ipc-silent-{}-{}.sock",
            std::process::id(),
            unique()
        ));
        serve_at(&path, tx, wake);
        // 故意保持连接打开且不写任何字节（注意 `let _ = mute` 不会提前析构一个具名变量，
        // 必须显式 drop 才能造出"哑巴但在线"的客户端）
        let mute = UnixStream::connect(&path).unwrap();
        let started = std::time::Instant::now();
        assert!(
            forward_to(&path, &["25m".to_string()]),
            "哑巴连接把后面的转发卡死了"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "耗时 {:?} 超出预算",
            started.elapsed()
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Command::Preset(1500)
        );
        drop(mute);
        let _ = std::fs::remove_file(&path);
    }

    fn unique() -> u32 {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }
}
