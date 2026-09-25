//! libdbus 迭代器的小工具：开闭容器、放基本类型与变体、读写字符串、打包回复。

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use crate::sys::dbus as d;
use crate::sys::dbus::{DBus, DBusMessage, DBusMessageIter};

pub(super) unsafe fn read_str(dbus: &DBus, it: *mut DBusMessageIter) -> String {
    let mut p: *const c_char = std::ptr::null();
    if unsafe { (dbus.dbus_message_iter_get_arg_type)(it) } == d::T_STRING {
        unsafe { (dbus.dbus_message_iter_get_basic)(it, &mut p as *mut _ as *mut c_void) };
    }
    cstr(p).unwrap_or_default()
}

pub(super) unsafe fn read_i32(dbus: &DBus, it: *mut DBusMessageIter) -> i32 {
    let mut v: i32 = 0;
    if unsafe { (dbus.dbus_message_iter_get_arg_type)(it) } == d::T_INT32 {
        unsafe { (dbus.dbus_message_iter_get_basic)(it, &mut v as *mut _ as *mut c_void) };
    }
    v
}

pub(super) fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// 打开一个容器。`contained_signature` 只对数组和 variant 有意义：
/// libdbus 断言结构体 / dict entry 必须传 `NULL`（元素类型由内容推出），
/// 而数组必须传元素类型、variant 必须传内容类型。传错直接 abort。
pub(super) unsafe fn open(
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

pub(super) unsafe fn close(dbus: &DBus, it: *mut DBusMessageIter, sub: &mut DBusMessageIter) {
    unsafe { (dbus.dbus_message_iter_close_container)(it, sub) };
}

pub(super) unsafe fn put_str(dbus: &DBus, it: *mut DBusMessageIter, s: &str) {
    let c = CString::new(s).unwrap_or_default();
    let p = c.as_ptr();
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_STRING, &p as *const _ as *const c_void) };
}

pub(super) unsafe fn put_i32(dbus: &DBus, it: *mut DBusMessageIter, v: i32) {
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_INT32, &v as *const _ as *const c_void) };
}

pub(super) unsafe fn put_bool(dbus: &DBus, it: *mut DBusMessageIter, v: bool) {
    let b: d::DBusBool = v as d::DBusBool;
    unsafe { (dbus.dbus_message_iter_append_basic)(it, d::T_BOOL, &b as *const _ as *const c_void) };
}

pub(super) unsafe fn variant_str(dbus: &DBus, it: *mut DBusMessageIter, s: &str) {
    let mut v = match open(dbus, it, d::T_VARIANT, Some(c"s")) {
        Some(v) => v,
        None => return,
    };
    put_str(dbus, &mut v, s);
    close(dbus, it, &mut v);
}

pub(super) unsafe fn variant_bool(dbus: &DBus, it: *mut DBusMessageIter, b: bool) {
    let mut v = match open(dbus, it, d::T_VARIANT, Some(c"b")) {
        Some(v) => v,
        None => return,
    };
    put_bool(dbus, &mut v, b);
    close(dbus, it, &mut v);
}

pub(super) unsafe fn variant_objpath(dbus: &DBus, it: *mut DBusMessageIter, p: &str) {
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
pub(super) unsafe fn reply(
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
pub(super) unsafe fn reply_error(dbus: &DBus, conn: *mut d::DBusConnection, call: *mut DBusMessage, name: &CStr, text: &str) -> c_int {
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
