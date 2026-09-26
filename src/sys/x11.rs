//! libX11 / libXext 的符号声明与 ABI 镜像（运行时 dlopen，不链接）。
//!
//! 结构体布局逐字段照 `X11/Xlib.h`、`X11/Xutil.h`、`X11/extensions/shape.h`：
//! `XID`（Window / Colormap / Cursor / Drawable）在 64 位上是 `unsigned long`，
//! 不是 32 位整数；`Bool` 是 `int`；`XEvent` 是个 24 个 long 的 union。
//! 这三处任何一处写错都会让 Xlib 往我们的内存里越界写。

use std::ffi::{c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_void};

use super::Lib;

pub const LIB_NAME: &str = "libX11.so.6";
pub const EXT_LIB_NAME: &str = "libXext.so.6";

/// 不透明句柄：只用指针，从不解引用（XImage 例外，见 [`ImageHdr`]）。
pub enum Display {}
pub enum Visual {}
pub enum GC {}

pub type Window = c_ulong;
pub type Drawable = c_ulong;
pub type Colormap = c_ulong;
pub type Cursor = c_ulong;
pub type Pixmap = c_ulong;
pub type Time = c_ulong;

/// `XSetWindowAttributes`：字段顺序与宽度照 Xlib.h。
#[repr(C)]
#[derive(Default)]
pub struct SetWindowAttributes {
    pub background_pixmap: Pixmap,
    pub background_pixel: c_ulong,
    pub border_pixmap: Pixmap,
    pub border_pixel: c_ulong,
    pub bit_gravity: c_int,
    pub win_gravity: c_int,
    pub backing_store: c_int,
    pub backing_planes: c_ulong,
    pub backing_pixel: c_ulong,
    pub save_under: c_int,
    pub event_mask: c_long,
    pub do_not_propagate_mask: c_long,
    pub override_redirect: c_int,
    pub colormap: Colormap,
    pub cursor: Cursor,
}

/// `XVisualInfo`。
#[repr(C)]
pub struct VisualInfo {
    pub visual: *mut Visual,
    pub visualid: c_ulong,
    pub screen: c_int,
    pub depth: c_int,
    pub class: c_int,
    pub red_mask: c_ulong,
    pub green_mask: c_ulong,
    pub blue_mask: c_ulong,
    pub colormap_size: c_int,
    pub bits_per_rgb: c_int,
}

/// `XEvent`：union，只有 `long pad[24]` 这个成员是我们需要的（192 字节）。
/// 具体事件用 [`event`] 按指针视图取。
#[repr(C)]
pub struct Event {
    pub pad: [c_long; 24],
}

impl Event {
    /// `type` 字段（低 32 位；最高位是 send_event 标志，已按 X 惯例掩掉）。
    pub fn kind(&self) -> c_int {
        let raw = self as *const Self as *const c_int;
        // 高位是 SendEvent 标志，Xlib 一般已清掉，这里再掩一次
        (unsafe { *raw }) & 0x7f
    }
}

/// 把 union 当成某个具体事件结构读。
pub fn event<T>(ev: &Event) -> &T {
    unsafe { &*(ev as *const Event as *const T) }
}

/// `XAnyEvent`：所有事件共有的前缀。
#[repr(C)]
pub struct AnyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub window: Window,
}

/// `XButtonEvent`（`XKeyEvent` 同布局）。
#[repr(C)]
pub struct ButtonEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub button: c_uint,
    pub same_screen: c_int,
}

/// `XMotionEvent`：`detail` 在 C 里是 `char`，位置与 button 相同但宽度不同，
/// 我们只取到 `state` 为止，故一并照抄。
#[repr(C)]
pub struct MotionEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub is_hint: c_char,
    pub same_screen: c_int,
}

/// `XKeyEvent`：与 `XButtonEvent` 同布局，末两格是 keycode / same_screen。
#[repr(C)]
pub struct KeyEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub window: Window,
    pub root: Window,
    pub subwindow: Window,
    pub time: Time,
    pub x: c_int,
    pub y: c_int,
    pub x_root: c_int,
    pub y_root: c_int,
    pub state: c_uint,
    pub keycode: c_uint,
    pub same_screen: c_int,
}

/// `XConfigureEvent`。
#[repr(C)]
pub struct ConfigureEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub event: Window,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub border_width: c_int,
    pub above: Window,
    pub override_redirect: c_int,
}

/// `XExposeEvent`。
#[repr(C)]
pub struct ExposeEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub window: Window,
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub count: c_int,
}

/// `XMapEvent` / `XUnmapEvent` / `XReparentEvent` / `XCreateWindowEvent` 的公共前缀。
/// 注意这类事件在 `window` 之前还多一个 `event` / `parent` 字段——偏移 32 那里
/// 装的是父窗口，真正的目标窗口在 40，用 [`AnyEvent`] 去读会读错。
#[repr(C)]
pub struct ParentWindowEvent {
    pub type_: c_int,
    pub serial: c_ulong,
    pub send_event: c_int,
    pub display: *mut Display,
    pub parent: Window,
    pub window: Window,
}

/// `XImage` 的头部：到 `blue_mask` 为止，字段与顺序照 `struct _XImage`。
/// 尾部的函数表我们从不碰（也因此不能调 `XDestroyImage`，那是个宏）。
#[repr(C)]
pub struct ImageHdr {
    pub width: c_int,
    pub height: c_int,
    pub xoffset: c_int,
    pub format: c_int,
    pub data: *mut c_char,
    pub byte_order: c_int,
    pub bitmap_unit: c_int,
    pub bitmap_bit_order: c_int,
    pub bitmap_pad: c_int,
    pub depth: c_int,
    pub bytes_per_line: c_int,
    pub bits_per_pixel: c_int,
    pub red_mask: c_ulong,
    pub green_mask: c_ulong,
    pub blue_mask: c_ulong,
}

/// `XErrorEvent`：X 的请求是异步的，出错只在这条事件里回报。
#[repr(C)]
pub struct ErrorEvent {
    pub type_: c_int,
    pub display: *mut Display,
    pub resourceid: c_ulong,
    pub serial: c_ulong,
    pub error_code: c_uchar,
    pub request_code: c_uchar,
    pub minor_code: c_uchar,
}

/// X 的错误码（`X.h` 的 `BadMatch` 等）。
pub fn error_name(code: c_uchar) -> &'static str {
    match code {
        1 => "BadRequest",
        2 => "BadValue",
        3 => "BadWindow",
        4 => "BadPixmap",
        5 => "BadAtom",
        6 => "BadCursor",
        7 => "BadFont",
        8 => "BadMatch",
        9 => "BadDrawable",
        10 => "BadAccess",
        11 => "BadAlloc",
        12 => "BadColor",
        13 => "BadGC",
        14 => "BadIDRange",
        15 => "BadImplementation",
        _ => "未知错误",
    }
}

/// `XRectangle`（Xproto.h：四个 short）。
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rectangle {
    pub x: c_short,
    pub y: c_short,
    pub width: c_short,
    pub height: c_short,
}

// —— 窗口与视觉 ——
pub const INPUT_OUTPUT: c_int = 1;
pub const TRUE_COLOR: c_int = 4;
/// 核心光标字体里的左箭头（`cursorfont.h` 的 `XC_left_ptr`）。
pub const XC_LEFT_PTR: c_uint = 68;
pub const ALLOC_NONE: c_int = 0;
pub const COPY_FROM_PARENT: c_ulong = 0;
/// `background_pixmap = None`：不继承父窗口背景，改用 `background_pixel`。
/// 必须连同 `CW_BACK_PIXMAP` 一起显式给出——不指定的话服务器按 CopyFromParent
/// 处理，而 32 位子窗口的父根窗是 24 位，直接 BadMatch。
pub const NONE_PIXMAP: c_ulong = 0;
pub const DEPTH_FROM_PARENT: c_int = 0;
// —— XCreateWindow 的属性位 ——
pub const CW_BACK_PIXMAP: c_ulong = 1 << 0;
pub const CW_BACK_PIXEL: c_ulong = 1 << 1;
pub const CW_BORDER_PIXMAP: c_ulong = 1 << 2;
pub const CW_BORDER_PIXEL: c_ulong = 1 << 3;
pub const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
pub const CW_EVENT_MASK: c_ulong = 1 << 11;
pub const CW_COLORMAP: c_ulong = 1 << 13;
pub const CW_CURSOR: c_ulong = 1 << 14;
// —— 事件掩码 ——
pub const KEY_PRESS_MASK: c_long = 1 << 0;
pub const BUTTON_PRESS_MASK: c_long = 1 << 2;
pub const BUTTON_RELEASE_MASK: c_long = 1 << 3;
pub const POINTER_MOTION_MASK: c_long = 1 << 6;
pub const BUTTON_MOTION_MASK: c_long = 1 << 13;
pub const EXPOSURE_MASK: c_long = 1 << 15;
pub const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
pub const SUBSTRUCTURE_NOTIFY_MASK: c_long = 1 << 19;
// —— 事件类型 ——
pub const KEY_PRESS: c_int = 2;
pub const KEY_RELEASE: c_int = 3;
pub const BUTTON_PRESS: c_int = 4;
pub const BUTTON_RELEASE: c_int = 5;
pub const MOTION_NOTIFY: c_int = 6;
pub const EXPOSE: c_int = 12;
pub const CREATE_NOTIFY: c_int = 16;
pub const DESTROY_NOTIFY: c_int = 17;
pub const UNMAP_NOTIFY: c_int = 18;
pub const MAP_NOTIFY: c_int = 19;
pub const REPARENT_NOTIFY: c_int = 21;
pub const CONFIGURE_NOTIFY: c_int = 22;
// —— 按键编号（X 的滚轮就是按钮 4/5） ——
pub const BTN_1: c_uint = 1;
pub const BTN_2: c_uint = 2;
pub const BTN_3: c_uint = 3;
pub const BTN_WHEEL_UP: c_uint = 4;
pub const BTN_WHEEL_DOWN: c_uint = 5;
// —— 指针 grab ——
pub const GRAB_MODE_ASYNC: c_int = 1;
pub const CURRENT_TIME: Time = 0;
// —— 键符号（keysymdef.h 里输入行用得到的几个特殊键；普通字符走 XLookupString）——
pub const XK_BACKSPACE: c_ulong = 0xff08;
pub const XK_TAB: c_ulong = 0xff09;
pub const XK_RETURN: c_ulong = 0xff0d;
pub const XK_ESCAPE: c_ulong = 0xff1b;
pub const XK_KP_ENTER: c_ulong = 0xff8d;
/// `KeySym` 就是 `unsigned long`。
pub type KeySym = c_ulong;
// —— XCreateImage ——
pub const Z_PIXMAP: c_int = 2;
// —— Shape extension ——
pub const SHAPE_BOUNDING: c_int = 0;
pub const SHAPE_INPUT: c_int = 2;
pub const SHAPE_SET: c_int = 0;

/// 生成函数表：一个字段一个符号，`load` 里逐个 dlsym。
macro_rules! bindings {
    ($table:ident, $name:expr; $( fn $f:ident ( $($arg:ty),* $(,)? ) $( -> $ret:ty )? ; )+) => {
        #[allow(non_snake_case)]
        pub struct $table {
            $( pub $f: unsafe extern "C" fn( $($arg),* ) $( -> $ret )?, )+
            /// 必须最后声明：库映射要活得比函数指针久（我们不 dlclose）。
            _lib: Lib,
        }

        impl $table {
            /// 打开库并解析全部符号；库不在或符号缺失返回 `None`。
            pub fn load() -> Option<Self> {
                let lib = Lib::open($name)?;
                Some(Self {
                    $( $f: lib.func(stringify!($f))?, )+
                    _lib: lib,
                })
            }
        }
    };
}

bindings! { X11, "libX11.so.6";
    fn XOpenDisplay(*const c_char) -> *mut Display;
    fn XCloseDisplay(*mut Display) -> c_int;
    fn XConnectionNumber(*mut Display) -> c_int;
    fn XDefaultScreen(*mut Display) -> c_int;
    fn XDefaultDepth(*mut Display, c_int) -> c_int;
    fn XDefaultRootWindow(*mut Display) -> Window;
    fn XDefaultVisual(*mut Display, c_int) -> *mut Visual;
    fn XDisplayWidth(*mut Display, c_int) -> c_int;
    fn XDisplayHeight(*mut Display, c_int) -> c_int;
    fn XResourceManagerString(*mut Display) -> *mut c_char;
    fn XMatchVisualInfo(*mut Display, c_int, c_int, c_int, *mut VisualInfo) -> c_int;
    fn XCreateColormap(*mut Display, Window, *mut Visual, c_int) -> Colormap;
    fn XCreateFontCursor(*mut Display, c_uint) -> Cursor;
    fn XCreateWindow(
        *mut Display, Window, c_int, c_int, c_uint, c_uint, c_uint, c_int, c_uint,
        *mut Visual, c_ulong, *mut SetWindowAttributes
    ) -> Window;
    fn XStoreName(*mut Display, Window, *const c_char) -> c_int;
    fn XSelectInput(*mut Display, Window, c_long) -> c_int;
    fn XMapRaised(*mut Display, Window) -> c_int;
    fn XUnmapWindow(*mut Display, Window) -> c_int;
    fn XMoveWindow(*mut Display, Window, c_int, c_int) -> c_int;
    fn XResizeWindow(*mut Display, Window, c_uint, c_uint) -> c_int;
    fn XRaiseWindow(*mut Display, Window) -> c_int;
    fn XSync(*mut Display, c_int) -> c_int;
    fn XFlush(*mut Display) -> c_int;
    fn XPending(*mut Display) -> c_int;
    fn XNextEvent(*mut Display, *mut Event) -> c_int;
    fn XCreateGC(*mut Display, Drawable, c_ulong, *mut c_void) -> *mut GC;
    fn XCreateImage(
        *mut Display, *mut Visual, c_uint, c_int, c_int, *mut c_char,
        c_uint, c_uint, c_int, c_int
    ) -> *mut c_void;
    fn XPutImage(
        *mut Display, Drawable, *mut GC, *mut c_void,
        c_int, c_int, c_int, c_int, c_uint, c_uint
    ) -> c_int;
    fn XGrabPointer(
        *mut Display, Window, c_int, c_uint, c_int, c_int, Window, Cursor, Time
    ) -> c_int;
    fn XUngrabPointer(*mut Display, Time) -> c_int;
    // 键盘：输入行用。抓键盘是 override-redirect 窗口拿到按键的唯一稳路
    //（没有 WM 替它管焦点），XLookupString 负责键码 → 字符/键符号。
    fn XGrabKeyboard(*mut Display, Window, c_int, c_int, c_int, Time) -> c_int;
    fn XUngrabKeyboard(*mut Display, Time) -> c_int;
    fn XLookupString(
        *mut KeyEvent, *mut c_char, c_int, *mut KeySym, *mut c_void
    ) -> c_int;
    fn XQueryExtension(
        *mut Display, *const c_char, *mut c_int, *mut c_int, *mut c_int
    ) -> c_int;
    fn XSetErrorHandler(Option<unsafe extern "C" fn(*mut Display, *mut c_void) -> c_int>)
        -> Option<unsafe extern "C" fn(*mut Display, *mut c_void) -> c_int>;
}

bindings! { XExt, "libXext.so.6";
    fn XShapeCombineRectangles(
        *mut Display, Window, c_int, c_int, c_int, *mut Rectangle, c_int, c_int, c_int
    );
}
