//! StatusNotifierItem 那一侧：属性读写、图标像素送出、注册到宿主、桌面通知与 `ActionInvoked` 过滤。

use std::ffi::{CStr, c_char, c_int, c_void};
use crate::sys::dbus as d;
use crate::sys::dbus::{DBus, DBusError, DBusMessage, DBusMessageIter};
use super::*;

/// `org.kde.StatusNotifierItem` 的属性变更信号。不声明刷新，宿主会一直显示它
/// 启动时缓存的那份——`NewIcon` 与 `NewToolTip` 是同一件事的两个成员。
pub(super) unsafe fn emit_signal(dbus: &DBus, conn: *mut d::DBusConnection, name: &CStr) {
    let msg =
        unsafe { (dbus.dbus_message_new_signal)(ITEM_PATH.as_ptr(), ITEM_IFACE.as_ptr(), name.as_ptr()) };
    if msg.is_null() {
        return;
    }
    (dbus.dbus_connection_send)(conn, msg, std::ptr::null_mut());
    (dbus.dbus_connection_flush)(conn);
    (dbus.dbus_message_unref)(msg);
}

/// `org.freedesktop.Notifications.Notify`：带一个「再来一次」按钮。
pub(super) unsafe fn send_notification(dbus: &DBus, conn: *mut d::DBusConnection, note: &Note) {
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

pub(super) unsafe fn put_u32(dbus: &DBus, it: *mut DBusMessageIter, v: u32) {
    (dbus.dbus_message_iter_append_basic)(it, d::T_UINT32, &v as *const _ as *const c_void);
}

pub(super) unsafe fn register_with_watcher(dbus: &DBus, conn: *mut d::DBusConnection, service: &str) {
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

/// 通知上的按钮被按下。
///
/// `DBusHandleMessageFunction` 的三个参数是 `(connection, message, user_data)`，
/// 与对象路径 vtable 同形，没有 `DBusError`。
pub(super) unsafe extern "C" fn on_filter(
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

pub(super) const ITEM_XML: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
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

pub(super) unsafe fn write_pixmap(dbus: &DBus, it: *mut DBusMessageIter, server: &Server) {
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

pub(super) const PIX_W: i32 = 32;
pub(super) const PIX_H: i32 = 32;
pub(super) unsafe fn write_sni_property(dbus: &DBus, it: *mut DBusMessageIter, server: &Server, name: &str) {
    match name {
        "Category" => variant_str(dbus, it, "ApplicationStatus"),
        "Id" => variant_str(dbus, it, "tinyticker"),
        "Title" => variant_str(dbus, it, "TinyTicker"),
        "Status" => variant_str(dbus, it, "Active"),
        "IconName" => variant_str(dbus, it, ""),
        // 规范里 ItemIsMenu=true 是给「只有右键菜单、没有自己的激活行为」的项用的；
        // 宿主据此把左键也直接吞成弹菜单，Activate 就永远到不了我们这里。既然左键
        // 现在真的有事做（开始/暂停），这里必须报 false。
        "ItemIsMenu" => variant_bool(dbus, it, false),
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
            // 正文是活的：CPU / 内存 / 上下行 / 电池，每秒随采样换一次
            put_str(dbus, &mut st, &server.tooltip.borrow());
            close(dbus, &mut v, &mut st);
            close(dbus, it, &mut v);
        }
        // 兜底也要写一个 variant：属性名由 SNI_PROPERTIES 把关卡，走到这里说明
        // 名单和这段 match 漂了——宁可回空值，也不能让回复缺项而 abort。
        _ => variant_str(dbus, it, ""),
    }
}

pub(super) unsafe fn reply_props_get(
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
pub(super) const SNI_PROPERTIES: &[&str] =
    &["Category", "Id", "Title", "Status", "IconName", "IconPixmap", "ToolTip", "ItemIsMenu", "Menu"];

pub(super) unsafe fn write_props_all(dbus: &DBus, it: *mut DBusMessageIter, server: &Server) {
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
