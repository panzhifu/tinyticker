//! libxkbcommon 的符号声明（运行时 dlopen，不链接）。
//!
//! Wayland 的 `wl_keyboard.key` 只给 evdev 键码，"这个键在当前布局上是哪个字符"
//! 要靠合成器下发的 keymap（一份 XKB v1 文本）来解。这份语法的规则表有几十年历史，
//! 自己解析不现实，交给系统的 libxkbcommon。它与本目录的 libfreetype / libasound
//! 同一待遇：运行时 `dlopen`，构建期零依赖，`ldd` 依旧只剩 libc / libm / libgcc_s。
//! 库不在（极罕见：Wayland 合成器自身都链接它）时输入行打不开，其余功能不受影响。

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};

use super::Lib;

const LIB_NAME: &str = "libxkbcommon.so.0";
/// `xkb_keymap_new_from_string` 的 format：`XKB_KEYMAP_FORMAT_TEXT_V2`。
const FORMAT_TEXT_V2: c_uint = 1;

/// 一个已打开的 xkbcommon 会话：context + 当前 keymap + 状态机。
///
/// 不实现 `Drop`：挂件常驻进程，与其它 dlopen 的库一致活到进程结束；
/// keymap/state 换新时会 unref 掉旧值。
pub struct Xkb {
    ctx: *mut c_void,
    keymap: *mut c_void,
    state: *mut c_void,
    context_new: unsafe extern "C" fn(c_uint) -> *mut c_void,
    keymap_new:
        unsafe extern "C" fn(*mut c_void, *const c_char, c_uint, c_uint) -> *mut c_void,
    keymap_unref: unsafe extern "C" fn(*mut c_void),
    state_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    state_unref: unsafe extern "C" fn(*mut c_void),
    update_mask: unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint, c_uint, c_uint, c_uint),
    get_one_sym: unsafe extern "C" fn(*mut c_void, c_uint) -> c_uint,
    to_utf8: unsafe extern "C" fn(c_uint, *mut c_char, usize) -> c_int,
    _lib: Lib,
}

impl Xkb {
    /// 打开库并建好 context；任一符号缺失或建不出来返回 `None`。
    pub fn load() -> Option<Self> {
        let lib = Lib::open(LIB_NAME)?;
        let context_new: unsafe extern "C" fn(c_uint) -> *mut c_void =
            lib.func("xkb_context_new")?;
        let ctx = unsafe { context_new(0) };
        if ctx.is_null() {
            return None;
        }
        Some(Self {
            ctx,
            keymap: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            context_new,
            keymap_new: lib.func("xkb_keymap_new_from_string")?,
            keymap_unref: lib.func("xkb_keymap_unref")?,
            state_new: lib.func("xkb_state_new")?,
            state_unref: lib.func("xkb_state_unref")?,
            update_mask: lib.func("xkb_state_update_mask")?,
            get_one_sym: lib.func("xkb_state_key_get_one_sym")?,
            to_utf8: lib.func("xkb_keysym_to_utf8")?,
            _lib: lib,
        })
    }

    /// 换 keymap（`wl_keyboard.keymap` 事件的正文，以 NUL 结尾的 XKB v1 文本）。
    /// 旧的 keymap 与状态一并释放；失败返回 `false` 且保持原状。
    pub fn set_keymap(&mut self, text: &CStr) -> bool {
        let km = unsafe { (self.keymap_new)(self.ctx, text.as_ptr(), FORMAT_TEXT_V2, 0) };
        if km.is_null() {
            return false;
        }
        let st = unsafe { (self.state_new)(km) };
        if st.is_null() {
            unsafe { (self.keymap_unref)(km) };
            return false;
        }
        unsafe {
            if !self.keymap.is_null() {
                (self.keymap_unref)(self.keymap);
            }
            if !self.state.is_null() {
                (self.state_unref)(self.state);
            }
        }
        self.keymap = km;
        self.state = st;
        true
    }

    /// `wl_keyboard.modifiers` 的载荷喂进状态机。shift / CapsLock 由此生效——
    /// 不喂的话按 Shift+数字出不来符号。
    pub fn update_mods(&self, depressed: c_uint, latched: c_uint, locked: c_uint, group: c_uint) {
        if self.state.is_null() {
            return;
        }
        // 布局的三个位置参数合成器只给一个 group，三处同填是标准桥接写法
        unsafe { (self.update_mask)(self.state, depressed, latched, locked, group, group, group) };
    }

    /// 键码 → 键符号。`keycode` 是 xkb 口径（evdev + 8）。0 表示没有 keymap 或无符号。
    pub fn key_sym(&self, keycode: c_uint) -> c_uint {
        if self.state.is_null() {
            return 0;
        }
        unsafe { (self.get_one_sym)(self.state, keycode) }
    }

    /// 键码 → 键面字符。特殊键（修饰键 / 功能键）没有 UTF-8 对应，返回 `None`。
    pub fn key_char(&self, keycode: c_uint) -> Option<char> {
        let sym = self.key_sym(keycode);
        if sym == 0 {
            return None;
        }
        let mut buf = [0 as c_char; 8];
        // 返回值 >0 是写入的字节数（键面字符最长 4 字节 UTF-8），0 / -1 都算没有
        let n = unsafe { (self.to_utf8)(sym, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n as usize) };
        std::str::from_utf8(bytes).ok()?.chars().next()
    }
}
