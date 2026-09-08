//! Wayland ARGB shm 呈现后端（支持透明）。
//!
//! 复用 winit 已连接的 wl_display（`Backend::from_foreign_display`，与其共享
//! 同一对象空间），从中恢复窗口的 `wl_surface`，再用 `wl_shm` 建立 ARGB8888
//! 双缓冲。像素格式为预乘 0xAARRGGBB（Wayland 协议规定 shm 缓冲使用预乘 alpha）。

use std::error::Error;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use memmap2::MmapMut;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use winit::window::Window;

use crate::surface::PixelSurface;

/// Wayland 呈现后端：ARGB shm 双缓冲。
pub struct WaylandSurface {
    /// 保活与 winit 共享的 wl_display 连接；必须最后 drop（晚于 surface/proxy）。
    #[allow(dead_code)]
    conn: Connection,
    event_queue: Mutex<EventQueue<State>>,
    qh: QueueHandle<State>,
    shm: wl_shm::WlShm,
    surface: wl_surface::WlSurface,
    /// 当前尺寸的 shm pool 与双缓冲；首次 resize 前为 None。
    pool: Option<Pool>,    /// 前台 buffer（合成器正在显示的那个）的下标。
    front: usize,
    /// 事件分发状态（release 标记通过 Arc 与本结构共享）。
    state: State,
}

/// 一组 shm 缓冲：一个 memfd + 一个 pool + 双 buffer。
struct Pool {
    pool: wl_shm_pool::WlShmPool,
    map: MmapMut,
    buffers: [wl_buffer::WlBuffer; 2],
    width: u32,
    height: u32,
}

/// 事件分发状态：仅记录两个 wl_buffer 的 release 事件。
struct State {
    released: [Arc<AtomicBool>; 2],
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// wl_shm / wl_shm_pool 无事件，仅需满足 bind/create_pool 的 trait 约束
impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            state.released[*index].store(true, Ordering::Relaxed);
        }
    }
}

impl WaylandSurface {
    /// 从 winit 窗口创建 Wayland 呈现后端。
    pub fn new(window: &Arc<Window>) -> Result<Self, Box<dyn Error>> {
        let RawDisplayHandle::Wayland(d) = window.display_handle()?.as_raw() else {
            return Err("非 Wayland 窗口".into());
        };
        let RawWindowHandle::Wayland(w) = window.window_handle()?.as_raw() else {
            return Err("非 Wayland 窗口".into());
        };

        // 共享 winit 的 wl_display：只有同一对象空间内才能恢复 winit 创建的 wl_surface。
        // 两个 Backend 都包裹同一个 wl_display（wayland-backend sys 后端）。
        let backend = unsafe { Backend::from_foreign_display(d.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, event_queue) = registry_queue_init::<State>(&conn)?;
        let qh = event_queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ())?;

        // 从裸指针恢复 winit 创建的 wl_surface
        let surface_id = unsafe {
            ObjectId::from_ptr(wl_surface::WlSurface::interface(), w.surface.as_ptr().cast())
        }?;
        let surface = wl_surface::WlSurface::from_id(&conn, surface_id)?;

        Ok(Self {
            conn,
            event_queue: Mutex::new(event_queue),
            qh,
            shm,
            surface,
            pool: None,
            front: 0,
            state: State {
                released: [Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(true))],
            },
        })
    }

    /// （重）建 shm pool 与双缓冲。
    fn build_pool(&mut self, width: u32, height: u32) -> Result<Pool, Box<dyn Error>> {
        let stride = width.checked_mul(4).ok_or("缓冲尺寸溢出")?;
        let half = stride.checked_mul(height).ok_or("缓冲尺寸溢出")?;
        let total = half.checked_mul(2).ok_or("缓冲尺寸溢出")?; // 双缓冲共享一个 pool

        let memfd = rustix::fs::memfd_create("tinyticker-shm", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&memfd, total as u64)?;
        let map = unsafe { MmapMut::map_mut(&memfd)? };

        let pool = self.shm.create_pool(memfd.as_fd(), total as i32, &self.qh, ());
        let buffers = [
            pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride as i32,
                wl_shm::Format::Argb8888,
                &self.qh,
                0,
            ),
            pool.create_buffer(
                half as i32,
                width as i32,
                height as i32,
                stride as i32,
                wl_shm::Format::Argb8888,
                &self.qh,
                1,
            ),
        ];
        Ok(Pool {
            pool,
            map,
            buffers,
            width,
            height,
        })
    }

    /// 第 `index` 块缓冲对应的像素切片（预乘 0xAARRGGBB）。
    fn slice_mut(&mut self, index: usize) -> &mut [u32] {
        let pool = self.pool.as_mut().expect("draw_frame called before resize");
        let stride = pool.width as usize * 4;
        let len = stride * pool.height as usize;
        bytemuck::cast_slice_mut(&mut pool.map[index * len..(index + 1) * len])
    }
}

impl PixelSurface for WaylandSurface {
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) {
        let (w, h) = (width.get(), height.get());
        if let Some(p) = &self.pool
            && p.width == w
            && p.height == h
        {
            return;
        }
        // 销毁旧 pool（发送 wl_buffer.destroy / wl_shm_pool.destroy 请求）
        if let Some(old) = self.pool.take() {
            old.buffers[0].destroy();
            old.buffers[1].destroy();
            old.pool.destroy();
        }
        match self.build_pool(w, h) {
            Ok(pool) => self.pool = Some(pool),
            Err(e) => eprintln!("⚠️ Wayland 缓冲重建失败: {e}"),
        }
    }

    fn draw_frame(&mut self, paint: &mut dyn FnMut(&mut [u32])) {
        if self.pool.is_none() {
            return;
        }

        // 处理攒下的 release 事件
        {
            let eq = self.event_queue.get_mut().unwrap();
            let _ = eq.dispatch_pending(&mut self.state);
        }

        let back = self.front ^ 1;
        // 后备未释放而前台已释放 → 直接重绘前台，避免覆写合成器还在读的缓冲
        if !self.state.released[back].load(Ordering::Relaxed)
            && self.state.released[self.front].load(Ordering::Relaxed)
        {
            self.front = back;
        }
        let index = self.front ^ 1;

        paint(self.slice_mut(index));
        self.state.released[index].store(false, Ordering::Relaxed);
        self.front = index;

        // attach + damage + commit 呈现
        let pool = self.pool.as_ref().unwrap();
        let (w, h) = (pool.width as i32, pool.height as i32);
        self.surface.attach(Some(&pool.buffers[index]), 0, 0);
        // damage_buffer 需要 wl_surface v4+；旧版本退化为 surface 坐标 damage
        if self.surface.version() >= 4 {
            self.surface.damage_buffer(0, 0, w, h);
        } else {
            self.surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        self.surface.commit();

        let _ = self.event_queue.lock().unwrap().flush();
    }
}
