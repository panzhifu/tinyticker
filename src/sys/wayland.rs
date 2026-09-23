//! libwayland-client 的符号声明与接口描述符。
//!
//! 核心协议（wl_surface / wl_shm / wl_pointer …）的 `wl_interface` 描述符由
//! libwayland-client.so 自己导出，dlsym 取用即可。扩展协议
//! （layer-shell / viewporter / fractional-scale / relative-pointer / cursor-shape）
//! 不在 libwayland 里，需要按协议 XML 的签名与 opcode 顺序自己摆表——
//! libwayland 靠这张表做序列化，签名写错就是协议错乱，所以逐条对照 XML 生成。
//! 表在加载时建一次并泄漏（进程常驻，libwayland 会一直持有指针）。

use std::ffi::{CStr, c_char, c_int, c_void};

use super::Lib;

/// wl_* 对象在 C 侧都是不透明类型，这里统一用裸指针表示。
pub type Obj = *mut c_void;

/// `struct wl_message`（见 wayland-util.h）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WlMessage {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub types: *const *const WlInterface,
}

/// `struct wl_interface`（见 wayland-util.h）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WlInterface {
    pub name: *const c_char,
    pub version: c_int,
    pub method_count: c_int,
    pub methods: *const WlMessage,
    pub event_count: c_int,
    pub events: *const WlMessage,
}

/// `union wl_argument`：定长 8 字节槽位，按签名字符逐个填充。
#[repr(C)]
#[derive(Clone, Copy)]
pub union WlArgument {
    pub i: i32,
    pub u: u32,
    /// wl_fixed_t 就是 int32_t，单位 1/256
    pub f: i32,
    pub o: *mut c_void,
    pub n: *const c_char,
    pub a: *const c_void,
    pub h: i32,
}

impl WlArgument {
    /// new_id 槽占位：传 NIL 表示请库创建新对象并回填。
    pub const NIL: Self = Self { o: std::ptr::null_mut() };

    pub fn int(v: i32) -> Self {
        Self { i: v }
    }

    pub fn uint(v: u32) -> Self {
        Self { u: v }
    }

    pub fn obj(v: Obj) -> Self {
        Self { o: v }
    }

    pub fn cstr(v: *const c_char) -> Self {
        Self { n: v }
    }

    pub fn fd(v: i32) -> Self {
        Self { h: v }
    }
}

pub type MarshalArray = unsafe extern "C" fn(Obj, u32, *mut WlArgument) -> c_int;
pub type MarshalCtor =
    unsafe extern "C" fn(Obj, u32, *mut WlArgument, *const WlInterface, u32) -> Obj;

/// 用到的 libwayland-client 函数集合。
///
/// 探针阶段只用到其中一部分，事件循环与输入接上后会全部用上。
#[allow(dead_code)]
pub struct Wl {
    pub connect: unsafe extern "C" fn(*const c_char) -> Obj,
    pub disconnect: unsafe extern "C" fn(Obj),
    pub get_fd: unsafe extern "C" fn(Obj) -> c_int,
    pub prepare_read: unsafe extern "C" fn(Obj) -> c_int,
    pub read_events: unsafe extern "C" fn(Obj) -> c_int,
    pub cancel_read: unsafe extern "C" fn(Obj),
    pub dispatch_pending: unsafe extern "C" fn(Obj) -> c_int,
    pub flush: unsafe extern "C" fn(Obj) -> c_int,
    pub get_error: unsafe extern "C" fn(Obj) -> c_int,
    pub roundtrip: unsafe extern "C" fn(Obj) -> c_int,
    pub marshal: MarshalArray,
    pub marshal_ctor: MarshalCtor,
    pub add_listener: unsafe extern "C" fn(Obj, *mut unsafe extern "C" fn(), *mut c_void) -> c_int,
    pub destroy: unsafe extern "C" fn(Obj),
    pub proxy_version: unsafe extern "C" fn(Obj) -> u32,
}

impl Wl {
    /// dlopen libwayland-client 并取齐符号；任一缺失返回 `None`。
    pub fn load() -> Option<Self> {
        let lib = Lib::open("libwayland-client.so.0")?;
        Some(Self {
            connect: lib.func("wl_display_connect")?,
            disconnect: lib.func("wl_display_disconnect")?,
            get_fd: lib.func("wl_display_get_fd")?,
            prepare_read: lib.func("wl_display_prepare_read")?,
            read_events: lib.func("wl_display_read_events")?,
            cancel_read: lib.func("wl_display_cancel_read")?,
            dispatch_pending: lib.func("wl_display_dispatch_pending")?,
            flush: lib.func("wl_display_flush")?,
            get_error: lib.func("wl_display_get_error")?,
            roundtrip: lib.func("wl_display_roundtrip")?,
            marshal: lib.func("wl_proxy_marshal_array")?,
            marshal_ctor: lib.func("wl_proxy_marshal_array_constructor_versioned")?,
            add_listener: lib.func("wl_proxy_add_listener")?,
            destroy: lib.func("wl_proxy_destroy")?,
            proxy_version: lib.func("wl_proxy_get_version")?,
        })
    }
}

fn leak_ptr<T: Sized>(v: Vec<T>) -> *const T {
    if v.is_empty() {
        std::ptr::null()
    } else {
        Box::leak(v.into_boxed_slice()).as_ptr()
    }
}

fn msg(
    name: &'static CStr,
    signature: &'static CStr,
    types: Vec<*const WlInterface>,
) -> WlMessage {
    WlMessage {
        name: name.as_ptr(),
        signature: signature.as_ptr(),
        types: leak_ptr(types),
    }
}

fn iface(
    name: &'static CStr,
    version: c_int,
    methods: Vec<WlMessage>,
    events: Vec<WlMessage>,
) -> &'static WlInterface {
    // 长度必须在泄漏成裸指针之前从 Vec 拿：libwayland 靠它校验 opcode 上界
    let (mc, ec) = (methods.len() as c_int, events.len() as c_int);
    let (m, e) = (leak_ptr(methods), leak_ptr(events));
    Box::leak(Box::new(WlInterface {
        name: name.as_ptr(),
        version,
        method_count: mc,
        methods: m,
        event_count: ec,
        events: e,
    }))
}

/// 只被 types 数组引用、从不调用的接口占位符（get_popup / 平板工具）。
/// libwayland 只在序列化对应参数时才解引用它，给个同名空表即可。
fn placeholder(name: &'static CStr) -> *const WlInterface {
    iface(name, 1, Vec::new(), Vec::new())
}

/// 接口描述符集合：核心的从 .so 取，扩展的自己摆表。
///
/// 探针阶段尚未用到全部描述符（seat/region/relative-pointer/cursor-shape 等），
/// 正式客户端会逐个用上。
#[allow(dead_code)]
pub struct Ifaces {
    pub registry: *const WlInterface,
    pub compositor: *const WlInterface,
    pub shm: *const WlInterface,
    pub shm_pool: *const WlInterface,
    pub buffer: *const WlInterface,
    pub surface: *const WlInterface,
    pub region: *const WlInterface,
    pub seat: *const WlInterface,
    pub pointer: *const WlInterface,
    pub output: *const WlInterface,
    pub callback: *const WlInterface,

    pub layer_shell: *const WlInterface,
    pub layer_surface: *const WlInterface,
    pub viewporter: *const WlInterface,
    pub viewport: *const WlInterface,
    pub fractional_manager: *const WlInterface,
    pub fractional_scale: *const WlInterface,
    pub relative_manager: *const WlInterface,
    pub relative_pointer: *const WlInterface,
    pub cursor_manager: *const WlInterface,
    pub cursor_device: *const WlInterface,
}

impl Ifaces {
    pub fn load(lib: &Lib) -> Option<Self> {
        let core = |n: &str| lib.data::<WlInterface>(n);
        let surface = core("wl_surface_interface")? as *const WlInterface;
        let output = core("wl_output_interface")? as *const WlInterface;
        let pointer = core("wl_pointer_interface")? as *const WlInterface;

        // ---- 扩展协议：签名与顺序逐条对照协议 XML ----
        let xdg_popup = placeholder(c"xdg_popup");
        let tablet_tool = placeholder(c"zwp_tablet_tool_v2");

        let layer_surface = iface(
            c"zwlr_layer_surface_v1",
            5,
            vec![
                msg(c"set_size", c"uu", vec![]),
                msg(c"set_anchor", c"u", vec![]),
                msg(c"set_exclusive_zone", c"i", vec![]),
                msg(c"set_margin", c"iiii", vec![]),
                msg(c"set_keyboard_interactivity", c"u", vec![]),
                msg(c"get_popup", c"o", vec![xdg_popup]),
                msg(c"ack_configure", c"u", vec![]),
                msg(c"destroy", c"", vec![]),
                msg(c"set_layer", c"u", vec![]),
                msg(c"set_exclusive_edge", c"u", vec![]),
            ],
            vec![msg(c"configure", c"uuu", vec![]), msg(c"closed", c"", vec![])],
        );
        let layer_shell = iface(
            c"zwlr_layer_shell_v1",
            5,
            vec![
                msg(
                    c"get_layer_surface",
                    c"no?ous",
                    vec![layer_surface, surface, output],
                ),
                msg(c"destroy", c"", vec![]),
            ],
            Vec::new(),
        );

        let viewport = iface(
            c"wp_viewport",
            1,
            vec![
                msg(c"destroy", c"", vec![]),
                msg(c"set_source", c"ffff", vec![]),
                msg(c"set_destination", c"ii", vec![]),
            ],
            Vec::new(),
        );
        let viewporter = iface(
            c"wp_viewporter",
            1,
            vec![
                msg(c"destroy", c"", vec![]),
                msg(c"get_viewport", c"no", vec![viewport, surface]),
            ],
            Vec::new(),
        );

        let fractional_scale = iface(
            c"wp_fractional_scale_v1",
            1,
            vec![msg(c"destroy", c"", vec![])],
            vec![msg(c"preferred_scale", c"u", vec![])],
        );
        let fractional_manager = iface(
            c"wp_fractional_scale_manager_v1",
            1,
            vec![
                msg(c"destroy", c"", vec![]),
                msg(c"get_fractional_scale", c"no", vec![fractional_scale, surface]),
            ],
            Vec::new(),
        );

        let relative_pointer = iface(
            c"zwp_relative_pointer_v1",
            1,
            vec![msg(c"destroy", c"", vec![])],
            vec![msg(c"relative_motion", c"uuffff", vec![])],
        );
        let relative_manager = iface(
            c"zwp_relative_pointer_manager_v1",
            1,
            vec![
                msg(c"destroy", c"", vec![]),
                msg(c"get_relative_pointer", c"no", vec![relative_pointer, pointer]),
            ],
            Vec::new(),
        );

        let cursor_device = iface(
            c"wp_cursor_shape_device_v1",
            2,
            vec![msg(c"destroy", c"", vec![]), msg(c"set_shape", c"uu", vec![])],
            Vec::new(),
        );
        let cursor_manager = iface(
            c"wp_cursor_shape_manager_v1",
            2,
            vec![
                msg(c"destroy", c"", vec![]),
                msg(c"get_pointer", c"no", vec![cursor_device, pointer]),
                msg(c"get_tablet_tool_v2", c"no", vec![cursor_device, tablet_tool]),
            ],
            Vec::new(),
        );

        Some(Self {
            registry: core("wl_registry_interface")?,
            compositor: core("wl_compositor_interface")?,
            shm: core("wl_shm_interface")?,
            shm_pool: core("wl_shm_pool_interface")?,
            buffer: core("wl_buffer_interface")?,
            surface,
            region: core("wl_region_interface")?,
            seat: core("wl_seat_interface")?,
            pointer,
            output,
            callback: core("wl_callback_interface")?,
            layer_shell,
            layer_surface,
            viewporter,
            viewport,
            fractional_manager,
            fractional_scale,
            relative_manager,
            relative_pointer,
            cursor_manager,
            cursor_device,
        })
    }
}
