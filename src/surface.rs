//! 像素呈现后端：按显示协议选择 Wayland shm（ARGB，支持透明）或 softbuffer（X11）。
//!
//! 两个后端的像素格式统一为 **预乘 0xAARRGGBB**（高 8 位 alpha）：
//! - Wayland：`ARGB8888` shm 缓冲，协议规定预乘 alpha；
//! - X11：depth-32 visual 直通 alpha 字节，XRender/合成器同样按预乘解释
//!   （与 cairo 的 CAIRO_FORMAT_ARGB32 约定一致）。

use std::num::NonZeroU32;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

use crate::wayland::WaylandSurface;

/// 像素呈现后端。
pub trait PixelSurface {
    /// 更新缓冲尺寸（下一帧生效）。
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32);

    /// 取一帧缓冲交给 `paint` 绘制（写入预乘 0xAARRGGBB 像素），
    /// 闭包返回后立即呈现到屏幕；初始化失败时静默跳过本帧。
    fn draw_frame(&mut self, paint: &mut dyn FnMut(&mut [u32]));
}

/// 按窗口所在的显示协议创建呈现后端。
pub fn create(window: &Arc<Window>) -> Result<Box<dyn PixelSurface>, Box<dyn std::error::Error>> {
    match window.display_handle()?.as_raw() {
        RawDisplayHandle::Wayland(_) => Ok(Box::new(WaylandSurface::new(window)?)),
        #[cfg(feature = "x11")]
        RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => {
            Ok(Box::new(SoftbufferSurface::new(window)?))
        }
        other => Err(format!("不支持的显示后端: {other:?}").into()),
    }
}

/// X11 后端：softbuffer（depth-32 visual 下 alpha 字节直通合成器）。
#[cfg(feature = "x11")]
struct SoftbufferSurface {
    /// Surface 依赖 Context 存活，字段保活即可。
    _context: softbuffer::Context<Arc<Window>>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

#[cfg(feature = "x11")]
impl SoftbufferSurface {
    fn new(window: &Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        // softbuffer：用窗口自身充当 display handle（官方 winit 示例同款做法）
        let context = softbuffer::Context::new(window.clone())?;
        let surface = softbuffer::Surface::new(&context, window.clone())?;
        Ok(Self {
            _context: context,
            surface,
        })
    }
}

#[cfg(feature = "x11")]
impl PixelSurface for SoftbufferSurface {
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) {
        let _ = self.surface.resize(width, height);
    }

    fn draw_frame(&mut self, paint: &mut dyn FnMut(&mut [u32])) {
        let Ok(mut buffer) = self.surface.buffer_mut() else {
            return;
        };
        paint(&mut buffer);
        // present 消费 buffer，把缓冲内容写入窗口
        let _ = buffer.present();
    }
}
