//! 系统 API 声明层。
//!
//! 本项目的原则是不引入任何第三方 crate：GUI 协议与总线都直接用发行版自带的共享库
//! （libwayland-client / libX11 / libdbus），运行时 `dlopen` 取符号。这样二进制仍然
//! 只硬链 libc，也不带来构建期的 -dev 依赖，和被调方共享同一份实现（体积不进二进制）。
//!
//! 本模块只负责「打开库 + 取符号 + C 类型声明」，不含任何业务逻辑。

// 这里是对 C 头的镜像：常量与符号按协议/头文件列全，用不到的也会被报死代码。
#![allow(dead_code)]

pub mod dbus;
pub mod wayland;
pub mod x11;

use std::ffi::{CString, c_char, c_int, c_void};
use std::mem::size_of;

const RTLD_NOW: c_int = 2;
const RTLD_LOCAL: c_int = 0;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

// —— poll(2)：两个后端都用它等 fd 事件（X 连接 fd / Wayland socket） ——

/// `struct pollfd`
#[repr(C)]
pub struct PollFd {
    pub fd: c_int,
    pub events: i16,
    pub revents: i16,
}

pub const POLL_IN: i16 = 1;

unsafe extern "C" {
    pub fn poll(fds: *mut PollFd, nfds: u64, timeout: c_int) -> c_int;
}

/// 已打开的共享库。故意不提供 Drop：挂件常驻进程，库映射活到进程结束即可，
/// `dlclose` 反而会让先前取到的函数指针失效。
pub struct Lib(*mut c_void);

impl Lib {
    pub fn open(name: &str) -> Option<Self> {
        let c = CString::new(name).ok()?;
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        (!h.is_null()).then_some(Self(h))
    }

    /// 取函数符号。`T` 必须是 `unsafe extern "C" fn(...) -> ...`。
    pub fn func<T: Copy>(&self, name: &str) -> Option<T> {
        let p = self.raw(name)?;
        assert_eq!(
            size_of::<T>(),
            size_of::<*mut c_void>(),
            "{name}: 符号宽度不是指针，声明与 .so 不符"
        );
        // dlsym 返回的就是函数地址；Option<fn> 以空指针做判别，非空即 Some
        Some(unsafe { std::mem::transmute_copy(&p) })
    }

    /// 取数据符号，例如 libwayland 导出的 `wl_surface_interface` 描述符。
    pub fn data<T>(&self, name: &str) -> Option<&'static T> {
        let p = self.raw(name)?;
        Some(unsafe { &*(p as *const T) })
    }

    fn raw(&self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let p = unsafe { dlsym(self.0, c.as_ptr()) };
        (!p.is_null()).then_some(p)
    }
}

