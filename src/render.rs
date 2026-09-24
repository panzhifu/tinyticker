//! 软渲染：把 8x8 字形文本写入预乘 0xAARRGGBB 像素缓冲。

use crate::text;

/// 配置/代码中的源色格式：0xRRGGBB（非预乘，不含 alpha）。
pub const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    (r << 16) | (g << 8) | b
}

/// 把 0xRRGGBB 源色与 alpha 合成为预乘 0xAARRGGBB。
/// Wayland ARGB shm 与 X11 depth-32 合成器均按预乘 alpha 解释。
pub const fn premultiply(color: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let r = ((color >> 16) & 0xFF) * a / 255;
    let g = ((color >> 8) & 0xFF) * a / 255;
    let b = (color & 0xFF) * a / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// 解析 `"#RRGGBB"` / `"0xRRGGBB"` / `"RRGGBB"` 颜色。
pub fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim();
    let hex = s
        .strip_prefix('#')
        .or_else(|| s.strip_prefix("0x"))
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// 秒数 → 显示文本：1 分钟内用 `"45s"`，之后用 `"m:ss"` / `"h:mm:ss"`。
pub fn format_time(secs: u32) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 画布：包装预乘 0xAARRGGBB 像素缓冲，提供文本绘制。
/// 写入的颜色应为 [`premultiply`] 的输出（文字用 0xFF alpha，背景用配置的透明度）。
pub struct Canvas<'a> {
    buf: &'a mut [u32],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u32], width: u32, height: u32) -> Self {
        Self { buf, width, height }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn fill(&mut self, color: u32) {
        self.buf.fill(color);
    }

    /// 预乘源覆盖（src-over）：`dst = src + dst × (1 - srcA)`。
    pub fn over(&mut self, x: i32, y: i32, src: u32) {
        self.blend(x, y, src, false);
    }

    /// 预乘源加法叠加：辉光与高光用，逐通道饱和到 255。
    pub fn add(&mut self, x: i32, y: i32, src: u32) {
        self.blend(x, y, src, true);
    }

    fn blend(&mut self, x: i32, y: i32, src: u32, additive: bool) {
        if x < 0 || y < 0 || (x as u32) >= self.width || (y as u32) >= self.height {
            return;
        }
        let idx = (y as u32 * self.width + x as u32) as usize;
        let dst = self.buf[idx];
        let sa = src >> 24;
        // 全透明的源什么都不留
        if sa == 0 {
            return;
        }
        // 不透明源直接覆盖，省掉四次乘除
        if sa >= 255 && !additive {
            self.buf[idx] = src;
            return;
        }
        let keep = 255 - sa.min(255);
        let mut out = 0u32;
        for ch in 0..4 {
            let shift = 24 - 8 * ch;
            let s = (src >> shift) & 0xFF;
            let d = (dst >> shift) & 0xFF;
            // 预乘空间里 src-over 就是 src + dst×(1-srcA)；alpha 通道同理
            let v = if additive { (s + d).min(255) } else { s + d * keep / 255 };
            out |= v << shift;
        }
        self.buf[idx] = out;
    }

    /// 在水平方向居中画一个已栅格化的行。
    /// `run.width` 是实测推进量，所以混排行（点阵 + TTF）也居得正。
    pub fn draw_run_centered(&mut self, run: &text::Run, color: u32) {
        let x = ((self.width as i32 - run.width as i32) / 2).max(0);
        self.draw_run(run, x, color);
    }

    /// 画一个已栅格化的行；`dx` 是整行的水平偏移（叠加在字形自带的 x 上）。
    pub fn draw_run(&mut self, run: &text::Run, dx: i32, color: u32) {
        for g in &run.glyphs {
            match &g.body {
                text::GlyphBody::Bits { bits, cell } => {
                    self.draw_glyph(g.x + dx, g.y, bits, color, *cell);
                }
                text::GlyphBody::Gray { gray, w, h, .. } => {
                    self.blit_gray(g.x + dx, g.y, gray, *w, *h, color);
                }
            }
        }
    }

    /// 逐像素覆盖度贴字：按覆盖度算 alpha 再 src-over。
    /// 这是"字形边缘有灰度"与"预乘 alpha 画布"之间唯一的桥。
    fn blit_gray(&mut self, x: i32, y: i32, gray: &[u8], w: u32, h: u32, color: u32) {
        // 传进来的 `color` 是文字色（alpha 恒 0xFF），只取它的源色再按覆盖度预乘。
        let src_color = color & 0xFF_FF_FF;
        for row in 0..h {
            for col in 0..w {
                let v = gray[(row * w + col) as usize];
                if v == 0 {
                    continue;
                }
                let src = premultiply(src_color, v);
                self.over(x + col as i32, y + row as i32, src);
            }
        }
    }

    fn draw_glyph(&mut self, x: i32, y: i32, glyph: &[u8; 8], color: u32, scale: u32) {
        for (row, &byte) in glyph.iter().enumerate() {
            for col in 0..8usize {
                // 位序：bit0 为最左列
                if byte & (1 << col) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = x + (col as u32 * scale + dx) as i32;
                            let py = y + (row as u32 * scale + dy) as i32;
                            if px >= 0 && py >= 0 && (px as u32) < self.width && (py as u32) < self.height {
                                self.buf[py as usize * self.width as usize + px as usize] = color;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0), "0s");
        assert_eq!(format_time(45), "45s");
        assert_eq!(format_time(59), "59s");
        assert_eq!(format_time(60), "1:00");
        assert_eq!(format_time(754), "12:34");
        assert_eq!(format_time(3599), "59:59");
        assert_eq!(format_time(3600), "1:00:00");
        assert_eq!(format_time(3661), "1:01:01");
    }

    #[test]
    fn premultiplication() {
        // 不透明 = 原色
        assert_eq!(premultiply(0x0F0F14, 0xFF), 0xFF0F0F14);
        // 半透明：各通道按 alpha 缩放（整数除法向下取整）
        assert_eq!(premultiply(0xFFFFFF, 0x80), 0x80808080);
        assert_eq!(premultiply(0xFF0000, 0x40), 0x40400000);
        assert_eq!(premultiply(0x100203, 0x80), 0x80080101);
        // 全透明 = 黑
        assert_eq!(premultiply(0xFFFFFF, 0), 0);
    }

    #[test]
    fn color_parsing() {        assert_eq!(parse_color("0F0F14"), Some(0x0F0F14));
        assert_eq!(parse_color("#FFFFFF"), Some(0xFFFFFF));
        assert_eq!(parse_color("0xffcc50"), Some(0xFFCC50));
        assert_eq!(parse_color(" 10aabb "), Some(0x10AABB));
        assert_eq!(parse_color("XYZ"), None);
        assert_eq!(parse_color("12345"), None);
        assert_eq!(parse_color("1234567"), None);
        assert_eq!(parse_color(""), None);
    }

    /// 点阵字行走的是新管道的快路：一格点阵放大成 `cell × cell` 的实心块，
    /// 所以一个字形至少留下 8×8×cell² 个不透明像素。这条守住"接 Run 之后画面没坏"。
    #[test]
    fn bitmap_run_paints_opaque_blocks() {
        for cell in [1u32, 3] {
            let run = text::shape("A", cell, 0);
            assert_eq!(run.width, 8 * cell);
            let mut buf = vec![0u32; (24 * 24) as usize];
            let mut c = Canvas::new(&mut buf, 24, 24);
            c.draw_run(&run, 0, 0xFF_FFFFFF);
            let painted = buf.iter().filter(|p| **p == 0xFF_FFFFFF).count();
            assert!(painted > 0, "cell={cell} 时一个字也没画出来");
            let whole = (cell * cell) as usize;
            assert_eq!(painted % whole, 0, "点阵必须整块放大");
        }
    }
}
