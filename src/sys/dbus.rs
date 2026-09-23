//! libdbus-1 的符号声明与 ABI 镜像（运行时 dlopen，不链接）。
//!
//! 结构体布局照 `dbus/dbus.h`：`DBusError` 与 `DBusMessageIter` 的字段我们基本不读，
//! 但**大小必须一致**，因为 libdbus 会往里面写。`dbus_bool_t` 是 32 位无符号整数，
//! 不是 Rust 的 `bool`。

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use super::Lib;

pub const LIB_NAME: &str = "libdbus-1.so.3";

/// 不透明句柄。
pub enum DBusConnection {}
pub enum DBusMessage {}

/// `dbus_bool_t`：32 位无符号整数，**不是** Rust `bool`。
pub type DBusBool = u32;
pub const TRUE: DBusBool = 1;
pub const FALSE: DBusBool = 0;

/// C `DBusError`。
#[repr(C)]
pub struct DBusError {
    pub name: *const c_char,
    pub message: *const c_char,
    _dummy_bits: c_uint,
    _padding: *mut c_void,
}

impl DBusError {
    /// 等价于 C 的 `DBusError err = DBUS_ERROR_INIT`：第三个字段（dummy1）必须是 1，
    /// 那是 libdbus 断言用的占位比特，清零会让部分发行版构建的 libdbus 直接 abort。
    pub fn zeroed() -> Self {
        Self {
            name: std::ptr::null(),
            message: std::ptr::null(),
            _dummy_bits: 1,
            _padding: std::ptr::null_mut(),
        }
    }

    pub fn is_set(&self) -> bool {
        !self.message.is_null()
    }

    pub fn describe(&self) -> String {
        if self.message.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(self.message) }.to_string_lossy().into_owned()
        }
    }
}

/// C `DBusMessageIter`：全是 libdbus 自己的暂存空间，只要布局大小对得上。
#[repr(C)]
pub struct DBusMessageIter {
    _d1: *mut c_void,
    _d2: *mut c_void,
    _d3: u32,
    _d4: c_int,
    _d5: c_int,
    _d6: c_int,
    _d7: c_int,
    _d8: c_int,
    _d9: c_int,
    _d10: c_int,
    _d11: c_int,
    _p1: c_int,
    _p2: c_int,
    _p3: *mut c_void,
}

impl DBusMessageIter {
    pub fn uninit() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// `DBusObjectPathVTable`：两个真实槽位 + 四个保留槽。
#[repr(C)]
pub struct DBusObjectPathVTable {
    pub unregister_function: Option<unsafe extern "C" fn(*mut DBusConnection, *mut c_void)>,
    pub message_function:
        Option<unsafe extern "C" fn(*mut DBusConnection, *mut DBusMessage, *mut c_void) -> c_int>,
    _r1: Option<unsafe extern "C" fn(*mut c_void)>,
    _r2: Option<unsafe extern "C" fn(*mut c_void)>,
    _r3: Option<unsafe extern "C" fn(*mut c_void)>,
    _r4: Option<unsafe extern "C" fn(*mut c_void)>,
}

impl DBusObjectPathVTable {
    pub fn new(msg: unsafe extern "C" fn(*mut DBusConnection, *mut DBusMessage, *mut c_void) -> c_int) -> Self {
        Self {
            unregister_function: None,
            message_function: Some(msg),
            _r1: None,
            _r2: None,
            _r3: None,
            _r4: None,
        }
    }
}

// 总线类型
pub const BUS_SESSION: c_int = 0;
// 消息类型
pub const MSG_METHOD_CALL: c_int = 1;
pub const MSG_METHOD_RETURN: c_int = 2;
pub const MSG_ERROR: c_int = 3;
pub const MSG_SIGNAL: c_int = 4;
// 类型码就是签名字符的 ASCII 值
pub const T_BYTE: c_int = b'y' as c_int;
pub const T_BOOL: c_int = b'b' as c_int;
pub const T_INT32: c_int = b'i' as c_int;
pub const T_UINT32: c_int = b'u' as c_int;
pub const T_STRING: c_int = b's' as c_int;
pub const T_OBJECT_PATH: c_int = b'o' as c_int;
pub const T_SIGNATURE: c_int = b'g' as c_int;
pub const T_ARRAY: c_int = b'a' as c_int;
pub const T_VARIANT: c_int = b'v' as c_int;
pub const T_STRUCT: c_int = b'r' as c_int;
pub const T_DICT_ENTRY: c_int = b'e' as c_int;
// 处理器返回值
pub const HANDLED: c_int = 0;
pub const NOT_YET_HANDLED: c_int = 1;
// 名字申请
pub const NAME_FLAG_DO_NOT_QUEUE: c_uint = 4;
pub const REQUEST_REPLY_PRIMARY_OWNER: c_int = 1;
pub const REQUEST_REPLY_ALREADY_OWNER: c_int = 4;
// 超时
pub const TIMEOUT_INFINITE: c_int = 0x7fff_ffff;
pub const TIMEOUT_DEFAULT: c_int = -1;

/// 生成 `DBus` 绑定表：一个字段一个 `dbus_*` 函数，`load` 里逐个 dlsym。
macro_rules! dbus_bindings {
    ( $( fn $name:ident ( $($arg:ty),* $(,)? ) $( -> $ret:ty )? ; )+ ) => {
        #[allow(non_snake_case)]
        pub struct DBus {
            $( pub $name: unsafe extern "C" fn( $($arg),* ) $( -> $ret )?, )+
            /// 必须最后声明：库映射要活得比函数指针久（我们不 dlclose）。
            _lib: Lib,
        }

        impl DBus {
            /// 打开 libdbus-1 并解析全部符号；任一符号缺失（或库不在）返回 `None`。
            pub fn load() -> Option<Self> {
                let lib = Lib::open(LIB_NAME)?;
                Some(Self {
                    $( $name: lib.func(stringify!($name))?, )+
                    _lib: lib,
                })
            }
        }
    };
}

dbus_bindings! {
    fn dbus_bus_get(c_int, *mut DBusError) -> *mut DBusConnection;
    fn dbus_bus_get_unique_name(*mut DBusConnection) -> *const c_char;
    fn dbus_connection_flush(*mut DBusConnection);
    fn dbus_connection_read_write_dispatch(*mut DBusConnection, c_int) -> DBusBool;
    fn dbus_connection_add_filter(*mut DBusConnection, Option<unsafe extern "C" fn(*mut DBusConnection, *mut DBusMessage, *mut c_void) -> c_int>, *mut c_void, Option<unsafe extern "C" fn(*mut c_void)>) -> DBusBool;
    fn dbus_bus_add_match(*mut DBusConnection, *const c_char, *mut DBusError);
    fn dbus_connection_send(*mut DBusConnection, *mut DBusMessage, *mut u32) -> DBusBool;
    fn dbus_connection_send_with_reply_and_block(
        *mut DBusConnection, *mut DBusMessage, c_int, *mut DBusError
    ) -> *mut DBusMessage;
    fn dbus_connection_try_register_object_path(
        *mut DBusConnection, *const c_char, *const DBusObjectPathVTable, *mut c_void, *mut DBusError
    ) -> DBusBool;
    fn dbus_message_new_method_call(
        *const c_char, *const c_char, *const c_char, *const c_char
    ) -> *mut DBusMessage;
    fn dbus_message_new_method_return(*mut DBusMessage) -> *mut DBusMessage;
    fn dbus_message_unref(*mut DBusMessage);
    fn dbus_message_get_type(*mut DBusMessage) -> c_int;
    fn dbus_message_get_member(*mut DBusMessage) -> *const c_char;
    fn dbus_message_get_interface(*mut DBusMessage) -> *const c_char;
    fn dbus_message_get_path(*mut DBusMessage) -> *const c_char;
    fn dbus_message_is_signal(*mut DBusMessage, *const c_char, *const c_char) -> DBusBool;
    fn dbus_message_iter_init(*mut DBusMessage, *mut DBusMessageIter) -> DBusBool;
    fn dbus_message_iter_init_append(*mut DBusMessage, *mut DBusMessageIter);
    fn dbus_message_iter_append_basic(*mut DBusMessageIter, c_int, *const c_void) -> DBusBool;
    fn dbus_message_iter_open_container(
        *mut DBusMessageIter, c_int, *const c_char, *mut DBusMessageIter
    ) -> DBusBool;
    fn dbus_message_iter_close_container(*mut DBusMessageIter, *mut DBusMessageIter) -> DBusBool;
    fn dbus_message_iter_get_arg_type(*mut DBusMessageIter) -> c_int;
    fn dbus_message_iter_recurse(*mut DBusMessageIter, *mut DBusMessageIter);
    fn dbus_message_iter_get_basic(*mut DBusMessageIter, *mut c_void);
    fn dbus_message_iter_append_fixed_array(*mut DBusMessageIter, c_int, *const c_void, c_int) -> DBusBool;
    fn dbus_message_iter_next(*mut DBusMessageIter) -> DBusBool;
    fn dbus_message_new_error(*mut DBusMessage, *const c_char, *const c_char) -> *mut DBusMessage;
}
