//! `com.canonical.dbusmenu` 那一侧：布局与属性的写出、点击与滚轮参数的读取。

use crate::sys::dbus::{DBus, DBusMessageIter};
use super::*;

pub(super) const MENU_XML: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
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

/// 菜单节点属性写进一个 `a{sv}`。
/// 把一个节点的属性写成 `a{sv}`。**这个数组必须始终写出来**（未知 id 就写空的）：
/// 外层签名是 `(ia{sv})` / `(ia{sv}av)`，少写一项会让 libdbus 断言失败直接 abort。
/// dbusmenu 节点属性的值：字符串或布尔。
pub(super) enum Prop<'a> {
    Str(&'a str),
    Bool(bool),
}

pub(super) unsafe fn write_node_props(dbus: &DBus, it: *mut DBusMessageIter, server: &Server, id: i32) {
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
    let entries: Vec<(&str, Prop)> = match node.kind {
        Kind::Separator => vec![("type", Prop::Str("separator"))],
        Kind::Root => vec![("children-display", Prop::Str("compound"))],
        Kind::Submenu => vec![
            ("label", Prop::Str(&node.label)),
            ("children-display", Prop::Str("submenu")),
        ],
        Kind::Button => {
            let mut entries = vec![
                ("label", Prop::Str(&node.label)),
                (
                    "enabled",
                    Prop::Bool(
                        node.command.as_ref().is_none_or(|c| server.state.borrow().enabled(c)),
                    ),
                ),
            ];
            // 单选组：把当前状态回灌成勾选，菜单才看得出「现在用的是哪一套」
            if let Some(c) = node.command.as_ref().and_then(Command::check) {
                entries.push(("checked", Prop::Bool(server.state.borrow().checked(c))));
            }
            entries
        }
    };
    for (key, value) in &entries {
        let mut de = match open(dbus, &mut arr, d::T_DICT_ENTRY, None) {
            Some(x) => x,
            None => return,
        };
        put_str(dbus, &mut de, key);
        match value {
            Prop::Str(text) => variant_str(dbus, &mut de, text),
            Prop::Bool(flag) => variant_bool(dbus, &mut de, *flag),
        }
        close(dbus, &mut arr, &mut de);
    }
    close(dbus, it, &mut arr);
}

pub(super) unsafe fn write_layout(
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

pub(super) unsafe fn write_group_properties(
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

pub(super) unsafe fn write_node_property(
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
        "enabled" => variant_bool(
            dbus,
            it,
            node.kind == Kind::Button
                && node.command.as_ref().is_none_or(|c| server.state.borrow().enabled(c)),
        ),
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

pub(super) unsafe fn second_string(dbus: &DBus, args: *mut DBusMessageIter) -> String {
    (dbus.dbus_message_iter_next)(args);
    read_str(dbus, args)
}

pub(super) unsafe fn read_layout_request(dbus: &DBus, args: *mut DBusMessageIter) -> (i32, i32) {
    let root = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    (root, read_i32(dbus, args))
}

pub(super) unsafe fn read_property_request(dbus: &DBus, args: *mut DBusMessageIter) -> (i32, String) {
    let id = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    (id, read_str(dbus, args))
}

/// `Event(i id, s eventId, v data, u timestamp)` → 取 id（仅认 clicked）。
pub(super) unsafe fn read_event(dbus: &DBus, args: *mut DBusMessageIter) -> Option<i32> {
    let id = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    let event = read_str(dbus, args);
    (event == "clicked").then_some(id)
}

/// `Scroll(i delta, s orientation)`：只认纵向，横向暂无可缩放的东西。
/// 规范里 delta 为正 = 向上滚，与悬浮窗滚轮的「上=放大」同向。
pub(super) unsafe fn read_scroll(dbus: &DBus, args: *mut DBusMessageIter) -> Option<i32> {
    let delta = read_i32(dbus, args);
    (dbus.dbus_message_iter_next)(args);
    (read_str(dbus, args) == "vertical").then_some(delta)
}

/// `GetGroupProperties(ai ids, as names)` → 读 ids。
pub(super) unsafe fn read_ids(dbus: &DBus, args: *mut DBusMessageIter) -> Vec<i32> {
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
