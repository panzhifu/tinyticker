//! 软渲染：把 8x8 字形文本写入预乘 0xAARRGGBB 像素缓冲。

use crate::font8x8;

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

    pub fn fill(&mut self, color: u32) {
        self.buf.fill(color);
    }

    /// 在水平方向居中绘制一行文本；超界部分自动裁剪。
    pub fn draw_text_centered(&mut self, y: i32, text: &str, color: u32, scale: u32) {
        let width = text_width(text.chars().count(), scale);
        let x = ((self.width as i32 - width as i32) / 2).max(0);
        self.draw_text(x, y, text, color, scale);
    }

    /// 把一串 ASCII 文本写入像素缓冲；非 ASCII 字符跳过（内置字体只覆盖 ASCII）。
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, color: u32, scale: u32) {
        let mut cursor_x = x;
        for ch in text.chars() {
            let code = ch as usize;
            if code < 128 {
                self.draw_glyph(cursor_x, y, &font8x8::FONT8X8_BASIC[code], color, scale);
            }
            cursor_x += (8 * scale) as i32;
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

pub fn text_width(chars: usize, scale: u32) -> u32 {
    chars as u32 * 8 * scale
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

    #[test]
    fn text_width_counts_chars() {
        assert_eq!(text_width("1:00".chars().count(), 2), 4 * 8 * 2);
    }
}
