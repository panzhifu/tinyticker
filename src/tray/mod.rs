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
use icon::{icon_pixmap, tooltip_text};
use menu::{Kind, Node, State, build_nodes};
use sni::{
    ITEM_XML, emit_signal, on_filter, register_with_watcher, reply_props_get, send_notification,
    write_props_all,
};
use wire::{
    close, cstr, open, put_bool, put_i32, put_str, read_i32, read_str, reply, reply_error,
    variant_bool, variant_objpath, variant_str,
};

use std::ffi::{CStr, CString, c_int, c_void};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use crate::config::Config;
use crate::effect::Effect;
use crate::gif;
use crate::render::Pad;
use crate::sys::dbus as d;
use crate::sys::dbus::{DBus, DBusError, DBusMessage, DBusMessageIter, DBusObjectPathVTable};
use crate::sysinfo;
use crate::timer::Mode;

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
    /// 换托盘图标显示的内容（写回 `tray_icon`，与透明度/配色同样要落盘）。
    SetIcon(IconMode),
    /// 切换数字行的百分之一秒（写回 `centiseconds`）。
    ToggleCentiseconds,
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
    /// 把编辑态设成给定值——`tinyticker --edit` / `--no-edit`，可重复执行。
    SetEdit(bool),
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
    nodes: Vec<Node>,
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

/// 启动托盘线程；就绪后把 [`TrayHandle`] 发回主线程。
///
/// 配置只用来给菜单的勾选态、图标内容和时长预设取初值，之后托盘自己镜像菜单点击的结果。
pub fn spawn(cmd_tx: Sender<Command>, handle_tx: Sender<TrayHandle>, cfg: &Config) {
    let state = State::from_config(cfg);
    let presets = cfg.presets.clone();
    let gif_path = cfg.tray_gif.clone();
    thread::spawn(move || {
        let (msg_tx, msg_rx) = std::sync::mpsc::channel::<TrayMsg>();
        match run(cmd_tx, msg_rx, handle_tx, msg_tx, state, presets, gif_path) {
            Ok(()) => {}
            Err(e) => eprintln!("⚠️ 托盘不可用: {e}"),
        }
    });
}

/// 发一条桌面通知；`action` 是按钮文案，点击后向主窗口发 [`Command::Start`]。
pub fn notify(handle: &TrayHandle, body: &str, action: &str) {
    let note = Note { body: body.to_string(), action: action.to_string() };
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

// ---------------------------------------------------------------------------
// 线程主体
// ---------------------------------------------------------------------------


fn run(
    cmd_tx: Sender<Command>,
    msg_rx: Receiver<TrayMsg>,
    handle_tx: Sender<TrayHandle>,
    msg_tx: Sender<TrayMsg>,
    state: State,
    presets: Vec<u32>,
    gif_path: Option<String>,
) -> Result<(), String> {
    let dbus: &'static DBus = Box::leak(Box::new(DBus::load().ok_or("打不开 libdbus-1.so.3")?));
    let mut err = DBusError::zeroed();
    let conn = unsafe { (dbus.dbus_bus_get)(d::BUS_SESSION, &mut err) };
    if conn.is_null() {
        return Err(format!("连不上会话总线: {}", err.describe()));
    }
    let mut sampler = sysinfo::Sampler::new();
    let mut player = gif::Player::new(gif_path.as_deref());
    let (_, _, pixmap) =
        icon_pixmap(state.icon, &sampler.sample(), crate::clock::now_hms(), &mut player);
    let server: *mut Server = Box::leak(Box::new(Server {
        dbus,
        cmd_tx,
        nodes: build_nodes(&presets, state.gif),
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
            }
        }
        // 采样按秒，图标按派发节拍（200ms）：动图帧间隔可以短到几十毫秒
        if icon_at.elapsed() >= ICON_EVERY {
            icon_at = Instant::now();
            sources = sampler.sample();
            // 悬停提示跟着采样走：宿主只在收到 NewToolTip 时才回读 ToolTip 属性
            let tip = tooltip_text(&sources);
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
        let mode = unsafe { (*server).state.borrow().icon };
        let (_, _, px) = icon_pixmap(mode, &sources, crate::clock::now_hms(), &mut player);
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
            reply(dbus, conn, msg, |_, _| {})
        }
        "SecondaryActivate" if path == item_p && iface == item_i => {
            let _ = server.cmd_tx.send(Command::Reset);
            reply(dbus, conn, msg, |_, _| {})
        }
        "ContextMenu" if path == item_p && iface == item_i => {
            reply(dbus, conn, msg, |_, _| {})
        }
        // 滚轮缩放：ITEM_XML 里声明了 Scroll，就得真的接住，否则宿主收不到回复。
        // 悬浮窗太小、又常开着点击穿透，在托盘图标上滚反而是更顺手的一条路。
        "Scroll" if path == item_p && iface == item_i => {
            if let Some(delta) = read_scroll(dbus, args) {
                let _ = server.cmd_tx.send(Command::ZoomBy(delta));
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
            reply(dbus, conn, msg, |db, it| write_group_properties(db, it, server, &ids))
        }
        "GetProperty" if path == menu_p && iface == menu_i => {
            let (id, name) = read_property_request(dbus, args);
            reply(dbus, conn, msg, |db, it| write_node_property(db, it, server, id, &name))
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
