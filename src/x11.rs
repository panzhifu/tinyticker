//! 自研 X11 悬浮挂件：直调系统 libX11（声明见 `src/sys/x11.rs`）。
//!
//! 与 [`crate::wl`] 一样不经过任何 Rust GUI 库：建一个 override-redirect 的
//! depth-32 ARGB 窗口，用 `XPutImage` 软渲染，拖动/滚轮/右键全部自己处理。
//!
//! 为什么用 override-redirect：无边框、不被窗口管理器重排、位置精确可控，
//! 这三件事靠 `_MOTIF_WM_HINTS` + `_NET_WM_STATE_ABOVE` 只能"请求"，各 WM 行为不一。
//! 代价是它不进 WM 的层叠序列——所以另外监听根窗口的 `SubstructureNotify`，
//! 一旦别的窗口 map/configure 就把自己 raise 回去，压住全屏窗口。
//!
//! 透明靠 32 位 ARGB visual + 预乘像素（alpha 字节直通合成器）；没有 32 位
//! visual 或没开合成器时退化为不透明背景，功能不受影响。

use std::ffi::{CStr, CString, c_char, c_int, c_short, c_uint, c_void};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::config::Config;
use crate::render::Canvas;
use crate::sys::x11 as x;
use crate::sys::{POLL_IN, PollFd, poll};
use crate::sys::x11::{Event, Rectangle, VisualInfo, X11, XExt};
use crate::tray::{Command, TrayHandle};
use crate::widget::{DRAW_INTERVAL, LOGICAL_SIZE, Widget};

/// 没有历史位置时的初始坐标（逻辑像素，距左上角），与 layer-shell 路径一致。
const DEFAULT_POS: (i32, i32) = (80, 80);
/// XPutImage 的 bitmap_pad：32 位像素。
const BITMAP_PAD: c_int = 32;
/// Shape 的 rectangle 顺序：不排序。
const ORDER_UNSORTED: c_int = 0;
/// 窗口被拖出屏幕后仍留这么多像素可见，免得抓不回来。
const SLACK: i32 = 24;


/// Xlib 出错时的默认处理器会打印并 `exit()`：一个异步的 BadWindow 就能杀掉挂件，
/// 太脆。换成只记录不换实现——注意 X 的错误是**异步**回报的，请求本身已经返回了，
/// 所以这里必须打印，否则错误就彻底消失了。
unsafe extern "C" fn log_error(_dpy: *mut x::Display, ev: *mut c_void) -> c_int {
    let e = unsafe { &*(ev as *const x::ErrorEvent) };
    // request_code 就是失败请求的 major opcode，够定位问题了
    eprintln!(
        "⚠️ X 请求失败: {} (code={}) request={} minor={} res=0x{:x}",
        x::error_name(e.error_code),
        e.error_code,
        e.request_code,
        e.minor_code,
        e.resourceid
    );
    0
}

pub struct Client {
    x: &'static X11,
    /// libXext 不在或服务器没有 SHAPE 扩展时只是没有点击穿透。
    ext: Option<&'static XExt>,
    dpy: *mut x::Display,
    screen: c_int,
    win: x::Window,
    visual: *mut x::Visual,
    depth: c_int,
    gc: *mut x::GC,
    widget: Widget,
    /// 缩放系数（`Xft.dpi / 96`，至少 1）。
    sf: f32,
    /// 窗口物理像素尺寸与位置（位置是根窗口坐标）。
    size: (u32, u32),
    pos: (i32, i32),
    /// 左键按下时指针在窗口内的偏移；`Some` 表示正在拖动。
    drag: Option<(i32, i32)>,
    /// 像素缓冲与指向它的 XImage。XImage 只在改尺寸时重建。
    buf: Vec<u32>,
    image: *mut c_void,
    /// 已下发的输入区域，避免每帧重复 Shape 请求。
    last_input: Option<(i32, i32, i32, i32)>,
    quit: bool,
}

impl Client {
    fn new(config: Config, autostart: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let x11: &'static X11 = Box::leak(Box::new(X11::load().ok_or("打不开 libX11.so.6")?));
        let dpy = unsafe { (x11.XOpenDisplay)(std::ptr::null()) };
        if dpy.is_null() {
            return Err("连不上 X server（DISPLAY 无效？）".into());
        }
        unsafe { (x11.XSetErrorHandler)(Some(log_error)) };

        let screen = unsafe { (x11.XDefaultScreen)(dpy) };
        let root = unsafe { (x11.XDefaultRootWindow)(dpy) };
        let (visual, depth) = pick_visual(x11, dpy, screen);
        let colormap = unsafe { (x11.XCreateColormap)(dpy, root, visual, x::ALLOC_NONE) };
        let ext = pick_shape(x11, dpy);

        let widget = Widget::new(config, autostart);
        let sf = scale_factor(x11, dpy);
        let size = physical_size(widget.zoom, sf);
        let logical = widget.config.window_pos.unwrap_or(DEFAULT_POS);
        let pos = clamp(
            ((logical.0 as f32 * sf).round() as i32, (logical.1 as f32 * sf).round() as i32),
            size,
            screen_size(x11, dpy, screen),
        );

        // 不显式给光标的窗口会去继承父窗口的：合成器/WM 没设根光标时，
        // 指针悬在挂件上会显示成服务器内置的怪光标，所以自己建一个左箭头。
        let cursor = unsafe { (x11.XCreateFontCursor)(dpy, x::XC_LEFT_PTR) };
        let mut attrs = x::SetWindowAttributes {
            // 背景/边框像素 0 = 预乘后的全透明
            background_pixmap: x::NONE_PIXMAP,
            background_pixel: 0,
            border_pixel: 0,
            override_redirect: 1,
            colormap,
            cursor,
            event_mask: x::BUTTON_PRESS_MASK
                | x::BUTTON_RELEASE_MASK
                | x::POINTER_MOTION_MASK
                | x::EXPOSURE_MASK
                | x::STRUCTURE_NOTIFY_MASK,
            ..Default::default()
        };
        // 属性位的选择是实测钉下来的：background_pixmap 必须显式给 None（不给就是
        // CopyFromParent，而根窗口是 24 位，直接 BadMatch）；反过来 CWBorderPixmap
        // 一旦给出，值为 0 会被服务器当回 CopyFromParent，同样 BadMatch——只给
        // border_pixel 就行。colormap 位也必须给，否则用根窗口的默认 colormap。
        let mask = x::CW_BACK_PIXMAP
            | x::CW_BACK_PIXEL
            | x::CW_BORDER_PIXEL
            | x::CW_COLORMAP
            | x::CW_CURSOR
            | x::CW_OVERRIDE_REDIRECT
            | x::CW_EVENT_MASK;
        let win = unsafe {
            (x11.XCreateWindow)(
                dpy,
                root,
                pos.0,
                pos.1,
                size.0,
                size.1,
                0,
                depth,
                x::INPUT_OUTPUT as c_uint,
                visual,
                mask,
                &mut attrs,
            )
        };
        if win == 0 {
            return Err("创建 X 窗口失败".into());
        }
        let name = CString::new("TinyTicker").unwrap();
        unsafe { (x11.XStoreName)(dpy, win, name.as_ptr()) };
        // 根窗口只听子窗口结构变化：别人 map/configure（含全屏）时我们要重新压上去
        unsafe { (x11.XSelectInput)(dpy, root, x::SUBSTRUCTURE_NOTIFY_MASK) };
        unsafe { (x11.XMapRaised)(dpy, win) };
        unsafe { (x11.XSync)(dpy, 0) };

        let gc = unsafe { (x11.XCreateGC)(dpy, win, 0, std::ptr::null_mut()) };
        if gc.is_null() {
            return Err("创建 GC 失败".into());
        }

        let mut client = Self {
            x: x11,
            ext,
            dpy,
            screen,
            win,
            visual,
            depth,
            gc,
            widget,
            sf,
            size,
            pos,
            drag: None,
            buf: Vec::new(),
            image: std::ptr::null_mut(),
            last_input: None,
            quit: false,
        };
        client.rebuild_image();
        Ok(client)
    }

    /// 按当前尺寸重建像素缓冲与 XImage。缓冲由我们持有，所以**不能**调
    /// `XDestroyImage`（那是个宏，会去 free 我们的 Rust 堆块）；旧结构体泄漏。
    fn rebuild_image(&mut self) {
        let (w, h) = self.size;
        let mut buf = vec![0u32; (w * h) as usize];
        let image = unsafe {
            (self.x.XCreateImage)(
                self.dpy,
                self.visual,
                self.depth as c_uint,
                x::Z_PIXMAP,
                0,
                buf.as_mut_ptr() as *mut c_char,
                w,
                h,
                BITMAP_PAD,
                (w as usize * 4) as c_int,
            )
        };
        if image.is_null() {
            self.quit = true;
            return;
        }
        self.buf = buf;
        self.image = image;
        self.widget.invalidate();
    }

    /// 缩放倍数变了就改窗口尺寸。真实的尺寸/位置以 ConfigureNotify 为准。
    fn sync_size(&mut self) {
        let want = physical_size(self.widget.zoom, self.sf);
        if want == self.size {
            return;
        }
        self.size = want;
        self.pos = clamp(self.pos, want, self.screen());
        unsafe {
            (self.x.XResizeWindow)(self.dpy, self.win, want.0, want.1);
            (self.x.XMoveWindow)(self.dpy, self.win, self.pos.0, self.pos.1);
        }
        self.rebuild_image();
    }

    /// 画一帧：内容没变就整帧跳过（脏检查在 `Widget::build_frame` 里）。
    fn draw(&mut self) {
        let (w, h) = self.size;
        if self.image.is_null() || w == 0 || h == 0 {
            return;
        }
        let scale = ((self.sf * self.widget.zoom).round() as u32).max(1);
        let Some(frame) = self.widget.build_frame(w, h, scale) else {
            return;
        };
        {
            let mut canvas = Canvas::new(&mut self.buf, w, h);
            frame.paint(&mut canvas);
        }
        unsafe { (self.x.XPutImage)(self.dpy, self.win, self.gc, self.image, 0, 0, 0, 0, w, h) };
        // 输入区域：默认整窗接收；开启点击穿透后收缩到文字包围盒
        let rect = frame.input_rect(self.sf).unwrap_or((0, 0, w as i32, h as i32));
        self.apply_input_rect(rect);
    }

    fn apply_input_rect(&mut self, rect: (i32, i32, i32, i32)) {
        if self.last_input == Some(rect) {
            return;
        }
        let Some(ext) = self.ext else { return };
        let mut r = Rectangle {
            x: rect.0 as c_short,
            y: rect.1 as c_short,
            width: rect.2.max(1) as c_short,
            height: rect.3.max(1) as c_short,
        };
        unsafe {
            (ext.XShapeCombineRectangles)(
                self.dpy,
                self.win,
                x::SHAPE_INPUT,
                0,
                0,
                &mut r,
                1,
                x::SHAPE_SET,
                ORDER_UNSORTED,
            )
        };
        self.last_input = Some(rect);
    }

    /// 取走所有已到达的 X 事件（`XPending` 保证 `XNextEvent` 不阻塞）。
    fn pump_events(&mut self) {
        while unsafe { (self.x.XPending)(self.dpy) } > 0 {
            let mut ev: Event = unsafe { std::mem::zeroed() };
            if unsafe { (self.x.XNextEvent)(self.dpy, &mut ev) } < 0 {
                self.quit = true;
                return;
            }
            self.handle_event(&ev);
        }
    }

    fn handle_event(&mut self, ev: &Event) {
        let any = x::event::<x::AnyEvent>(ev);
        match ev.kind() {
            x::BUTTON_PRESS => {
                let b = x::event::<x::ButtonEvent>(ev);
                match b.button {
                    x::BTN_1 => self.begin_drag(b.x, b.y),
                    x::BTN_3 => self.quit = true,
                    x::BTN_WHEEL_UP => self.zoom(1.0),
                    x::BTN_WHEEL_DOWN => self.zoom(-1.0),
                    _ => {}
                }
            }
            x::BUTTON_RELEASE => {
                let b = x::event::<x::ButtonEvent>(ev);
                if b.button == x::BTN_1 {
                    self.end_drag(b.time);
                }
            }
            x::MOTION_NOTIFY => {
                let Some((dx, dy)) = self.drag else { return };
                let m = x::event::<x::MotionEvent>(ev);
                let next = (m.x_root - dx, m.y_root - dy);
                if next != self.pos {
                    self.pos = clamp(next, self.size, self.screen());
                    unsafe { (self.x.XMoveWindow)(self.dpy, self.win, self.pos.0, self.pos.1) };
                }
            }
            x::EXPOSE => {
                if any.window == self.win {
                    self.widget.invalidate();
                }
            }
            x::CONFIGURE_NOTIFY => {
                let c = x::event::<x::ConfigureEvent>(ev);
                if c.window == self.win {
                    self.pos = (c.x, c.y);
                    let size = (c.width.max(1) as u32, c.height.max(1) as u32);
                    if size != self.size {
                        self.size = size;
                        self.rebuild_image();
                    }
                    self.widget.invalidate();
                } else {
                    // 别的窗口动了（含全屏）：重新压到最上面
                    unsafe { (self.x.XRaiseWindow)(self.dpy, self.win) };
                }
            }
            // 别的窗口出现/换父/重排（含全屏）：重新压到最上面。自己的 map 带
            // window == win，必须排除，否则自己跟自己刷。
            x::MAP_NOTIFY | x::CREATE_NOTIFY | x::REPARENT_NOTIFY
                if x::event::<x::ParentWindowEvent>(ev).window != self.win =>
            {
                unsafe { (self.x.XRaiseWindow)(self.dpy, self.win) };
            }
            _ => {}
        }
    }

    /// 按下左键：记下指针在窗口内的偏移，并 grab 指针——否则指针滑出窗口
    /// 就没有 MotionNotify 了，拖动会断。
    fn begin_drag(&mut self, x_in: c_int, y_in: c_int) {
        self.drag = Some((x_in, y_in));
        unsafe { (self.x.XSync)(self.dpy, 0) };
        unsafe {
            (self.x.XGrabPointer)(
                self.dpy,
                self.win,
                1,
                (x::POINTER_MOTION_MASK | x::BUTTON_RELEASE_MASK) as c_uint,
                x::GRAB_MODE_ASYNC,
                x::GRAB_MODE_ASYNC,
                0,
                0,
                x::CURRENT_TIME,
            )
        };
    }

    fn end_drag(&mut self, time: x::Time) {
        if self.drag.take().is_some() {
            unsafe { (self.x.XUngrabPointer)(self.dpy, time) };
        }
    }

    fn zoom(&mut self, dy: f32) {
        if self.widget.zoom_by(dy) {
            self.last_input = None; // 尺寸要变，输入区域得重新下发
        }
    }

    fn screen(&self) -> (u32, u32) {
        screen_size(self.x, self.dpy, self.screen)
    }

    /// 事件 + 心跳：每 `DRAW_INTERVAL` 推进一次，其余时间阻塞在 X 的连接 fd 上。
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
            self.widget.tick();
            self.pump_events();
            self.sync_size();
            self.draw();
            unsafe { (self.x.XFlush)(self.dpy) };

            let deadline = Instant::now() + DRAW_INTERVAL;
            // 队列里已有事件就别白等
            if unsafe { (self.x.XPending)(self.dpy) } == 0 {
                let fd = unsafe { (self.x.XConnectionNumber)(self.dpy) };
                let ms = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(c_int::MAX as u128) as c_int;
                let mut pfd = [PollFd { fd, events: POLL_IN, revents: 0 }];
                unsafe { poll(pfd.as_mut_ptr(), 1, ms) };
            }
        }
        // 配置里统一存逻辑像素，与 layer-shell 路径保持一致
        let logical =
            ((self.pos.0 as f32 / self.sf).round() as i32, (self.pos.1 as f32 / self.sf).round() as i32);
        self.widget.persist(Some(logical));
        unsafe { (self.x.XCloseDisplay)(self.dpy) };
        Ok(())
    }
}

/// 创建并运行 X11 挂件。
pub fn run(
    cmd_rx: &Receiver<Command>,
    handle_rx: &Receiver<TrayHandle>,
    config: Config,
    autostart: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(config, autostart)?;
    client.event_loop(cmd_rx, handle_rx)
}

/// 优先 32 位 TrueColor（alpha 通道给合成器），拿不到就退回默认 visual。
/// 只接受标准 0x00ff0000 / 0x0000ff00 / 0x000000ff 掩码——我们的像素就是这个
/// 排布，掩码不同宁可退化成不透明，也不要显示成怪色。
fn pick_visual(x11: &X11, dpy: *mut x::Display, screen: c_int) -> (*mut x::Visual, c_int) {
    let mut info: VisualInfo = unsafe { std::mem::zeroed() };
    let ok = unsafe { (x11.XMatchVisualInfo)(dpy, screen, 32, x::TRUE_COLOR, &mut info) } != 0;
    let standard = ok
        && !info.visual.is_null()
        && info.red_mask == 0x00ff_0000
        && info.green_mask == 0x0000_ff00
        && info.blue_mask == 0x0000_00ff;
    if standard {
        return (info.visual, 32);
    }
    let depth = unsafe { (x11.XDefaultDepth)(dpy, screen) };
    unsafe { ((x11.XDefaultVisual)(dpy, screen), depth) }
}

/// libXext 可加载且服务器真有 SHAPE 扩展时才返回 Some。
fn pick_shape(x11: &X11, dpy: *mut x::Display) -> Option<&'static XExt> {
    let ext: &'static XExt = Box::leak(Box::new(XExt::load()?));
    let name = CString::new("SHAPE").ok()?;
    let (mut major, mut first_ev, mut first_err) = (0, 0, 0);
    let present =
        unsafe { (x11.XQueryExtension)(dpy, name.as_ptr(), &mut major, &mut first_ev, &mut first_err) }
            != 0;
    present.then_some(ext)
}

/// `Xft.dpi / 96`。X11 没有分数缩放协议，DPI 资源是唯一线索；拿不到按 1 算。
fn scale_factor(x11: &X11, dpy: *mut x::Display) -> f32 {
    let p = unsafe { (x11.XResourceManagerString)(dpy) };
    if p.is_null() {
        return 1.0;
    }
    let text = unsafe { CStr::from_ptr(p) }.to_bytes();
    let Some(at) = text.windows(8).position(|w| w == b"Xft.dpi:") else {
        return 1.0;
    };
    let rest = &text[at + 8..];
    let end = rest
        .iter()
        .position(|c| !(c.is_ascii_whitespace() || c.is_ascii_digit() || *c == b'.'))
        .unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end])
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|dpi| *dpi > 0.0)
        .map_or(1.0, |dpi| (dpi / 96.0).clamp(1.0, 4.0))
}

fn screen_size(x11: &X11, dpy: *mut x::Display, screen: c_int) -> (u32, u32) {
    let w = unsafe { (x11.XDisplayWidth)(dpy, screen) }.max(1) as u32;
    let h = unsafe { (x11.XDisplayHeight)(dpy, screen) }.max(1) as u32;
    (w, h)
}

/// 窗口物理尺寸：逻辑尺寸 × 缩放倍数 × DPI 系数。
fn physical_size(zoom: f32, sf: f32) -> (u32, u32) {
    (
        (LOGICAL_SIZE.0 as f32 * zoom * sf).round().max(1.0) as u32,
        (LOGICAL_SIZE.1 as f32 * zoom * sf).round().max(1.0) as u32,
    )
}

/// 把窗口左上角限制在屏幕内（至少留 [`SLACK`] 像素可见，免得整块拖出屏幕）。
fn clamp(pos: (i32, i32), size: (u32, u32), screen: (u32, u32)) -> (i32, i32) {
    let maxx = screen.0 as i32 - SLACK;
    let maxy = screen.1 as i32 - SLACK;
    let minx = SLACK - size.0 as i32;
    let miny = SLACK - size.1 as i32;
    (pos.0.clamp(minx, maxx), pos.1.clamp(miny, maxy))
}
