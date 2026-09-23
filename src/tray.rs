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

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::sys::dbus as d;
use crate::sys::dbus::{DBus, DBusError, DBusMessage, DBusMessageIter, DBusObjectPathVTable};
use crate::sysinfo;
use crate::timer::Mode;

/// 托盘 → 主窗口的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Pause,
    Reset,
    /// 快速预设：设置倒计时时长，重置并立即开始。
    Preset(u32),
    /// 切换计时模式（重置计时）。
    SetMode(Mode),
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
}

impl IconMode {
    pub fn from_name(name: &str) -> Option<IconMode> {
        match name {
            "clock" => Some(IconMode::Clock),
            "cpu" => Some(IconMode::Cpu),
            "memory" => Some(IconMode::Memory),
            "battery" => Some(IconMode::Battery),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            IconMode::Clock => "clock",
            IconMode::Cpu => "cpu",
            IconMode::Memory => "memory",
            IconMode::Battery => "battery",
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

/// 快速预设（标签, 秒）。
const PRESETS: [(&str, u32); 6] = [
    ("1 分钟", 60),
    ("5 分钟", 300),
    ("15 分钟", 900),
    ("25 分钟", 1500),
    ("45 分钟", 2700),
    ("1 小时", 3600),
];

// ---------------------------------------------------------------------------
// 菜单模型：启动时摊平成 id 索引的节点表
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Root,
    Button,
    Separator,
    Submenu,
}

struct Node {
    kind: Kind,
    label: String,
    command: Option<Command>,
    children: Vec<i32>,
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
}

fn build_nodes() -> Vec<Node> {
    let button = |label: &str, command: Command| Node {
        kind: Kind::Button,
        label: label.into(),
        command: Some(command),
        children: Vec::new(),
    };
    let mut n = vec![Node {
        kind: Kind::Root,
        label: String::new(),
        command: None,
        children: Vec::new(),
    }];
    n.push(button("▶ 开始", Command::Start));
    n.push(button("⏸ 暂停", Command::Pause));
    n.push(button("⟳ 重置", Command::Reset));
    n.push(Node { kind: Kind::Separator, label: String::new(), command: None, children: Vec::new() });
    let presets = n.len() as i32;
    n.push(Node {
        kind: Kind::Submenu,
        label: "时长预设".into(),
        command: None,
        children: Vec::new(),
    });
    let modes = n.len() as i32;
    n.push(Node {
        kind: Kind::Submenu,
        label: "模式".into(),
        command: None,
        children: Vec::new(),
    });
    n.push(Node { kind: Kind::Separator, label: String::new(), command: None, children: Vec::new() });
    n.push(button("✕ 退出", Command::Quit));
    for (label, secs) in PRESETS {
        let id = n.len() as i32;
        n.push(button(&format!("⏱ {label}"), Command::Preset(secs)));
        n[presets as usize].children.push(id);
    }
    for (label, mode) in [
        ("倒计时", Mode::Countdown),
        ("秒表", Mode::Stopwatch),
        ("🍅 番茄钟", Mode::Pomodoro),
        ("🕐 时钟", Mode::Clock),
    ] {
        let id = n.len() as i32;
        n.push(button(label, Command::SetMode(mode)));
        n[modes as usize].children.push(id);
    }
    // 根节点的子项顺序即菜单顺序
    n[0].children = (1..=8).collect();
    n
}

/// 32x32 时钟图标（深色表盘 + 白色表圈和指针），输出 ARGB32 网络字节序。
///
/// 与悬浮窗的位图字体同一取向：不引入任何图片资源文件。
fn clock_pixmap(now: (u32, u32, u32)) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    let (h, m, _) = now;
    // 表盘位置以「分钟格」为单位：时针含分钟分量，否则一小时里指针会跳一下
    hand(&mut px, dial_dir((h % 12) as f32 * 5.0 + m as f32 / 12.0), 8.0);
    hand(&mut px, dial_dir(m as f32), 12.0);
    (S as i32, S as i32, px)
}

/// 32x32 表盘底：白色圆环 + 深色填充，圆外全透明。
fn face() -> Vec<u8> {
    let mut px = vec![0u8; S * S * 4]; // 圆外全透明
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f32 - CENTER, y as f32 - CENTER);
            if dist(dx, dy) > 15.0 {
                continue;
            }
            let (r, g, b) = if dist(dx, dy) > 12.5 { (255, 255, 255) } else { DARK };
            put(&mut px, x, y, r, g, b);
        }
    }
    px
}

/// 60 个整分钟方向的单位向量（0 = 12 点，顺时针），×4096 定点。
///
/// 用固定 6° 增量旋转累加生成，而不是调 `sin`/`cos`：后者会让我们去链 libm，
/// 而本项目的卖点是 `ldd` 里只有 libc 与 libgcc_s。所选整数 (4074, 428) 的模长
/// 比 4096 大 0.01%，转满一圈累计误差不到 1 像素。
const DIAL: [(i32, i32); 60] = {
    let mut t = [(0i32, 0i32); 60];
    let (mut x, mut y) = (0i32, -4096i32); // 12 点方向：屏幕 y 轴向下，故取负
    let mut i = 0;
    while i < 60 {
        t[i] = (x, y);
        let (nx, ny) = ((x * 4074 - y * 428) >> 12, (x * 428 + y * 4074) >> 12);
        (x, y) = (nx, ny);
        i += 1;
    }
    t
};

/// 第 60 格回到起点，因此 `pos` 可取任意 [0, 60) 的实数，相邻两格线性插值。
fn dial_dir(pos: f32) -> (f32, f32) {
    let lo = pos.floor() as usize % 60;
    let hi = (lo + 1) % 60;
    let f = pos - pos.floor();
    let (a, b) = (DIAL[lo], DIAL[hi]);
    (
        (a.0 as f32 + (b.0 - a.0) as f32 * f) / 4096.0,
        (a.1 as f32 + (b.1 - a.1) as f32 * f) / 4096.0,
    )
}

/// 从圆心沿单位向量 `(sx, sy)` 画一条 `len` 长的 2px 指针。
fn hand(px: &mut [u8], (sx, sy): (f32, f32), len: f32) {
    for step in 0..(len * 2.0) as usize {
        let t = step as f32 / 2.0;
        // 2px 粗：沿指针方向再错开半像素画一次
        put(px, (CENTER + sx * t) as usize, (CENTER + sy * t) as usize, 255, 255, 255);
        put(px, (CENTER + sx * (t + 0.5)) as usize, (CENTER + sy * (t + 0.5)) as usize, 255, 255, 255);
    }
}

/// 占用表：深色内盘自底向上填到与 `percent` 对应的水位线，颜色按阈值分级。
/// 没有电池的机器选电池档时显示空心盘，与「0%」区分开。
fn gauge_pixmap(percent: Option<u8>, charging: bool) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    if let Some(p) = percent {
        // 充电中一律绿色：20% 的红色会让人以为快没电，而实际在涨
        let (r, g, b) = if charging { (80, 220, 120) } else { level_color(p) };
        // 水位线的 y 偏移：0% 在顶端（不填充），100% 在底端（填满内盘）
        let line = 12.5 - p as f32 * 0.25;
        for y in 0..S {
            for x in 0..S {
                let (dx, dy) = (x as f32 - CENTER, y as f32 - CENTER);
                if dist(dx, dy) > 12.5 || dy < line {
                    continue;
                }
                put(&mut px, x, y, r, g, b);
            }
        }
    }
    (S as i32, S as i32, px)
}

/// 按 `mode` 生成当前该显示的图标。
fn icon_pixmap(
    mode: IconMode,
    src: &sysinfo::Sources,
    now: (u32, u32, u32),
) -> (i32, i32, Vec<u8>) {
    match mode {
        IconMode::Clock => clock_pixmap(now),
        IconMode::Cpu => gauge_pixmap(Some(src.cpu), false),
        IconMode::Memory => gauge_pixmap(Some(src.mem), false),
        IconMode::Battery => {
            gauge_pixmap(src.battery.map(|b| b.percent), src.battery.is_some_and(|b| b.charging))
        }
    }
}

fn dist(dx: f32, dy: f32) -> f32 {
    (dx * dx + dy * dy).sqrt()
}

fn level_color(percent: u8) -> (u8, u8, u8) {
    match percent {
        0..=59 => (80, 220, 120),
        60..=84 => (255, 200, 80),
        _ => (240, 90, 90),
    }
}

const S: usize = 32;
const CENTER: f32 = 15.5;
const DARK: (u8, u8, u8) = (15, 15, 20);

/// SNI 的 IconPixmap 是网络字节序（大端）的 A,R,G,B 四字节。
fn put(px: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let i = (y * 32 + x) * 4;
    px[i] = 255;
    px[i + 1] = r;
    px[i + 2] = g;
    px[i + 3] = b;
}

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------

/// 主线程用来发通知的句柄（内部只是一条到托盘线程的通道）。
pub struct TrayHandle {
    tx: Sender<Note>,
}

/// 启动托盘线程；就绪后把 [`TrayHandle`] 发回主线程。
pub fn spawn(cmd_tx: Sender<Command>, handle_tx: Sender<TrayHandle>, icon: IconMode) {
    thread::spawn(move || {
        let (note_tx, note_rx) = std::sync::mpsc::channel::<Note>();
        match run(cmd_tx, note_rx, handle_tx, note_tx, icon) {
            Ok(()) => {}
            Err(e) => eprintln!("⚠️ 托盘不可用: {e}"),
        }
    });
}

/// 发一条桌面通知；`action` 是按钮文案，点击后向主窗口发 [`Command::Start`]。
pub fn notify(handle: &TrayHandle, body: &str, action: &str) {
    let _ = handle.tx.send(Note { body: body.to_string(), action: action.to_string() });
}

// ---------------------------------------------------------------------------
// 线程主体
// ---------------------------------------------------------------------------


fn run(
    cmd_tx: Sender<Command>,
    note_rx: Receiver<Note>,
    handle_tx: Sender<TrayHandle>,
    note_tx: Sender<Note>,
    icon: IconMode,
) -> Result<(), String> {
    let dbus: &'static DBus = Box::leak(Box::new(DBus::load().ok_or("打不开 libdbus-1.so.3")?));
    let mut err = DBusError::zeroed();
    let conn = unsafe { (dbus.dbus_bus_get)(d::BUS_SESSION, &mut err) };
    if conn.is_null() {
        return Err(format!("连不上会话总线: {}", err.describe()));
    }
    let mut sampler = sysinfo::Sampler::new();
    let (_, _, pixmap) = icon_pixmap(icon, &sampler.sample(), crate::clock::now_hms());
    let server: *mut Server = Box::leak(Box::new(Server {
        dbus,
        cmd_tx,
        nodes: build_nodes(),
        pixmap,
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
    if handle_tx.send(TrayHandle { tx: note_tx }).is_err() {
        return Ok(());
    }

    // 派发循环：200ms 醒一次，顺带把待发通知送出去、按秒刷新托盘图标
    let mut icon_at = Instant::now();
    loop {
        while let Ok(note) = note_rx.try_recv() {
            unsafe { send_notification(dbus, conn, &note) };
        }
        if icon_at.elapsed() >= ICON_EVERY {
            icon_at = Instant::now();
            let (_, _, px) = icon_pixmap(icon, &sampler.sample(), crate::clock::now_hms());
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
                unsafe { emit_new_icon(dbus, conn) };
            }
        }
        if unsafe { (dbus.dbus_connection_read_write_dispatch)(conn, 200) } != d::TRUE {
            return Ok(()); // 连接断了（多数是退出登录）
        }
    }
}

/// `org.kde.StatusNotifierItem.NewIcon`：不声明刷新，宿主会一直显示启动时那份缓存。
unsafe fn emit_new_icon(dbus: &DBus, conn: *mut d::DBusConnection) {
    let msg = (dbus.dbus_message_new_signal)(
        ITEM_PATH.as_ptr(),
        ITEM_IFACE.as_ptr(),
        c"NewIcon".as_ptr(),
    );
    if msg.is_null() {
        return;
    }
    (dbus.dbus_connection_send)(conn, msg, std::ptr::null_mut());
    (dbus.dbus_connection_flush)(conn);
    (dbus.dbus_message_unref)(msg);
}

/// `org.freedesktop.Notifications.Notify`：带一个「再来一次」按钮。
unsafe fn send_notification(dbus: &DBus, conn: *mut d::DBusConnection, note: &Note) {
    let msg = (dbus.dbus_message_new_method_call)(
        NOTIF_NAME.as_ptr(),
        NOTIF_PATH.as_ptr(),
        NOTIF_NAME.as_ptr(),
        c"Notify".as_ptr(),
    );
    if msg.is_null() {
        return;
    }
    let mut it = DBusMessageIter::uninit();
    (dbus.dbus_message_iter_init_append)(msg, &mut it);
    put_str(dbus, &mut it, "TinyTicker"); // app_name
    put_u32(dbus, &mut it, 0); // replaces_id
    put_str(dbus, &mut it, ""); // 图标名：留空，宿主用我们的 SNI 图标
    put_str(dbus, &mut it, "TinyTicker"); // summary
    put_str(dbus, &mut it, &note.body); // body
    // actions: as —— [key, label] 成对
    let mut acts = match open(dbus, &mut it, d::T_ARRAY, Some(c"s")) {
        Some(a) => a,
        None => return,
    };
    put_str(dbus, &mut acts, ACTION_RESTART);
    put_str(dbus, &mut acts, &note.action);
    close(dbus, &mut it, &mut acts);
    // hints: a{sv} —— 空字典
    let mut hints = match open(dbus, &mut it, d::T_ARRAY, Some(c"{sv}")) {
        Some(h) => h,
        None => return,
    };
    close(dbus, &mut it, &mut hints);
    put_i32(dbus, &mut it, 0); // expire_timeout：0 = 用默认
    (dbus.dbus_connection_send)(conn, msg, std::ptr::null_mut());
    (dbus.dbus_connection_flush)(conn);
    (dbus.dbus_message_unref)(msg);
}

unsafe fn put_u32(dbus: &DBus, it: *mut DBusMessageIter, v: u32) {
    (dbus.dbus_message_iter_append_basic)(it, d::T_UINT32, &v as *const _ as *const c_void);
}

unsafe fn register_with_watcher(dbus: &DBus, conn: *mut d::DBusConnection, service: &str) {
    let msg = (dbus.dbus_message_new_method_call)(
        WATCHER_NAME.as_ptr(),
        WATCHER_PATH.as_ptr(),
        WATCHER_NAME.as_ptr(),
        c"RegisterStatusNotifierItem".as_ptr(),
    );
    if msg.is_null() {
        return;
    }
    let mut it = DBusMessageIter::uninit();
    (dbus.dbus_message_iter_init_append)(msg, &mut it);
    put_str(dbus, &mut it, service);
    let mut err = DBusError::zeroed();
    let reply = (dbus.dbus_connection_send_with_reply_and_block)(conn, msg, 2000, &mut err);
    if reply.is_null() {
        eprintln!("⚠️ 注册托盘图标失败: {}", err.describe());
    } else {
        (dbus.dbus_message_unref)(reply);
    }
    (dbus.dbus_message_unref)(msg);
}

// ---------------------------------------------------------------------------
// 消息处理
// ---------------------------------------------------------------------------

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
        // 三击都只回成功：ItemIsMenu=true，宿主自己弹 /MenuBar
        "Activate" | "SecondaryActivate" | "ContextMenu" if path == item_p && iface == item_i => {
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
                && let Some(cmd) = server.nodes.get(id as usize).and_then(|n| n.command)
            {
                let _ = server.cmd_tx.send(cmd);
            }
            reply(dbus, conn, msg, |_, _| {})
        }
        "EventGroup" if path == menu_p && iface == menu_i => {
            reply(dbus, conn, msg, |db, it| put_bool(db, it, false))
        }
        _ => d::NOT_YET_HANDLED,
    }
}

/// 通知上的按钮被按下。
///
/// `DBusHandleMessageFunction` 的三个参数是 `(connection, message, user_data)`，
/// 与对象路径 vtable 同形，没有 `DBusError`。
unsafe extern "C" fn on_filter(
    _conn: *mut d::DBusConnection,
    msg: *mut DBusMessage,
    data: *mut c_void,
) -> c_int {
    let server = unsafe { &*(data as *const Server) };
    let dbus = server.dbus;
    if unsafe { (dbus.dbus_message_is_signal)(msg, NOTIF_NAME.as_ptr(), c"ActionInvoked".as_ptr()) }
        != d::TRUE
    {
        return d::NOT_YET_HANDLED;
    }
    let mut it = DBusMessageIter::uninit();
    if unsafe { (dbus.dbus_message_iter_init)(msg, &mut it) } != d::TRUE {
        return d::NOT_YET_HANDLED;
    }
    // (u id, s key)
    (dbus.dbus_message_iter_next)(&mut it);
    let key = match cstr({
        let mut p: *const c_char = std::ptr::null();
        (dbus.dbus_message_iter_get_basic)(&mut it, &mut p as *mut _ as *mut c_void);
        p
    }) {
        Some(k) => k,
        None => return d::NOT_YET_HANDLED,
    };
    if key == ACTION_RESTART {
        let _ = server.cmd_tx.send(Command::Start);
    }
    d::HANDLED
}

// ---------------------------------------------------------------------------
// 属性与布局的写出
// ---------------------------------------------------------------------------

const ITEM_XML: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.kde.StatusNotifierItem">
  <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
  <signal name="NewIcon"/><signal name="NewToolTip"/><signal name="NewTitle"/>
  <signal name="NewStatus"><arg name="status" type="s"/></signal>
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  <property name="Menu" type="o" access="read"/>
 </interface>
</node>"#;

const MENU_XML: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="com.canonical.dbusmenu">
  <method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
  <method name="GetGroupProperties"><arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/></method>
  <method name="GetProperty"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
  <method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
  <method name="EventGroup"><arg type="a(isv)" direction="in"/><arg type="u" direction="in"/><arg type="b" direction="out"/></method>
  <method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
  <method name="Ping"><arg type="b" direction="out"/></method>
 </interface>
</node>"#;

unsafe fn write_pixmap(dbus: &DBus, it: *mut DBusMessageIter, server: &Server) {
    let mut arr = match open(dbus, it, d::T_ARRAY, Some(c"(iiay)")) {
        Some(a) => a,
        None => return,
    };
    let mut st = match open(dbus, &mut arr, d::T_STRUCT, None) {
        Some(s) => s,
        None => return,
    };
    put_i32(dbus, &mut st, PIX_W);
    put_i32(dbus, &mut st, PIX_H);
    let mut bytes = match open(dbus, &mut st, d::T_ARRAY, Some(c"y")) {
        Some(b) => b,
        None => return,
    };
    let ptr = server.pixmap.as_ptr() as *const c_void;
    let n = server.pixmap.len() as c_int;
    (dbus.dbus_message_iter_append_fixed_array)(&mut bytes, d::T_BYTE, &ptr as *const _ as *const c_void, n);
    close(dbus, &mut st, &mut bytes);
    close(dbus, &mut arr, &mut st);
    close(dbus, it, &mut arr);
}

const PIX_W: i32 = 32;
const PIX_H: i32 = 32;
/// 图标刷新间隔：分针一秒走 6 度，1 秒足够；再快只是多读 /proc。
const ICON_EVERY: Duration = Duration::from_secs(1);

unsafe fn write_sni_property(dbus: &DBus, it: *mut DBusMessageIter, server: &Server, name: &str) {
    match name {
        "Category" => variant_str(dbus, it, "ApplicationStatus"),
        "Id" => variant_str(dbus, it, "tinyticker"),
        "Title" => variant_str(dbus, it, "TinyTicker"),
        "Status" => variant_str(dbus, it, "Active"),
        "IconName" => variant_str(dbus, it, ""),
        "ItemIsMenu" => variant_bool(dbus, it, true),
        "Menu" => variant_objpath(dbus, it, "/MenuBar"),
        "IconPixmap" => {
            let mut v = match open(dbus, it, d::T_VARIANT, Some(c"a(iiay)")) {
                Some(v) => v,
                None => return,
            };
            write_pixmap(dbus, &mut v, server);
            close(dbus, it, &mut v);
        }
        "ToolTip" => {
            // (sa(iiay)ss)：图标 + 标题 + 正文
            let mut v = match open(dbus, it, d::T_VARIANT, Some(c"(sa(iiay)ss)")) {
                Some(v) => v,
                None => return,
            };
            let mut st = match open(dbus, &mut v, d::T_STRUCT, None) {
                Some(s) => s,
                None => return,
            };
            put_str(dbus, &mut st, "");
            write_pixmap(dbus, &mut st, server);
            put_str(dbus, &mut st, "TinyTicker");
            put_str(dbus, &mut st, "极简悬浮计时器");
            close(dbus, &mut v, &mut st);
            close(dbus, it, &mut v);
        }
        // 兜底也要写一个 variant：属性名由 SNI_PROPERTIES 把关卡，走到这里说明
        // 名单和这段 match 漂了——宁可回空值，也不能让回复缺项而 abort。
        _ => variant_str(dbus, it, ""),
    }
}

unsafe fn reply_props_get(
    dbus: &DBus,
    conn: *mut d::DBusConnection,
    msg: *mut DBusMessage,
    server: &Server,
    name: &str,
) -> c_int {
    if !SNI_PROPERTIES.contains(&name) {
        return reply_error(
            dbus,
            conn,
            msg,
            c"org.freedesktop.DBus.Error.UnknownProperty",
            &format!("未知属性 {name}"),
        );
    }
    reply(dbus, conn, msg, |db, it| {
        write_sni_property(db, it, server, name);
    })
}

/// SNI 属性名单，`Properties.Get` 的认账表与 `GetAll` 的遍历表共用一份。
/// 与上面的 `write_sni_property` 及 `ITEM_XML` 里的 property 列表一一对应。
const SNI_PROPERTIES: &[&str] =
    &["Category", "Id", "Title", "Status", "IconName", "IconPixmap", "ToolTip", "ItemIsMenu", "Menu"];

unsafe fn write_props_all(dbus: &DBus, it: *mut DBusMessageIter, server: &Server) {
    let mut arr = match open(dbus, it, d::T_ARRAY, Some(c"{sv}")) {
        Some(a) => a,
        None => return,
    };
    for name in SNI_PROPERTIES {
        let mut de = match open(dbus, &mut arr, d::T_DICT_ENTRY, None) {
            Some(x) => x,
            None => return,
        };
        put_str(dbus, &mut de, name);
        write_sni_property(dbus, &mut de, server, name);
        close(dbus, &mut arr, &mut de);
    }
    close(dbus, it, &mut arr);
}

/// 菜单节点属性写进一个 `a{sv}`。
/// 把一个节点的属性写成 `a{sv}`。**这个数组必须始终写出来**（未知 id 就写空的）：
/// 外层签名是 `(ia{sv})` / `(ia{sv}av)`，少写一项会让 libdbus 断言失败直接 abort。
unsafe fn write_node_props(dbus: &DBus, it: *mut DBusMessageIter, server: &Server, id: i32) {
    let mut arr = match open(dbus, it, d::T_ARRAY, Some(c"{sv}")) {
        Some(a) => a,
        None => return,
    };
    let node = match server.nodes.get(id as usize) {
        Some(n) => n,
        None => {
            close(dbus, it, &mut arr);
            return;
        }
    };
    // 值为 `None` 表示布尔 true（dbusmenu 只用到 enabled=true 这一种）
    let entries: &[(&str, Option<&str>)] = match node.kind {
        Kind::Separator => &[("type", Some("separator"))],
        Kind::Root => &[("children-display", Some("compound"))],
        Kind::Submenu => &[("label", Some(&node.label)), ("children-display", Some("submenu"))],
        Kind::Button => &[("label", Some(&node.label)), ("enabled", None)],
    };
    for (key, text) in entries {
        let mut de = match open(dbus, &mut arr, d::T_DICT_ENTRY, None) {
            Some(x) => x,
            None => return,
        };
        put_str(dbus, &mut de, key);
        match text {
            Some(text) => variant_str(dbus, &mut de, text),
            None => variant_bool(dbus, &mut de, true),
        }
        close(dbus, &mut arr, &mut de);
    }
    close(dbus, it, &mut arr);
}

unsafe fn write_layout(
    dbus: &DBus,
    it: *mut DBusMessageIter,
    server: &Server,
    id: i32,
    depth: i32,
) {
    let mut st = match open(dbus, it, d::T_STRUCT, None) {
        Some(s) => s,
        None => return,
    };
    put_i32(dbus, &mut st, id);
    write_node_props(dbus, &mut st, server, id);
    let mut kids = match open(dbus, &mut st, d::T_ARRAY, Some(c"v")) {
        Some(k) => k,
        None => return,
    };
    // depth == 1 表示只要本层，不带子节点
    if depth != 1
        && let Some(node) = server.nodes.get(id as usize)
    {
        for child in &node.children {
            let mut v = match open(dbus, &mut kids, d::T_VARIANT, Some(c"(ia{sv}av)")) {
                Some(v) => v,
                None => break,
            };
            write_layout(dbus, &mut v, server, *child, if depth > 0 { depth - 1 } else { 0 });
            close(dbus, &mut kids, &mut v);
        }
    }
    close(dbus, &mut st, &mut kids);
    close(dbus, it, &mut st);
}

unsafe fn write_group_properties(
    dbus: &DBus,
    it: *mut DBusMessageIter,
    server: &Server,
    ids: &[i32],
) {
    let mut arr = match open(dbus, it, d::T_ARRAY, Some(c"(ia{sv})")) {
        Some(a) => a,
        None => return,
    };
    // ids 为空 = 宿主想要全部节点（dbusmenu 的约定）
    let all: Vec<i32> = (0..server.nodes.len() as i32).collect();
    let list: &[i32] = if ids.is_empty() { &all } else { ids };
    for &id in list {
        let mut st = match open(dbus, &mut arr, d::T_STRUCT, None) {
            Some(s) => s,
            None => return,
        };
        put_i32(dbus, &mut st, id);
        write_node_props(dbus, &mut st, server, id);
        close(dbus, &mut arr, &mut st);
    }
    close(dbus, it, &mut arr);
}

unsafe fn write_node_property(
    dbus: &DBus,
    it: *mut DBusMessageIter,
    server: &Server,
    id: i32,
    name: &str,
) {
    let node = match server.nodes.get(id as usize) {
        Some(n) => n,
        None => {
            variant_str(dbus, it, "");
            return;
        }
    };
    match name {
        "label" => variant_str(dbus, it, &node.label),
        "enabled" => variant_bool(dbus, it, node.kind == Kind::Button),
        "visible" => variant_bool(dbus, it, true),
        "children-display" => {
            variant_str(dbus, it, if node.kind == Kind::Submenu { "submenu" } else { "compound" })
        }
        "type" => variant_str(dbus, it, if node.kind == Kind::Separator { "separator" } else { "standard" }),
        _ => variant_str(dbus, it, ""),
    }
}

// ---------------------------------------------------------------------------
// 参数读取
// ---------------------------------------------------------------------------

unsafe fn second_string(dbus: &DBus, args: *mut DBusMessageIter) -> String {
    (dbus.dbus_message_iter_next)(args);
    read_str(dbus, args)
}

unsafe fn read_str(dbus: &DBus, it: *mut DBusMessageIter) -> String {
    let mut p: *const c_char = std::ptr::null();
    if unsafe { (dbus.dbus_message_iter_get_arg_type)(it) } == d::T_STRING {
        unsafe { (dbus.dbus_message_iter_get_basic)(it, &mut p as *mut _ as *mut c_void) };
    }
    cstr(p).unwrap_or_default()
}

unsafe fn read_i32(dbus: &DBus, it: *mut DBusMessageIter) -> i32 {
    let mut v: i32 = 0;
    if unsafe { (dbus.dbus_message_iter_get_arg_type)(it) } == d::T_INT32 {
        unsafe { (dbus.dbus_message_iter_get_basic)(it, &mut v as *mut _ as *mut c_void) };
    }
    v
}

unsafe fn read_layout_request(dbus: &DBus, args: *mut DBusMessageIter) -> (i32, i32) {
    let root = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    (root, read_i32(dbus, args))
}

unsafe fn read_property_request(dbus: &DBus, args: *mut DBusMessageIter) -> (i32, String) {
    let id = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    (id, read_str(dbus, args))
}

/// `Event(i id, s eventId, v data, u timestamp)` → 取 id（仅认 clicked）。
unsafe fn read_event(dbus: &DBus, args: *mut DBusMessageIter) -> Option<i32> {
    let id = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    let event = read_str(dbus, args);
    (event == "clicked").then_some(id)
}

/// `GetGroupProperties(ai ids, as names)` → 读 ids。
unsafe fn read_ids(dbus: &DBus, args: *mut DBusMessageIter) -> Vec<i32> {
    let mut out = Vec::new();
    if unsafe { (dbus.dbus_message_iter_get_arg_type)(args) } != d::T_ARRAY {
        return out;
    }
    let mut sub = DBusMessageIter::uninit();
    unsafe { (dbus.dbus_message_iter_recurse)(args, &mut sub) };
    while unsafe { (dbus.dbus_message_iter_get_arg_type)(&mut sub) } == d::T_INT32 {
        out.push(read_i32(dbus, &mut sub));
        unsafe { (dbus.dbus_message_iter_next)(&mut sub) };
    }
    out
}

// ---------------------------------------------------------------------------
// 写出小工具
// ---------------------------------------------------------------------------

fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// 打开一个容器。`contained_signature` 只对数组和 variant 有意义：
/// libdbus 断言结构体 / dict entry 必须传 `NULL`（元素类型由内容推出），
/// 而数组必须传元素类型、variant 必须传内容类型。传错直接 abort。
unsafe fn open(
    dbus: &DBus,
    it: *mut DBusMessageIter,
    ty: c_int,
    sig: Option<&CStr>,
) -> Option<DBusMessageIter> {
    let mut sub = DBusMessageIter::uninit();
    let sig = sig.map_or(std::ptr::null(), |s| s.as_ptr());
    let ok = unsafe { (dbus.dbus_message_iter_open_container)(it, ty, sig, &mut sub) };
    (ok == d::TRUE).then_some(sub)
}

unsafe fn close(dbus: &DBus, it: *mut DBusMessageIter, sub: &mut DBusMessageIter) {
    unsafe { (dbus.dbus_message_iter_close_container)(it, sub) };
}

unsafe fn put_str(dbus: &DBus, it: *mut DBusMessageIter, s: &str) {
    let c = CString::new(s).unwrap_or_default();
    let p = c.as_ptr();
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_STRING, &p as *const _ as *const c_void) };
}

unsafe fn put_i32(dbus: &DBus, it: *mut DBusMessageIter, v: i32) {
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_INT32, &v as *const _ as *const c_void) };
}

unsafe fn put_bool(dbus: &DBus, it: *mut DBusMessageIter, v: bool) {
    let b: d::DBusBool = v as d::DBusBool;
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_BOOL, &b as *const _ as *const c_void) };
}

unsafe fn variant_str(dbus: &DBus, it: *mut DBusMessageIter, s: &str) {
    let mut v = match open(dbus, it, d::T_VARIANT, Some(c"s")) {
        Some(v) => v,
        None => return,
    };
    put_str(dbus, &mut v, s);
    close(dbus, it, &mut v);
}

unsafe fn variant_bool(dbus: &DBus, it: *mut DBusMessageIter, b: bool) {
    let mut v = match open(dbus, it, d::T_VARIANT, Some(c"b")) {
        Some(v) => v,
        None => return,
    };
    put_bool(dbus, &mut v, b);
    close(dbus, it, &mut v);
}

unsafe fn variant_objpath(dbus: &DBus, it: *mut DBusMessageIter, p: &str) {
    let mut v = match open(dbus, it, d::T_VARIANT, Some(c"o")) {
        Some(v) => v,
        None => return,
    };
    let c = CString::new(p).unwrap_or_default();
    let ptr = c.as_ptr();
    unsafe { (dbus.dbus_message_iter_append_basic)(&mut v, d::T_OBJECT_PATH, &ptr as *const _ as *const c_void) };
    close(dbus, it, &mut v);
}

/// 发一个方法回复；`fill` 负责写入返回值。
unsafe fn reply(
    dbus: &DBus,
    conn: *mut d::DBusConnection,
    call: *mut DBusMessage,
    fill: impl FnOnce(&DBus, *mut DBusMessageIter),
) -> c_int {
    let msg = unsafe { (dbus.dbus_message_new_method_return)(call) };
    if msg.is_null() {
        return d::NOT_YET_HANDLED;
    }
    let mut it = DBusMessageIter::uninit();
    unsafe { (dbus.dbus_message_iter_init_append)(msg, &mut it) };
    fill(dbus, &mut it);
    unsafe {
        (dbus.dbus_connection_send)(conn, msg, std::ptr::null_mut());
        (dbus.dbus_connection_flush)(conn);
        (dbus.dbus_message_unref)(msg);
    }
    d::HANDLED
}

/// 回一个 D-Bus 错误（未知属性等）。
unsafe fn reply_error(dbus: &DBus, conn: *mut d::DBusConnection, call: *mut DBusMessage, name: &CStr, text: &str) -> c_int {
    let msg = unsafe {
        (dbus.dbus_message_new_error)(call, name.as_ptr(), CString::new(text).unwrap_or_default().as_ptr())
    };
    if msg.is_null() {
        return d::NOT_YET_HANDLED;
    }
    unsafe {
        (dbus.dbus_connection_send)(conn, msg, std::ptr::null_mut());
        (dbus.dbus_connection_flush)(conn);
        (dbus.dbus_message_unref)(msg);
    }
    d::HANDLED
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 占用表里水位填充的像素数（排除白色圆环与深色底）。
    fn filled(px: &[u8]) -> usize {
        const RING: (u8, u8, u8) = (255, 255, 255);
        px.chunks(4)
            .filter(|p| p[0] != 0 && !matches!((p[1], p[2], p[3]), RING | DARK))
            .count()
    }

    #[test]
    fn gauge_fills_monotonically() {
        let counts = [0u8, 25, 50, 75, 100].map(|p| filled(&gauge_pixmap(Some(p), false).2));
        assert!(counts.windows(2).all(|w| w[0] < w[1]), "水位应随占用率单调上升: {counts:?}");
        // 100% 时整个内盘被填满（内盘半径 12.5）
        assert!(counts[4] > 450, "满盘像素过少: {}", counts[4]);
        // 0% 只剩水位线那一条缝
        assert!(counts[0] < 10, "0% 不该有明显填充: {}", counts[0]);
        // 25% 的水位在内盘下沿到圆心之间
        assert!(counts[1] > counts[0] && counts[1] < counts[2]);
    }

    #[test]
    fn dial_table_matches_true_trig() {
        // 表本身是定点增量旋转的产物；拿真三角函数对一遍，误差按 1px（半径 12）内算。
        // 只在测试里用 sin/cos——它们不会进 release 二进制，libm 因此仍不在 ldd 里。
        for i in 0..60 {
            let deg = i as f32 * 6.0;
            let ideal = (deg.to_radians().sin(), -deg.to_radians().cos());
            let got = dial_dir(i as f32);
            assert!(
                (got.0 - ideal.0).abs() < 0.01 && (got.1 - ideal.1).abs() < 0.01,
                "第 {i} 格偏了: {got:?} vs {ideal:?}"
            );
            let len = (got.0 * got.0 + got.1 * got.1).sqrt();
            assert!((len - 1.0).abs() < 0.01, "第 {i} 格不是单位向量: {len}");
        }
        // 四个正点方向必须落在轴上
        assert_eq!(dial_dir(0.0), (0.0, -1.0));
        assert!(dial_dir(15.0).0 > 0.99 && dial_dir(15.0).1.abs() < 0.01);
        assert!(dial_dir(30.0).1 > 0.99 && dial_dir(30.0).0.abs() < 0.01);
        assert!(dial_dir(45.0).0 < -0.99 && dial_dir(45.0).1.abs() < 0.01);
        // 插值必须单调：同一象限里序号越大 x 越大
        for i in 0..14 {
            assert!(dial_dir(i as f32 + 0.5).0 > dial_dir(i as f32).0);
        }
    }

    #[test]
    fn empty_battery_shows_hollow_face() {
        // 没有电池的机器选 battery 档：只有表盘，不画饼
        assert_eq!(filled(&gauge_pixmap(None, false).2), 0);
    }

    #[test]
    fn clock_hands_stay_inside_the_face() {
        // 任一时刻都不该越出内盘或 panic（put 不做边界检查，越界即 panic）
        for m in (0..60).chain([59]) {
            for h in [0u32, 3, 6, 9, 12, 18, 23] {
                let px = clock_pixmap((h, m, 0)).2;
                assert_eq!(px.len(), S * S * 4);
            }
        }
    }

    #[test]
    fn icon_mode_names_roundtrip() {
        for m in [IconMode::Clock, IconMode::Cpu, IconMode::Memory, IconMode::Battery] {
            assert_eq!(IconMode::from_name(m.name()), Some(m));
        }
        assert_eq!(IconMode::from_name("disk"), None);
    }
}
