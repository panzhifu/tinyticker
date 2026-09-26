//! 系统托盘与桌面通知：直调 libdbus 实现 StatusNotifierItem + com.canonical.dbusmenu。
//!
//! 不依赖任何 Rust 托盘库：dlopen `libdbus-1.so.3`，导出
//! - `/StatusNotifierItem`（`org.kde.StatusNotifierItem`）：属性里 `IconPixmap` 直接
//!   带 32x32 位图（ARGB32 网络字节序），因此不需要任何图标文件；`Menu` 指向
//! - `/MenuBar`（`com.canonical.dbusmenu`）：宿主拉取菜单布局，点击回 `Event`
//!
//! 通知走 `org.freedesktop.Notifications.Notify`，并用消息过滤器接
//! `ActionInvoked` 信号来实现通知上的按钮。
//!
//! 整条链路跑在独立线程上；托盘不可用（没有 StatusNotifierWatcher，或没有 libdbus）
//! 时只打印警告，不影响悬浮窗口。
// 本模块几乎每个函数体都是逐条 libdbus 调用；按 C 的习惯，`unsafe fn` 本身就声明了
// 调用前置条件（消息/迭代器非空且属于当前派发），再给上百处调用点逐个套 unsafe 块
// 只会淹没真正需要审读的边界。
#![allow(unsafe_op_in_unsafe_fn)]
mod dbusmenu;
mod icon;
mod menu;
mod sni;
mod wire;

// 五个子模块按"谁在总线上说话"分：`sni` 是 StatusNotifierItem 的属性与通知，
// `menu` 是菜单数据（节点表 + 那份自持的勾选态），`dbusmenu` 是菜单的线上读写，
// `icon` 只管画像素，`wire` 是 libdbus 迭代器的小工具。它们互相之间还要调用
// （菜单要读迭代器、图标要用常量），所以名字统一收到这一层再往下给——子模块
// 顶部那条 `use super::*;` 依赖的就是这个。
use dbusmenu::{
    MENU_XML, read_event, read_ids, read_layout_request, read_property_request, read_scroll,
    second_string, write_group_properties, write_layout, write_node_property,
};
use icon::{icon_pixmap, play_speed, tooltip_text};
use menu::{Kind, Node, State, build_nodes};
use sni::{
    ITEM_XML, emit_signal, on_filter, register_with_watcher, reply_props_get, send_notification,
    write_props_all,
};
use wire::{
    close, cstr, open, put_bool, put_i32, put_str, read_i32, read_str, reply, reply_error,
    variant_bool, variant_objpath, variant_str,
};

use crate::anim;
use crate::config::Config;
use crate::effect::Effect;
use crate::lang::Language;
use crate::render::Pad;
use crate::sys::dbus as d;
use crate::sys::dbus::{DBus, DBusError, DBusMessage, DBusMessageIter, DBusObjectPathVTable};
use crate::sysinfo;
use crate::timer::Mode;
use crate::wake::WakeSender;
use std::cell::Cell;
use std::ffi::{CStr, CString, c_int, c_void};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// 托盘 → 主窗口的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Start,
    Pause,
    /// 运行中则暂停，否则开始——托盘图标左键（SNI `Activate`）用。
    Toggle,
    Reset,
    /// 快速预设：设置倒计时时长，重置并立即开始。
    Preset(u32),
    /// 切换计时模式（重置计时）。
    SetMode(Mode),
    /// 滚轮缩放：`dy` 为带符号格数，与悬浮窗上的滚轮同义。
    ZoomBy(i32),
    /// 设置背景不透明度（0-255）；文字始终不透明。
    SetAlpha(u8),
    /// 套用 `config::PALETTES` 的第 i 套配色。
    SetPalette(usize),
    /// 只换运行中的数字色：套用 `config::COLOR_OPTIONS` 的第 i 条（Catime 的那 30 条值）。
    SetRunningColor(usize),
    /// 换文字特效（写回 `text_effect`，与透明度/配色同样要落盘）。
    SetEffect(Effect),
    /// 换托盘文案语言（写回 `language`）。菜单标签在重建时跟着换。
    SetLanguage(Language),
    /// 跳到 `pomo_seq` 的第 i 段（托盘「番茄分段」那一项）。
    SetPomoStep(usize),
    /// 换托盘图标显示的内容（写回 `tray_icon`，与透明度/配色同样要落盘）。
    SetIcon(IconMode),
    /// 切换"图标里的占用指标用数字还是水位"（写回 `tray_numbers`）。
    ToggleNumbers,
    /// 换动图限速看的指标（写回 `tray_throttle`）。
    SetThrottle(Throttle),
    /// 切换数字行的百分之一秒（写回 `centiseconds`）。
    ToggleCentiseconds,
    /// 把百分之一秒设成给定值——`tinyticker --centis` / `--no-centis`，可重复执行。
    SetCentiseconds(bool),
    /// 换计时数字行的补零档位（写回 `time_pad`）。
    SetTimePad(Pad),
    /// 切换时钟挂件的秒（写回 `clock_seconds`）。
    ToggleClockSeconds,
    /// 隐藏 / 显示挂件（托盘那一项用；命令行走 [`Command::SetHidden`]）。
    ToggleHidden,
    /// 把"是否隐藏"设成给定值——`tinyticker --hide` / `--show` 的语义，可重复执行。
    SetHidden(bool),
    /// 切换编辑态（托盘那一项用；只存在于内存里，**不进配置**）。
    ToggleEdit,
    /// 按当前配置播一次提示音（托盘「试听音效」）——用户刚改完 `alarm_sound`/音量，
    /// 不用等计时结束就能确认它响不响。
    PreviewSound,
    /// 把编辑态设成给定值——`tinyticker --edit` / `--no-edit`，可重复执行。
    SetEdit(bool),
    /// 打开输入行（托盘「⌨ 输入时长」/ `tinyticker --input`）：在挂件上键入时长，
    /// 回车按预设的语义开始、Esc 取消。键盘只借这一次，关行即还。
    InputTime,
    /// 输入行换颜色模式（「外观 ▸ 文字颜色」子菜单的编辑项）：Gradient 全语法，
    /// 回车落到运行色并写回配置。
    InputColor,
    /// 输入行换分段模式（「🍅 番茄分段」子菜单的编辑项）：`25m,5m,15m` 整条替换
    /// `pomo_seq`，菜单那串经 SyncConfig 当场重建。
    InputPomo,
    /// 输入行换预设模式（「时长预设」子菜单的编辑项）：`90,1500,5400` 整条替换，
    /// 档位列表经 SyncConfig 当场重建。
    InputPresets,
    /// 切换计时结束时发不发桌面通知（写回 `notify`）。
    ToggleNotify,
    /// 登记 / 取消开机自启（写删 `~/.config/autostart/` 里那份同名条目）。
    ToggleAutostart,
    /// 把配置恢复成出厂值并立刻写盘。
    ResetConfig,
    /// 把窗口挪回出厂位置（并清掉 `window_x` / `window_y`）。
    ResetPosition,
    /// 武装一条**一次性**结束命令：下次计时结束时执行，然后自动回落。不进配置。
    ArmFinish(String),
    Quit,
}

/// 托盘图标显示什么，对应配置项 `tray_icon`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconMode {
    /// 真实时表盘：指针按当前本地时间摆放。
    Clock,
    /// 水位表 = CPU 占用（两次 /proc/stat 采样的差分）。
    Cpu,
    /// 水位表 = 内存占用（MemTotal - MemAvailable）。
    Memory,
    /// 水位表 = 剩余电量；没有电池时显示空心盘。
    Battery,
    /// 水位表 = 网络速率（上下行取大，对数刻度；`/proc/net/dev` 差分）。
    Network,
    /// 用户提供的 GIF 动图（`tray_gif`）。解不出时退回真实时表盘。
    Gif,
}

impl IconMode {
    pub fn from_name(name: &str) -> Option<IconMode> {
        match name {
            "clock" => Some(IconMode::Clock),
            "cpu" => Some(IconMode::Cpu),
            "memory" => Some(IconMode::Memory),
            "battery" => Some(IconMode::Battery),
            "network" => Some(IconMode::Network),
            "gif" => Some(IconMode::Gif),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            IconMode::Clock => "clock",
            IconMode::Cpu => "cpu",
            IconMode::Memory => "memory",
            IconMode::Battery => "battery",
            IconMode::Network => "network",
            IconMode::Gif => "gif",
        }
    }
}

/// 托盘动图按哪个指标限速，对应配置项 `tray_throttle`。档名与取值对齐 Catime 的
/// `ANIMATION_SPEED_METRIC`（`include/config/config_types.h:24-30`）。
///
/// 只管 `tray_icon = gif` 那一档：静态图标没有"速率"可以慢下来。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Throttle {
    /// 原速（Catime 的 `ORIGINAL` 档，也是我们的默认值）。
    #[default]
    Off,
    /// 看 CPU 占用。
    Cpu,
    /// 看内存占用。
    Memory,
    /// 看倒计时进度（Catime 的 `TIMER` 档）：elapsed/total 当负载百分数送进同一条
    /// 曲线——计时越到后段动图越慢。非倒计时跑动时按 0 处理（原速）。
    Timer,
    /// 固定倍率（Catime 的 `FIXED` 档）：不跟任何指标，直接按 `tray_gif_speed` 走。
    /// 这一档可以超过 1.0（双倍速），也是五档里唯一会变快的。
    Fixed,
}

impl Throttle {
    pub fn from_name(name: &str) -> Option<Throttle> {
        match name {
            "off" => Some(Throttle::Off),
            "cpu" => Some(Throttle::Cpu),
            "memory" => Some(Throttle::Memory),
            "timer" => Some(Throttle::Timer),
            "fixed" => Some(Throttle::Fixed),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Throttle::Off => "off",
            Throttle::Cpu => "cpu",
            Throttle::Memory => "memory",
            Throttle::Timer => "timer",
            Throttle::Fixed => "fixed",
        }
    }
}

const ITEM_PATH: &CStr = c"/StatusNotifierItem";
const ITEM_IFACE: &CStr = c"org.kde.StatusNotifierItem";
const WATCHER_NAME: &CStr = c"org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &CStr = c"/StatusNotifierWatcher";
const MENU_PATH: &CStr = c"/MenuBar";
const MENU_IFACE: &CStr = c"com.canonical.dbusmenu";
const PROPS_IFACE: &CStr = c"org.freedesktop.DBus.Properties";
const INTROSPECT_IFACE: &CStr = c"org.freedesktop.DBus.Introspectable";
const NOTIF_NAME: &CStr = c"org.freedesktop.Notifications";
const NOTIF_PATH: &CStr = c"/org/freedesktop/Notifications";
/// 通知按钮的动作标识，`ActionInvoked` 原样回传。
const ACTION_RESTART: &str = "restart";

// ---------------------------------------------------------------------------
// 菜单模型：启动时摊平成 id 索引的节点表
// ---------------------------------------------------------------------------

/// 主循环 → 托盘线程的消息。
enum TrayMsg {
    /// 发一条桌面通知。
    Notify(Note),
    /// 把菜单里「编辑态」那一格设成给定值。
    ///
    /// 存在的理由是**不撒谎**：编辑态可以从挂件中键或套接字进来，那两条路都不经过
    /// 菜单，托盘自持的那份镜像就不知道（GAP §七 的"勾选态漂移"）。宿主每次打开
    /// 菜单都会重新拉 `GetLayout`，所以改内存即可，不必额外发信号催它。
    SyncEdit(bool),
    /// 「隐藏挂件」那一格同理：`tinyticker --hide` / `--show` 走的是套接字。
    SyncHidden(bool),
    /// 「⌨ 输入时长」那一格的可点性：键盘可用性是后端在启动时探明的
    /// （Wayland = 座位键盘能力 + libxkbcommon），托盘自己不知道，得主循环告知。
    /// 不可用就置灰并把原因写进标签（与「试听音效」未配时同一规矩）。
    SyncKb(bool),
    /// 用一份新配置回填所有配置派生的勾选格，并按新语言重建菜单节点。
    /// 热加载与套接字那两条不经过菜单的路都靠它收口（取代逐项 `Sync*` 的趋势：
    /// 加一个配置项不会漏一条回填消息）。
    SyncConfig(Box<Config>),
    /// `pomo_seq` 当前段号（计时器的真值，主循环变化时推一次）。
    SyncPomo(Option<usize>),
    /// 倒计时进度（0-100），只在 `tray_throttle = timer` 时有意义；主循环按变化推。
    SyncProgress(u8),
}

/// 一条待发的通知。
struct Note {
    body: String,
    action: String,
}

/// 托盘线程的全部状态。会被泄漏，因此回调里的裸指针始终有效。
struct Server {
    dbus: &'static DBus,
    cmd_tx: Sender<Command>,
    /// 向主循环发命令后捅一下唤醒管道（空闲档 1s 心跳下命令不能等一个心跳）。
    wake: WakeSender,
    nodes: Vec<Node>,
    /// 重建菜单要用的三份料（语言切换与配置回填时节点表整张重造）。
    presets: Vec<u32>,
    pomo_seq: Vec<u32>,
    lang: Language,
    /// `tray_throttle = timer` 的驱动量：主循环按 1% 一格回填。
    progress: Cell<u8>,
    /// `tray_gif_speed`（Fixed 档倍率），随 SyncConfig 更新。
    fixed_pct: std::cell::Cell<u8>,
    /// 32x32 图标的 ARGB32 网络字节序像素
    pixmap: Vec<u8>,
    /// 菜单 `checked` 与图标内容的依据。只在托盘线程上读写，`RefCell` 足够。
    state: std::cell::RefCell<State>,
    /// 悬停提示的正文，每秒随采样刷新。SNI 里宿主是**拉取**这个属性的，所以我们
    /// 只能自己判断"变了没有"，变了再发 `NewToolTip` 催它回读。
    tooltip: std::cell::RefCell<String>,
}

/// 主线程用来发通知的句柄（内部只是一条到托盘线程的通道）。
pub struct TrayHandle {
    tx: Sender<TrayMsg>,
}

/// 托盘线程的开机包：配置里取出来的那五样，打包一次好过十个位置参数。
struct Boot {
    state: State,
    presets: Vec<u32>,
    pomo_seq: Vec<u32>,
    lang: Language,
    gif_path: Option<String>,
    /// `tray_gif_speed`：Fixed 限速档的倍率百分数，热加载可改。
    fixed_pct: u8,
}

/// 启动托盘线程；就绪后把 [`TrayHandle`] 发回主线程。
///
/// 配置只用来给菜单的勾选态、图标内容、时长预设与番茄分段取初值，之后托盘自己
/// 镜像菜单点击的结果，并由 `SyncConfig` / `SyncPomo` 回填不经菜单的那两条路。
pub fn spawn(
    cmd_tx: Sender<Command>,
    handle_tx: Sender<TrayHandle>,
    cfg: &Config,
    wake: WakeSender,
) {
    let boot = Boot {
        state: State::from_config(cfg),
        presets: cfg.presets.clone(),
        pomo_seq: cfg.pomo.seq.clone(),
        lang: cfg.language.resolve(),
        gif_path: cfg.tray_gif.clone(),
        fixed_pct: cfg.tray_gif_speed,
    };
    thread::spawn(move || {
        let (msg_tx, msg_rx) = std::sync::mpsc::channel::<TrayMsg>();
        match run(cmd_tx, msg_rx, handle_tx, msg_tx, wake, boot) {
            Ok(()) => {}
            Err(e) => eprintln!("⚠️ 托盘不可用: {e}"),
        }
    });
}

/// 发一条桌面通知；`action` 是按钮文案，点击后向主窗口发 [`Command::Start`]。
pub fn notify(handle: &TrayHandle, body: &str, action: &str) {
    let note = Note {
        body: body.to_string(),
        action: action.to_string(),
    };
    let _ = handle.tx.send(TrayMsg::Notify(note));
}

/// 把托盘菜单「编辑态」那一格对齐到挂件的真实状态（中键 / 套接字进来的那些）。
pub fn sync_edit(handle: &TrayHandle, on: bool) {
    let _ = handle.tx.send(TrayMsg::SyncEdit(on));
}

/// 同上，「隐藏挂件」那一格。
pub fn sync_hidden(handle: &TrayHandle, hidden: bool) {
    let _ = handle.tx.send(TrayMsg::SyncHidden(hidden));
}

/// 「⌨ 输入时长」那一格的可点性：键盘可用性由后端探明（座位能力 + libxkbcommon），
/// 托盘自己不知道，主循环在句柄到手时告知一次。
pub fn sync_kb(handle: &TrayHandle, ok: bool) {
    let _ = handle.tx.send(TrayMsg::SyncKb(ok));
}

/// 倒计时进度变了推一次（`tray_throttle = timer` 的驱动源；1% 一格，不每秒刷屏）。
pub fn sync_progress(handle: &TrayHandle, percent: u8) {
    let _ = handle.tx.send(TrayMsg::SyncProgress(percent));
}

/// 用一份新配置回填托盘：勾选格全部对齐、菜单节点按新语言重建。
/// 热加载与套接字那两条不经过菜单的路靠它收口。
pub fn sync_config(handle: &TrayHandle, cfg: &Config) {
    let _ = handle.tx.send(TrayMsg::SyncConfig(Box::new(cfg.clone())));
}

/// `pomo_seq` 当前段号变了就推一次（只在变化时，不必每秒）。
pub fn sync_pomo(handle: &TrayHandle, step: Option<usize>) {
    let _ = handle.tx.send(TrayMsg::SyncPomo(step));
}

// ---------------------------------------------------------------------------
// 线程主体
// ---------------------------------------------------------------------------

fn run(
    cmd_tx: Sender<Command>,
    msg_rx: Receiver<TrayMsg>,
    handle_tx: Sender<TrayHandle>,
    msg_tx: Sender<TrayMsg>,
    wake: WakeSender,
    boot: Boot,
) -> Result<(), String> {
    let Boot {
        state,
        presets,
        pomo_seq,
        lang,
        gif_path,
        fixed_pct,
    } = boot;
    let dbus: &'static DBus = Box::leak(Box::new(DBus::load().ok_or("打不开 libdbus-1.so.3")?));
    let mut err = DBusError::zeroed();
    let conn = unsafe { (dbus.dbus_bus_get)(d::BUS_SESSION, &mut err) };
    if conn.is_null() {
        return Err(format!("连不上会话总线: {}", err.describe()));
    }
    let mut sampler = sysinfo::Sampler::new();
    let mut player = anim::Player::new(gif_path.as_deref());
    let (_, _, pixmap) = icon_pixmap(
        state.icon,
        &sampler.sample(),
        crate::clock::now_hms(),
        &mut player,
        state.numbers,
    );
    // 键盘可用性启动时按"可用"乐观：后端探明拿不到才经 SyncKb 推一次置灰
    let nodes = build_nodes(&presets, state.gif, state.sound, true, &pomo_seq, lang);
    let server: *mut Server = Box::leak(Box::new(Server {
        dbus,
        cmd_tx,
        wake,
        nodes,
        presets,
        pomo_seq,
        lang,
        progress: Cell::new(0),
        fixed_pct: Cell::new(fixed_pct),
        pixmap,
        state: std::cell::RefCell::new(state),
        tooltip: std::cell::RefCell::new(String::new()),
    }));

    // 导出两个对象路径
    let vtable = Box::leak(Box::new(DBusObjectPathVTable::new(on_message)));
    for path in [ITEM_PATH, MENU_PATH] {
        unsafe {
            (dbus.dbus_connection_try_register_object_path)(
                conn,
                path.as_ptr(),
                vtable as *const DBusObjectPathVTable,
                server as *mut c_void,
                &mut err,
            )
        };
    }

    // 注册到宿主
    let unique = unsafe { CStr::from_ptr((dbus.dbus_bus_get_unique_name)(conn)) };
    unsafe { register_with_watcher(dbus, conn, unique.to_string_lossy().as_ref()) };

    // 通知按钮回调：需要 match 规则 + 消息过滤器
    let rule = CString::new(
        "type='signal',interface='org.freedesktop.Notifications',member='ActionInvoked'",
    )
    .unwrap();
    unsafe { (dbus.dbus_bus_add_match)(conn, rule.as_ptr(), &mut err) };
    unsafe {
        (dbus.dbus_connection_add_filter)(conn, Some(on_filter), server as *mut c_void, None)
    };

    // 主线程拿到句柄后就能发通知
    if handle_tx.send(TrayHandle { tx: msg_tx }).is_err() {
        return Ok(());
    }

    // 派发循环：200ms 醒一次，顺带把待发通知送出去、按秒刷新托盘图标
    let mut icon_at = Instant::now();
    let mut sources = sampler.sample();
    loop {
        while let Ok(msg) = msg_rx.try_recv() {
            match msg {
                TrayMsg::Notify(note) => unsafe { send_notification(dbus, conn, &note) },
                TrayMsg::SyncEdit(on) => unsafe { (*server).state.borrow_mut().edit = on },
                TrayMsg::SyncHidden(v) => unsafe { (*server).state.borrow_mut().hidden = v },
                // 键盘可用性连着那一项的标签与可点性：格子和节点表都得跟上
                TrayMsg::SyncKb(ok) => unsafe {
                    let s = &mut *server;
                    s.state.borrow_mut().kb_ok = ok;
                    let (gif, sound, kb, lang) = {
                        let st = s.state.borrow();
                        (st.gif, st.sound, st.kb_ok, s.lang)
                    };
                    s.nodes =
                        build_nodes(&s.presets.clone(), gif, sound, kb, &s.pomo_seq.clone(), lang);
                },
                TrayMsg::SyncPomo(step) => unsafe { (*server).state.borrow_mut().pomo_step = step },
                TrayMsg::SyncProgress(p) => unsafe { (*server).progress.set(p) },
                TrayMsg::SyncConfig(cfg) => unsafe {
                    let s = &mut *server;
                    s.state.borrow_mut().resync(&cfg);
                    // 序列变了要重造的子菜单不只是勾选：段数本身是菜单结构
                    s.pomo_seq = cfg.pomo.seq.clone();
                    // 预设档位同理（输入行/热加载改了 presets，托盘那串要跟着换）
                    s.presets = cfg.presets.clone();
                    s.lang = cfg.language.resolve();
                    s.fixed_pct.set(cfg.tray_gif_speed);
                    let (gif, sound, kb) = {
                        let st = s.state.borrow();
                        (st.gif, st.sound, st.kb_ok)
                    };
                    s.nodes = build_nodes(
                        &s.presets.clone(),
                        gif,
                        sound,
                        kb,
                        &s.pomo_seq.clone(),
                        s.lang,
                    );
                },
            }
        }
        // 采样按秒，图标按派发节拍（200ms）：动图帧间隔可以短到几十毫秒
        if icon_at.elapsed() >= ICON_EVERY {
            icon_at = Instant::now();
            sources = sampler.sample();
            // 悬停提示跟着采样走：宿主只在收到 NewToolTip 时才回读 ToolTip 属性。
            // 动图档且限速开着时多一行当前倍率（Catime 的 tooltip 有这一行）
            let (tip_mode, tip_throttle) = {
                let st = unsafe { (*server).state.borrow() };
                (st.icon, st.throttle)
            };
            let anim_speed =
                (tip_mode == IconMode::Gif && tip_throttle != Throttle::Off).then(|| {
                    play_speed(
                        tip_throttle,
                        &sources,
                        unsafe { (*server).progress.get() },
                        unsafe { (*server).fixed_pct.get() },
                    )
                });
            let tip = tooltip_text(&sources, anim_speed);
            let changed = unsafe {
                let cell = &mut *server;
                let mut slot = cell.tooltip.borrow_mut();
                if *slot == tip {
                    false
                } else {
                    *slot = tip;
                    true
                }
            };
            if changed {
                unsafe { emit_signal(dbus, conn, c"NewToolTip") };
            }
        }
        // 图标内容可能刚被菜单改过，每轮从 state 取而不是记在局部变量里
        let (mode, numbers, throttle) = {
            let st = unsafe { (*server).state.borrow() };
            (st.icon, st.numbers, st.throttle)
        };
        // 倍率每轮都推进虚拟时钟（哪怕当前不是动图档）：只在切到动图时才推的话，
        // 从"不限速"切回来那一刻帧序会按累计的墙钟跳一大段
        let speed = play_speed(
            throttle,
            &sources,
            unsafe { (*server).progress.get() },
            unsafe { (*server).fixed_pct.get() },
        );
        player.advance(speed);
        let (_, _, px) = icon_pixmap(
            mode,
            &sources,
            crate::clock::now_hms(),
            &mut player,
            numbers,
        );
        // 裸指针只在两次派发之间换整个 Vec：回调拿到的 &Server 不会看到写了一半的图标
        let changed = unsafe {
            let server = &mut *server;
            if server.pixmap == px {
                false
            } else {
                server.pixmap = px;
                true
            }
        };
        // 像素真变了才发信号：宿主收到 NewIcon 会立刻回读 IconPixmap
        if changed {
            unsafe { emit_signal(dbus, conn, c"NewIcon") };
        }
        if unsafe { (dbus.dbus_connection_read_write_dispatch)(conn, 200) } != d::TRUE {
            return Ok(()); // 连接断了（多数是退出登录）
        }
    }
}

unsafe extern "C" fn on_message(
    conn: *mut d::DBusConnection,
    msg: *mut DBusMessage,
    data: *mut c_void,
) -> c_int {
    let server = unsafe { &*(data as *const Server) };
    let dbus = server.dbus;
    let member = match cstr(unsafe { (dbus.dbus_message_get_member)(msg) }) {
        Some(m) => m,
        None => return d::NOT_YET_HANDLED,
    };
    let iface = cstr(unsafe { (dbus.dbus_message_get_interface)(msg) }).unwrap_or_default();
    let path = cstr(unsafe { (dbus.dbus_message_get_path)(msg) }).unwrap_or_default();
    let is_call = unsafe { (dbus.dbus_message_get_type)(msg) } == d::MSG_METHOD_CALL;
    if !is_call {
        return d::NOT_YET_HANDLED;
    }

    let mut args = DBusMessageIter::uninit();
    let has_args = unsafe { (dbus.dbus_message_iter_init)(msg, &mut args) } == d::TRUE;
    let mut empty = DBusMessageIter::uninit();
    let args = if has_args { &mut args } else { &mut empty };

    // 接口/路径名统一用上面的 CStr 常量比对（它们同时也是注册对象路径时用的）
    let (item_p, menu_p) = (ITEM_PATH.to_str().unwrap(), MENU_PATH.to_str().unwrap());
    let (props_i, intro_i, item_i, menu_i) = (
        PROPS_IFACE.to_str().unwrap(),
        INTROSPECT_IFACE.to_str().unwrap(),
        ITEM_IFACE.to_str().unwrap(),
        MENU_IFACE.to_str().unwrap(),
    );
    match member.as_str() {
        "Ping" => reply(dbus, conn, msg, |_, _| {}),
        "Introspect" if iface == intro_i => {
            let xml = if path == menu_p { MENU_XML } else { ITEM_XML };
            reply(dbus, conn, msg, |db, it| put_str(db, it, xml))
        }
        "Get" if iface == props_i => {
            // (s 接口名, s 属性名) → v
            let name = second_string(dbus, args);
            reply_props_get(dbus, conn, msg, server, &name)
        }
        "GetAll" if iface == props_i => {
            reply(dbus, conn, msg, |db, it| write_props_all(db, it, server))
        }
        // 左键 = 开始/暂停，中键 = 重置，右键 = 让宿主弹 /MenuBar。
        // 这三个方法在 ITEM_XML 里一直都有声明，此前一律只回成功。
        "Activate" if path == item_p && iface == item_i => {
            let _ = server.cmd_tx.send(Command::Toggle);
            server.wake.wake();
            reply(dbus, conn, msg, |_, _| {})
        }
        "SecondaryActivate" if path == item_p && iface == item_i => {
            let _ = server.cmd_tx.send(Command::Reset);
            server.wake.wake();
            reply(dbus, conn, msg, |_, _| {})
        }
        "ContextMenu" if path == item_p && iface == item_i => reply(dbus, conn, msg, |_, _| {}),
        // 滚轮缩放：ITEM_XML 里声明了 Scroll，就得真的接住，否则宿主收不到回复。
        // 悬浮窗太小、又常开着点击穿透，在托盘图标上滚反而是更顺手的一条路。
        "Scroll" if path == item_p && iface == item_i => {
            if let Some(delta) = read_scroll(dbus, args) {
                let _ = server.cmd_tx.send(Command::ZoomBy(delta));
                server.wake.wake();
            }
            reply(dbus, conn, msg, |_, _| {})
        }
        "GetLayout" if path == menu_p && iface == menu_i => {
            let (root, depth) = read_layout_request(dbus, args);
            reply(dbus, conn, msg, |db, it| {
                put_i32(db, it, 1); // revision
                write_layout(db, it, server, root, depth);
            })
        }
        "GetGroupProperties" if path == menu_p && iface == menu_i => {
            let ids = read_ids(dbus, args);
            reply(dbus, conn, msg, |db, it| {
                write_group_properties(db, it, server, &ids)
            })
        }
        "GetProperty" if path == menu_p && iface == menu_i => {
            let (id, name) = read_property_request(dbus, args);
            reply(dbus, conn, msg, |db, it| {
                write_node_property(db, it, server, id, &name)
            })
        }
        "AboutToShow" if path == menu_p && iface == menu_i => {
            reply(dbus, conn, msg, |db, it| put_bool(db, it, false))
        }
        "Event" if path == menu_p && iface == menu_i => {
            if let Some(id) = read_event(dbus, args)
                && let Some(cmd) = server.nodes.get(id as usize).and_then(|n| n.command.as_ref())
                // 置灰的项（比如没配路径的 GIF 档）不该有动作，哪怕宿主还是发了 Event
                && server.state.borrow().enabled(cmd)
            {
                // 先镜像再转发：菜单的 checked 读的是这一份，不能等主窗口回话
                server.state.borrow_mut().note(cmd);
                let _ = server.cmd_tx.send(cmd.clone());
                // 空闲档心跳 1s：不唤醒的话，点了菜单要等到下一拍才结算
                server.wake.wake();
            }
            reply(dbus, conn, msg, |_, _| {})
        }
        "EventGroup" if path == menu_p && iface == menu_i => {
            reply(dbus, conn, msg, |db, it| put_bool(db, it, false))
        }
        _ => d::NOT_YET_HANDLED,
    }
}

/// 图标刷新间隔：分针一秒走 6 度，1 秒足够；再快只是多读 /proc。
const ICON_EVERY: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn icon_mode_names_roundtrip() {
        for m in [
            IconMode::Clock,
            IconMode::Cpu,
            IconMode::Memory,
            IconMode::Battery,
            IconMode::Network,
            IconMode::Gif,
        ] {
            assert_eq!(IconMode::from_name(m.name()), Some(m));
        }
        assert_eq!(IconMode::from_name("disk"), None);
    }
}
