//! 自研 Wayland 客户端：layer-shell overlay 层挂件。
//!
//! 直调系统 libwayland-client（声明与接口描述符见 `src/sys/wayland.rs`），不经过
//! winit 也不经过 wayland-client crate。放在 overlay 层是 Wayland 上唯一能压住全屏
//! 窗口的办法——xdg-shell 不给客户端任何指定层级的途径。
//!
//! 事件回调与主循环之间只经 [`Events`] 里的 `Cell` 单向传值：回调在 `dispatch` 内部被
//! libwayland 同步调用，用共享引用而不是 `&mut`，从而不存在可重入借用。
//!
//! layer-shell 没有 `xdg_toplevel.move`，拖动只能自己改 margin；位移量取自增指针
//! （`zwp_relative_pointer_v1`）的增量而非 surface 局部坐标——后者窗口跟随指针会让
//! 局部坐标恒定不变，从而卡死。按住左键期间 Wayland 的隐式指针 grab 保证事件不丢。

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem::size_of;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::tray::TrayHandle;

use crate::config::Config;
use crate::render::Canvas;
use crate::sys::wayland::{Ifaces, Obj, Wl, WlArgument, WlInterface};
use crate::sys::{Lib, POLL_IN, PollFd, poll};
use crate::tray::Command;
use crate::widget::{LOGICAL_SIZE, Frame, Widget};

/// Linux input-event 按键码，`wl_pointer.button` 原样透传。
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// 没有历史位置时的初始坐标（逻辑像素，距左上角）。
const DEFAULT_POS: (i32, i32) = (80, 80);
/// layer-shell：overlay 层、anchor top|left、键盘不交互。
const LAYER_OVERLAY: u32 = 3;
const ANCHOR_TOP_LEFT: u32 = 1 | 4;
/// wl_seat 能力位。
const SEAT_POINTER: u32 = 1;
/// wp_cursor_shape_device_v1 的 `default`（普通箭头）。
/// 注意这个枚举**从 1 开始**（0 是无效值）：3 是 `help`，写成 3 会显示成问号光标。
const SHAPE_DEFAULT: u32 = 1;
/// 请求 opcode（顺序照协议 XML）
const SURFACE_ATTACH: u32 = 1;
const SURFACE_DAMAGE: u32 = 2;
const SURFACE_SET_INPUT_REGION: u32 = 5;
const SURFACE_COMMIT: u32 = 6;
const SURFACE_DAMAGE_BUFFER: u32 = 9;
const LAYER_SET_SIZE: u32 = 0;
const LAYER_SET_ANCHOR: u32 = 1;
const LAYER_SET_EXCLUSIVE_ZONE: u32 = 2;
const LAYER_SET_MARGIN: u32 = 3;
const LAYER_SET_KEYBOARD: u32 = 4;
const LAYER_ACK_CONFIGURE: u32 = 6;

// —— 共享库调用：mmap / poll / memfd / close ——

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64)
    -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
    fn ftruncate(fd: c_int, length: i64) -> c_int;
}

const PROT_RW: c_int = 1 | 2;
const MAP_SHARED: c_int = 1;
const MAP_FAILED: *mut c_void = !0usize as *mut c_void;

/// libwayland 的 `f`（`wl_fixed_t`）是 **24.8** 定点数，1.0 = **256**
/// （`wayland-util.h`：`wl_fixed_to_int(f) { return f / 256; }`）。
///
/// 曾被当成 16.16（÷65536）写过，后果是 `relative_motion` 的 dx/dy 被缩小 256 倍：
/// 鼠标挪 128 px 才凑够 `round()` 的 0.5 阈值、挂件只挪 1 px，表现为"完全拖不动"。
/// 原始值只需整数除法精度，`/ 256.0` 与 `wl_fixed_to_double` 逐位一致。
fn fixed(v: i32) -> f64 {
    v as f64 / 256.0
}

/// 回调 → 主循环的单向传值。
#[derive(Default)]
struct Events {
    globals: RefCell<Vec<(u32, String, u32)>>,
    /// 待 ack 的 layer configure serial（0 = 无）
    configure: Cell<u32>,
    closed: Cell<bool>,
    /// preferred_scale，单位 1/120（0 = 未收到）
    scale: Cell<u32>,
    seat_caps: Cell<u32>,
    /// 两块缓冲各自的 release 标志
    released: [Cell<bool>; 2],
    left_down: Cell<bool>,
    /// 左键"刚按下"的边沿标记：主循环据此把 drag_origin 对齐到当前 margin
    left_pressed: Cell<bool>,
    right_pressed: Cell<bool>,
    rel_dx: Cell<f64>,
    rel_dy: Cell<f64>,
    axis: Cell<f64>,
    axis_discrete: Cell<i32>,
    axis_120: Cell<i32>,
    frame: Cell<bool>,
    /// 最近一次指针事件 serial（设光标形状要用）
    serial: Cell<u32>,
}

/// `wl_buffer.release` 回调要区分是哪块缓冲，data 指针带上下标。
struct BufSink {
    events: &'static Events,
    index: usize,
}

/// 一块 shm 缓冲：mmap 地址 + wl_buffer。
#[derive(Clone, Copy)]
struct Buffer {
    map: *mut c_void,
    buffer: Obj,
}

pub struct Client {
    wl: Wl,
    ifaces: &'static Ifaces,
    events: &'static Events,
    display: Obj,
    registry: Obj,
    compositor: Obj,
    shm: Obj,
    _shell: Obj,
    _seat: Option<Obj>,
    surface: Obj,
    layer: Obj,
    viewport: Option<Obj>,
    // 以下只需保活（事件写进 Events），主循环不再直接使用
    _fractional: Option<Obj>,
    _relative: Option<Obj>,
    cursor: Option<Obj>,
    region: Option<Obj>,
    input_rect: Option<(i32, i32, i32, i32)>,
    buffers: [Buffer; 2],
    pool: Obj,
    pool_bytes: usize,
    front: usize,
    widget: Widget,
    logical: (u32, u32),
    margin: (i32, i32),
    scale: f64,
    font_scale: u32,
    configured: bool,
    quit: bool,
    drag_origin: (i32, i32),
    drag_acc: (f64, f64),
}

/// 创建并运行 layer-shell 挂件；合成器不支持时返回 `Err`。
pub fn run(
    cmd_rx: &Receiver<Command>,
    handle_rx: &Receiver<TrayHandle>,
    config: Config,
    autostart: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(config, autostart)?;
    client.event_loop(cmd_rx, handle_rx)?;
    Ok(())
}

impl Client {
    fn new(config: Config, autostart: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let lib = Lib::open("libwayland-client.so.0").ok_or("打不开 libwayland-client.so.0")?;
        let wl = Wl::load().ok_or("libwayland-client 符号不全")?;
        let ifaces: &'static Ifaces =
            Box::leak(Box::new(Ifaces::load(&lib).ok_or("接口描述符符号不全")?));
        let events: &'static Events = Box::leak(Box::default());
        let edata = events as *const Events as *mut c_void;

let display = unsafe { (wl.connect)(std::ptr::null()) };
        if display.is_null() {
            return Err("无法连接 Wayland 显示".into());
        }

        // registry：先收齐 global
        let registry = request_new(&wl, display, 1, ifaces.registry, &mut [WlArgument::NIL]);
        let table = leak_listeners(vec![
            cb(on_global as *const c_void),
            cb(on_global_remove as *const c_void),
        ]);
        check(unsafe { (wl.add_listener)(registry, table, edata) }, "registry 监听")?;
        if unsafe { (wl.roundtrip)(display) } < 0 {
            return Err("registry roundtrip 失败".into());
        }
        let global = |n: &str| -> Option<(u32, u32)> {
            events
                .globals
                .borrow()
                .iter()
                .find(|(_, i, _)| i == n)
                .map(|(a, _, v)| (*a, *v))
        };
        let need = |n: &str| -> Result<(u32, u32), Box<dyn std::error::Error>> {
            global(n).ok_or_else(|| format!("合成器未提供 {n}").into())
        };
        let bind_to = |id: u32, have: u32, iface: *const WlInterface, want: u32| {
            bind(&wl, registry, id, iface, have.min(want))
        };
        let bind_by = |n: &str, iface: *const WlInterface, want: u32| -> Option<Obj> {
            global(n).map(|(id, have)| bind_to(id, have, iface, want))
        };

        // layer-shell 是这条路径的前提
        let (shell_id, shell_ver) = need("zwlr_layer_shell_v1")?;
        let shell = bind_to(shell_id, shell_ver, ifaces.layer_shell, 4);
        let compositor = bind_by("wl_compositor", ifaces.compositor, 6).ok_or("没有 wl_compositor")?;
        let shm = bind_by("wl_shm", ifaces.shm, 1).ok_or("没有 wl_shm")?;
        // 能力位在 roundtrip 期间就会送到，必须先挂监听
        let seat = bind_by("wl_seat", ifaces.seat, 5).inspect(|s| {
            let t = leak_listeners(vec![
                cb(on_seat_capabilities as *const c_void),
                cb(on_seat_name as *const c_void),
            ]);
            unsafe { (wl.add_listener)(*s, t, edata) };
        });

        let widget = Widget::new(config, autostart);
        let margin = widget.config.window_pos.unwrap_or(DEFAULT_POS);
        let logical = zoomed_logical(widget.zoom);

        let surface =
            request_new(&wl, compositor, 0, ifaces.surface, &mut [WlArgument::NIL]);
        // get_layer_surface 签名 "no?ous"：output 传 NULL 由合成器选屏
        let ns = CString::new("tinyticker").unwrap();
        let layer = request_new(
            &wl,
            shell,
            0,
            ifaces.layer_surface,
            &mut [
                WlArgument::NIL,
                WlArgument::obj(surface),
                WlArgument::obj(std::ptr::null_mut()),
                WlArgument::uint(LAYER_OVERLAY),
                WlArgument::cstr(ns.as_ptr()),
            ],
        );

request(&wl, layer, LAYER_SET_SIZE, &[uint(logical.0), uint(logical.1)]);
        request(&wl, layer, LAYER_SET_ANCHOR, &[uint(ANCHOR_TOP_LEFT)]);
        request(&wl, layer, LAYER_SET_EXCLUSIVE_ZONE, &[int(0)]); // 不独占空间
        request(&wl, layer, LAYER_SET_KEYBOARD, &[uint(0)]); // 永不抢焦点
        request(
            &wl,
            layer,
            LAYER_SET_MARGIN,
            &[int(margin.1), int(0), int(0), int(margin.0)],
        );
        let layer_table = leak_listeners(vec![
            cb(on_layer_configure as *const c_void),
            cb(on_layer_closed as *const c_void),
        ]);
        unsafe { (wl.add_listener)(layer, layer_table, edata) };

        // 分数缩放：缓冲按物理像素画，viewport 把表面缩回逻辑尺寸（buffer_scale 保持 1）
        let viewport = bind_by("wp_viewporter", ifaces.viewporter, 1).map(|vp| {
            request_new(&wl, vp, 1, ifaces.viewport, &mut [WlArgument::NIL, WlArgument::obj(surface)])
        });
        let fractional = bind_by("wp_fractional_scale_manager_v1", ifaces.fractional_manager, 1).map(
            |mgr| {
                let obj = request_new(
                    &wl,
                    mgr,
                    1,
                    ifaces.fractional_scale,
                    &mut [WlArgument::NIL, WlArgument::obj(surface)],
                );
                let t = leak_listeners(vec![cb(on_preferred_scale as *const c_void)]);
                unsafe { (wl.add_listener)(obj, t, edata) };
                obj
            },
        );

        let mut client = Self {
            wl,
            ifaces,
            events,
            display,
            registry,
            compositor,
            shm,
            _shell: shell,
            _seat: seat,
            surface,
            layer,
            viewport,
            _fractional: fractional,
            _relative: None,
            cursor: None,
            region: None,
            input_rect: None,
            buffers: [
                Buffer { map: std::ptr::null_mut(), buffer: std::ptr::null_mut() },
                Buffer { map: std::ptr::null_mut(), buffer: std::ptr::null_mut() },
            ],
            pool: std::ptr::null_mut(),
            pool_bytes: 0,
            front: 0,
            widget,
            logical,
            margin,
            scale: 1.0,
            font_scale: 1,
            configured: false,
            quit: false,
            drag_origin: (0, 0),
            drag_acc: (0.0, 0.0),
        };
        if let Some(seat) = client._seat {
            // 能力位要等一次 roundtrip 才收到
            unsafe { (client.wl.roundtrip)(client.display) };
            client.bind_pointer(&seat);
        } else {
            eprintln!("⚠️ 合成器未提供 wl_seat，挂件无法接收鼠标输入");
        }
        // 只提交状态：configure 要等首次 commit 之后才下发
client.commit();
        Ok(client)
    }

    /// 指针设备 + 自增指针 + 光标形状。
    fn bind_pointer(&mut self, seat: &Obj) {
        let edata = self.events as *const Events as *mut c_void;
        let caps = self.events.seat_caps.get();
        if caps & SEAT_POINTER == 0 {
            eprintln!("⚠️ 座位无指针能力，挂件无法接收鼠标输入");
            return;
        }
        let pointer = request_new(
            &self.wl,
            *seat,
            0,
            self.ifaces.pointer,
            &mut [WlArgument::NIL, WlArgument::obj(*seat)],
        );
        let table = leak_listeners(vec![
            cb(on_ptr_enter as *const c_void),
            cb(on_ptr_leave as *const c_void),
            cb(on_ptr_motion as *const c_void),
            cb(on_ptr_button as *const c_void),
            cb(on_ptr_axis as *const c_void),
            cb(on_ptr_frame as *const c_void),
            cb(on_ptr_noop as *const c_void), // axis_source
            cb(on_ptr_noop as *const c_void), // axis_stop
            cb(on_ptr_axis_discrete as *const c_void),
            cb(on_ptr_axis_value120 as *const c_void),
        ]);
        unsafe { (self.wl.add_listener)(pointer, table, edata) };

        // 拖动位移取自增指针：局部坐标会跟着窗口一起动，算不出位移
        if let Some((id, have)) = self.find_global("zwp_relative_pointer_manager_v1") {
            let mgr = bind(&self.wl, self.registry, id, self.ifaces.relative_manager, have);
            let rel = request_new(
                &self.wl,
                mgr,
                1,
                self.ifaces.relative_pointer,
                &mut [WlArgument::NIL, WlArgument::obj(pointer)],
            );
            let t = leak_listeners(vec![cb(on_relative_motion as *const c_void)]);
            unsafe { (self.wl.add_listener)(rel, t, edata) };
            self._relative = Some(rel);
        } else {
            eprintln!("⚠️ 合成器未提供 relative-pointer，挂件无法拖动");
        }
        // 光标形状：现代协议由合成器画，省掉主题图标加载
        if let Some((id, have)) = self.find_global("wp_cursor_shape_manager_v1") {
            let mgr = bind(&self.wl, self.registry, id, self.ifaces.cursor_manager, have);
            let dev = request_new(
                &self.wl,
                mgr,
                1,
                self.ifaces.cursor_device,
                &mut [WlArgument::NIL, WlArgument::obj(pointer)],
            );
            self.cursor = Some(dev);
        }
    }

    fn find_global(&self, n: &str) -> Option<(u32, u32)> {
        self.events
            .globals
            .borrow()
            .iter()
            .find(|(_, i, _)| i == n)
            .map(|(a, _, v)| (*a, *v))
    }

    fn physical_size(&self) -> (u32, u32) {
        let (lw, lh) = self.logical;
        (
            ((lw as f64 * self.scale).round() as u32).max(1),
            ((lh as f64 * self.scale).round() as u32).max(1),
        )
    }

    /// 按当前逻辑尺寸与缩放重建 shm 双缓冲。
    fn rebuild_buffers(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (w, h) = self.physical_size();
        let half = (w as usize) * (h as usize) * 4;
        let bytes = half * 2;
        if !self.pool.is_null() {
            unsafe {
                (self.wl.destroy)(self.buffers[0].buffer);
                (self.wl.destroy)(self.buffers[1].buffer);
                (self.wl.destroy)(self.pool);
                munmap(self.buffers[0].map, self.pool_bytes);
            }
            self.pool = std::ptr::null_mut();
        }
        let fd = unsafe { memfd_create(c"tinyticker-shm".as_ptr(), 2 /* MFD_CLOEXEC */) };
        if fd < 0 {
            return Err("memfd_create 失败".into());
        }
        if unsafe { ftruncate(fd, bytes as i64) } != 0 {
            unsafe { close(fd) };
            return Err("ftruncate 失败".into());
        }
        let map = unsafe { mmap(std::ptr::null_mut(), bytes, PROT_RW, MAP_SHARED, fd, 0) };
        if map == MAP_FAILED {
            unsafe { close(fd) };
            return Err("mmap 失败".into());
        }
        let mut args = [WlArgument::NIL, WlArgument::fd(fd), int(bytes as i32)];
        self.pool = request_new(&self.wl, self.shm, 0, self.ifaces.shm_pool, &mut args);
        // pool 请求已把 fd 交给合成器（它自己 dup），客户端侧关掉
        unsafe { close(fd) };
        self.pool_bytes = bytes;
        let edata = self.events as *const Events as *mut c_void;
        for (i, slot) in self.buffers.iter_mut().enumerate() {
            let mut args = [
                WlArgument::NIL,
                int((i * half) as i32),
                int(w as i32),
                int(h as i32),
                int((w * 4) as i32),
                uint(0), // ARGB8888
            ];
            let sink = Box::leak(Box::new(BufSink { events: self.events, index: i }));
            let buffer =
                request_new(&self.wl, self.pool, 0, self.ifaces.buffer, &mut args);
            let t = leak_listeners(vec![cb(on_buffer_release as *const c_void)]);
            unsafe { (self.wl.add_listener)(buffer, t, sink as *const BufSink as *mut c_void) };
            slot.buffer = buffer;
            slot.map = unsafe { map.byte_add(i * half) };
            self.events.released[i].set(true);
        }
        if let Some(vp) = self.viewport {
            let (lw, lh) = self.logical;
            // wp_viewport: destroy(0) set_source(1) set_destination(2)
            request(&self.wl, vp, 2, &[int(lw as i32), int(lh as i32)]);
        }
        self.font_scale = ((self.scale * self.widget.zoom as f64).round() as u32).max(1);
        let _ = edata;
        Ok(())
    }

    fn commit(&mut self) {
        request(&self.wl, self.surface, SURFACE_COMMIT, &[]);
        unsafe { (self.wl.flush)(self.display) };
    }

    fn set_margin(&mut self, (x, y): (i32, i32)) {
        self.margin = (x, y);
        request(
            &self.wl,
            self.layer,
            LAYER_SET_MARGIN,
            &[int(y), int(0), int(0), int(x)],
        );
    }

    /// 把合成器与指针事件落到状态上；返回是否请求退出。
    fn apply_events(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let e = self.events;
        if e.closed.get() {
            self.quit = true;
            return Ok(());
        }
        let serial = e.configure.take();
        if serial != 0 {
            request(&self.wl, self.layer, LAYER_ACK_CONFIGURE, &[uint(serial)]);
            self.configured = true;
            self.commit();
        }
        let scale = e.scale.take();
        if scale != 0 {
            let s = scale as f64 / 120.0;
            if s > 0.0 && s != self.scale {
                self.scale = s;
                self.rebuild_buffers()?;
                self.commit();
            }
        }
        // 光标形状：每次进表面都要用当时的 serial 重设
        if let Some(dev) = self.cursor {
            let serial = e.serial.take();
            if serial != 0 {
                // wp_cursor_shape_device_v1: destroy(0) set_shape(1)
                request(&self.wl, dev, 1, &[uint(serial), uint(SHAPE_DEFAULT)]);
            }
        }
        // 按下瞬间把拖动原点钉在当前 margin 上、增量清零。少了这一步，
        // 挂件会跳到"自启动以来累计位移"的位置上，指针立刻脱离文字。
        if e.left_pressed.replace(false) {
            self.drag_origin = self.margin;
            self.drag_acc = (0.0, 0.0);
        }
        // 拖动
        let (dx, dy) = (e.rel_dx.take(), e.rel_dy.take());
        if (dx != 0.0 || dy != 0.0) && e.left_down.get() {
            self.drag_acc.0 += dx;
            self.drag_acc.1 += dy;
            let next = (
                self.drag_origin.0 + self.drag_acc.0.round() as i32,
                self.drag_origin.1 + self.drag_acc.1.round() as i32,
            );
            if next != self.margin {
                self.set_margin(next);
                self.commit();
            }
        }
        if e.frame.get() {
            self.flush_axis()?;
        }
        if e.right_pressed.replace(false) {
            self.quit = true;
        }
        Ok(())
    }

    /// 一帧滚轮事件结算一次：优先离散格数（Wayland 符号与直觉相反，取负）。
    fn flush_axis(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let e = self.events;
        e.frame.set(false);
        let steps = if e.axis_120.get() != 0 {
            e.axis_120.take() as f64 / 120.0
        } else if e.axis_discrete.get() != 0 {
            e.axis_discrete.take() as f64
        } else {
            // 无离散信息时按传统 10 单位一格
            e.axis.take() / 10.0
        };
        e.axis.set(0.0);
        if steps != 0.0 {
            self.widget.zoom_by(-steps as f32);
            self.sync_zoom()?;
        }
        Ok(())
    }

    /// 按当前 zoom 结算 surface 尺寸：悬浮窗滚轮与托盘 `Scroll` 都走这一条。
    /// 尺寸没变（已顶到 clamp 边界，或还没 configure）就整段跳过，免得白折腾 shm。
    fn sync_zoom(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let want = zoomed_logical(self.widget.zoom);
        if !self.configured || want == self.logical {
            return Ok(());
        }
        self.logical = want;
        request(
            &self.wl,
            self.layer,
            LAYER_SET_SIZE,
            &[uint(self.logical.0), uint(self.logical.1)],
        );
        // rebuild_buffers 会按新的 self.logical 重算 viewport 目标尺寸与 font_scale
        self.rebuild_buffers()?;
        self.commit();
        Ok(())
    }

    /// 绘制一帧；内容未变化时整帧跳过（连 shm 都不提交）。
    fn render(&mut self) {
        if !self.configured || self.pool.is_null() {
            return;
        }
        let (bw, bh) = self.physical_size();
        let Some(frame) = self.widget.build_frame(bw, bh, self.font_scale) else {
            return;
        };
        // 挑一块已释放的缓冲；两块都被合成器占着就跳过本帧
        let back = self.front ^ 1;
        let index = if self.events.released[back].get() {
            back
        } else if self.events.released[self.front].get() {
            self.front
        } else {
            return;
        };
        let n = (bw * bh) as usize;
        unsafe {
            let px = std::slice::from_raw_parts_mut(self.buffers[index].map as *mut u32, n);
            let mut canvas = Canvas::new(px, bw, bh);
            frame.paint(&mut canvas);
        }
        self.events.released[index].set(false);
        self.front = index;

        request(
            &self.wl,
            self.surface,
            SURFACE_ATTACH,
            &[WlArgument::obj(self.buffers[index].buffer), int(0), int(0)],
        );
        // damage_buffer 需要 wl_surface v4+，旧版本退化为 surface 坐标 damage
        let opcode = if unsafe { (self.wl.proxy_version)(self.surface) } >= 4 {
            SURFACE_DAMAGE_BUFFER
        } else {
            SURFACE_DAMAGE
        };
        request(
            &self.wl,
            self.surface,
            opcode,
            &[int(0), int(0), int(bw as i32), int(bh as i32)],
        );
        self.apply_input_region(&frame);
        self.commit();
    }

    /// 点击穿透：把输入区域收缩到文字包围盒（surface 逻辑坐标）。
    fn apply_input_region(&mut self, frame: &Frame) {
        let target = frame.input_rect(self.scale as f32);
        if self.input_rect == target {
            return;
        }
        let new_region = target.map(|(x, y, w, h)| {
            let region =
                request_new(&self.wl, self.compositor, 1, self.ifaces.region, &mut [WlArgument::NIL]);
            request(&self.wl, region, 0, &[int(x), int(y), int(w), int(h)]);
            region
        });
        request(
            &self.wl,
            self.surface,
            SURFACE_SET_INPUT_REGION,
            &[WlArgument::obj(new_region.unwrap_or(std::ptr::null_mut()))],
        );
        if let Some(old) = self.region.take() {
            unsafe { (self.wl.destroy)(old) };
        }
        self.region = new_region;
        self.input_rect = target;
    }

    /// 事件循环：每 `Widget::tick_interval` 至少推进一次，其余时间阻塞在 Wayland socket 上。
    /// （动画特效开着时这个间隔会缩到 50ms，静态时是 200ms。）
    fn event_loop(
        &mut self,
        cmd_rx: &Receiver<Command>,
        handle_rx: &Receiver<TrayHandle>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        while !self.quit {
            while let Ok(handle) = handle_rx.try_recv() {
                self.widget.accept_tray_handle(handle);
            }
            while let Ok(cmd) = cmd_rx.try_recv() {
                if self.widget.handle_cmd(cmd) {
                    self.quit = true;
                }
            }
            if self.quit {
                break;
            }
            // 托盘命令也可能改 zoom（图标上的 Scroll），尺寸统一在这里结算
            self.sync_zoom()?;
            self.widget.tick();
            self.apply_events()?;
            self.render();

            let deadline = Instant::now() + self.widget.tick_interval();
            unsafe { (self.wl.flush)(self.display) };
            // prepare_read 返回 0 才需要等 socket（非 0 表示已有事件排队）
            if unsafe { (self.wl.prepare_read)(self.display) } == 0 {
                let fd = unsafe { (self.wl.get_fd)(self.display) };
                let budget = deadline.saturating_duration_since(Instant::now());
                let ms = budget.as_millis().min(c_int::MAX as u128) as c_int;
                let mut pfd = [PollFd { fd, events: POLL_IN, revents: 0 }];
                let _ = unsafe { poll(pfd.as_mut_ptr(), 1, ms) };
                if unsafe { (self.wl.read_events)(self.display) } < 0 {
                    unsafe { (self.wl.cancel_read)(self.display) };
                    return Err("读取 Wayland 事件失败".into());
                }
            }
            if unsafe { (self.wl.dispatch_pending)(self.display) } < 0 {
                return Err("派发 Wayland 事件失败".into());
            }
        }
        self.widget.persist(Some(self.margin));
        Ok(())
    }
}

fn zoomed_logical(zoom: f32) -> (u32, u32) {
    (
        (LOGICAL_SIZE.0 as f32 * zoom).round() as u32,
        (LOGICAL_SIZE.1 as f32 * zoom).round() as u32,
    )
}

// —— FFI 辅助 ——

/// 把带具体签名的回调塞进 libwayland 的 `void (**)(void)` 监听表。
/// libwayland 只按 opcode 取数组下标后直接调用，签名一致性由调用方保证。
///
/// 参数写成 `*const c_void`：函数名若直接进泛型会推断成零宽的 fn item，
/// 先转成指针才拿得到真正的函数地址。
fn cb(f: *const c_void) -> unsafe extern "C" fn() {
    debug_assert_eq!(size_of::<*const c_void>(), size_of::<unsafe extern "C" fn()>());
    unsafe { std::mem::transmute(f) }
}

/// 监听表要活到进程结束（libwayland 一直持有指针），所以建一次就泄漏。
fn leak_listeners(items: Vec<unsafe extern "C" fn()>) -> *mut unsafe extern "C" fn() {
    Box::leak(items.into_boxed_slice()).as_mut_ptr()
}

fn int(v: i32) -> WlArgument {
    WlArgument::int(v)
}

fn uint(v: u32) -> WlArgument {
    WlArgument::uint(v)
}

fn check(code: c_int, what: &str) -> Result<(), Box<dyn std::error::Error>> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("{what} 失败（{code}）").into())
    }
}

/// 发一个不创建对象的请求。
fn request(wl: &Wl, target: Obj, opcode: u32, args: &[WlArgument]) {
    let mut args = args.to_vec();
    let r = unsafe { (wl.marshal)(target, opcode, args.as_mut_ptr()) };
    debug_assert_eq!(r, 0, "请求发送失败 opcode={opcode}");
}

/// 发一个创建新对象的请求（new_id 在签名首位）。
///
/// 统一规则：`args` 必须逐个对应签名字符，new_id 那一格填 NIL 表示请库创建并回填。
/// 首位是 `n` 的请求（get_registry / create_surface / create_pool / create_buffer /
/// get_layer_surface / get_viewport …）用本函数；new_id 在末尾的 `wl_registry.bind`
/// 见 [`bind`]。
fn request_new(
    wl: &Wl,
    factory: Obj,
    opcode: u32,
    iface: *const WlInterface,
    args: &mut [WlArgument],
) -> Obj {
    debug_assert!(unsafe { args[0].o }.is_null(), "首位不是 new_id 槽");
    let version = unsafe { (wl.proxy_version)(factory) };
    let new = unsafe { (wl.marshal_ctor)(factory, opcode, args.as_mut_ptr(), iface, version) };
    assert!(!new.is_null(), "创建对象失败 opcode={opcode}");
    new
}

/// 按 global 名字绑定对象（等价于头文件里 inline 的 `wl_registry_bind`）。
/// 签名 "usun"：new_id 在末尾，args 只给前三个参数。
fn bind(wl: &Wl, registry: Obj, name: u32, iface: *const WlInterface, want: u32) -> Obj {
    let (have, iname) = unsafe { ((*iface).version.max(1) as u32, (*iface).name) };
    let version = want.min(have).max(1);
    // 签名 "usun"：new_id 在末尾，args 必须给满 4 个槽位，否则 libwayland 会去读
    // 数组之外的内存当对象指针。
    let mut args = [
        uint(name),
        WlArgument::cstr(iname),
        uint(version),
        WlArgument::NIL,
    ];
    let new = unsafe { (wl.marshal_ctor)(registry, 0, args.as_mut_ptr(), iface, version) };
    assert!(!new.is_null(), "bind 失败");
    new
}

// —— 事件回调 ——

unsafe extern "C" fn on_global(
    data: *mut c_void,
    _registry: Obj,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    let e = unsafe { &*(data as *const Events) };
    let iface = unsafe { CStr::from_ptr(interface) }.to_string_lossy().into_owned();
    e.globals.borrow_mut().push((name, iface, version));
}

unsafe extern "C" fn on_global_remove(_data: *mut c_void, _registry: Obj, _name: u32) {}

unsafe extern "C" fn on_layer_configure(data: *mut c_void, _o: Obj, serial: u32, _w: u32, _h: u32) {
    unsafe { &*(data as *const Events) }.configure.set(serial);
}

unsafe extern "C" fn on_layer_closed(data: *mut c_void, _o: Obj) {
    unsafe { &*(data as *const Events) }.closed.set(true);
}

unsafe extern "C" fn on_preferred_scale(data: *mut c_void, _o: Obj, scale: u32) {
    unsafe { &*(data as *const Events) }.scale.set(scale);
}

unsafe extern "C" fn on_seat_capabilities(data: *mut c_void, _seat: Obj, caps: u32) {
    unsafe { &*(data as *const Events) }.seat_caps.set(caps);
}

unsafe extern "C" fn on_seat_name(_data: *mut c_void, _seat: Obj, _name: *const c_char) {}

unsafe extern "C" fn on_buffer_release(data: *mut c_void, _buffer: Obj) {
    let sink = unsafe { &*(data as *const BufSink) };
    sink.events.released[sink.index].set(true);
}

/// wl_pointer 的十个事件，顺序必须与协议一致（enter…axis_value120）。
unsafe extern "C" fn on_ptr_enter(data: *mut c_void, _o: Obj, serial: u32, _s: Obj, _x: i32, _y: i32) {
    let e = unsafe { &*(data as *const Events) };
    e.serial.set(serial);
    e.frame.set(true);
}

unsafe extern "C" fn on_ptr_leave(_data: *mut c_void, _o: Obj, _serial: u32, _s: Obj) {}

unsafe extern "C" fn on_ptr_motion(_data: *mut c_void, _o: Obj, _time: u32, _x: i32, _y: i32) {}

unsafe extern "C" fn on_ptr_noop(_data: *mut c_void, _o: Obj, _a: u32, _b: u32) {}

unsafe extern "C" fn on_ptr_button(
    data: *mut c_void,
    _o: Obj,
    serial: u32,
    _time: u32,
    button: u32,
    state: u32,
) {
    let e = unsafe { &*(data as *const Events) };
    e.serial.set(serial);
    let pressed = state == 1;
    match (button, pressed) {
        // 按住左键即形成隐式 grab，指针移出表面也继续收事件，拖动不会断
        (BTN_LEFT, true) => {
            e.left_down.set(true);
            e.left_pressed.set(true);
            e.rel_dx.set(0.0);
            e.rel_dy.set(0.0);
        }
        (BTN_LEFT, false) => {
            e.left_down.set(false);
            e.rel_dx.set(0.0);
            e.rel_dy.set(0.0);
        }
        (BTN_RIGHT, true) => e.right_pressed.set(true),
        _ => {}
    }
}

unsafe extern "C" fn on_ptr_axis(data: *mut c_void, _o: Obj, _time: u32, axis: u32, value: i32) {
    // axis 1 = 垂直；fixed 单位 1/256
    if axis == 1 {
        let e = unsafe { &*(data as *const Events) };
        e.axis.set(e.axis.get() + fixed(value));
    }
}

unsafe extern "C" fn on_ptr_frame(data: *mut c_void, _o: Obj) {
    unsafe { &*(data as *const Events) }.frame.set(true);
}

unsafe extern "C" fn on_ptr_axis_discrete(data: *mut c_void, _o: Obj, axis: u32, discrete: i32) {
    if axis == 1 {
        let e = unsafe { &*(data as *const Events) };
        e.axis_discrete.set(e.axis_discrete.get() + discrete);
    }
}

unsafe extern "C" fn on_ptr_axis_value120(data: *mut c_void, _o: Obj, axis: u32, value: i32) {
    if axis == 1 {
        let e = unsafe { &*(data as *const Events) };
        e.axis_120.set(e.axis_120.get() + value);
    }
}

unsafe extern "C" fn on_relative_motion(
    data: *mut c_void,
    _o: Obj,
    _utime_hi: u32,
    _utime_lo: u32,
    dx: i32,
    dy: i32,
    _dx_unaccel: i32,
    _dy_unaccel: i32,
) {
    // 用加速后的 dx/dy：那才是屏幕上指针真正移动的量，挂件才能 1:1 跟手。
    // 增量与窗口跟随无关，不会像表面局部坐标那样反馈死锁。
    let e = unsafe { &*(data as *const Events) };
    e.rel_dx.set(e.rel_dx.get() + fixed(dx));
    e.rel_dy.set(e.rel_dy.get() + fixed(dy));
}

#[cfg(test)]
mod tests {
    use super::fixed;

    /// `wl_fixed_t` 是 24.8，不是 16.16：线上 1 px 的位移就是 256。
    /// 这条钉住换算常数——写成 ÷65536 会让所有位移缩水 256 倍，
    /// 拖动表现为"完全拖不动"（128 px 鼠标位移才凑够 1 px 窗口位移）。
    #[test]
    fn fixed_uses_wl_fixed_t_24_8_scale() {
        assert_eq!(fixed(0), 0.0);
        assert_eq!(fixed(128), 0.5);
        assert_eq!(fixed(256), 1.0);
        assert_eq!(fixed(-256), -1.0);
        // 传统滚轮一格在轴事件里就是 10.0（`flush_axis` 以 /10 折算格数）
        assert_eq!(fixed(2560), 10.0);
    }

    /// 拖动是「按下原点 + 累计增量取整」，所以增量必须与像素同量纲：
    /// 鼠标走 4 px 就该累计出 4.0、窗口跟着走 4 px。
    #[test]
    fn drag_accumulator_tracks_pixels_one_to_one() {
        let origin = (598, 254); // 与配置里的窗口位置同量纲（逻辑像素）
        let mut acc = (0.0f64, 0.0f64);
        for _ in 0..4 {
            acc.0 += fixed(256); // 每帧指针向右 1 px
        }
        let next = (
            origin.0 + acc.0.round() as i32,
            origin.1 + acc.1.round() as i32,
        );
        assert_eq!(next, (602, 254));
        // 旧常数下这里只有 0.0156，连 round() 的 0.5 阈值都够不着
        assert!(fixed(256) >= 0.5, "1 px 鼠标位移必须够一轮取整");
    }
}
