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

/// CSS 颜色名。取值逐条照 Catime 的 `CSS_COLORS[]`（`src/color/color_parser.c:33-43`）
/// 抄——那 30 条就是它全部的名表，所以它配置里的颜色串能直接搬过来。比对时忽略
/// 大小写，所以 `Red` / `RED` / `red` 都收（它 `strcmp` 只认小写）。
///
/// 表压成一条 `"name=rrggbb …"` 而不是 30 个 `(&str, u32)`：后者每条要一对胖指针再
/// 加对齐，实测光那 30 条数组就多占 1.1 KB。
const NAMED: &str =
    "white=ffffff black=000000 red=ff0000 lime=00ff00 blue=0000ff yellow=ffff00 cyan=00ffff magenta=ff00ff silver=c0c0c0 gray=808080 maroon=800000 olive=808000 green=008000 purple=800080 teal=008080 navy=000080 orange=ffa500 pink=ffc0cb brown=a52a2a violet=ee82ee indigo=4b0082 gold=ffd700 coral=ff7f50 salmon=fa8072 khaki=f0e68c plum=dda0dd azure=f0ffff ivory=fffff0 wheat=f5deb3 snow=fffafa";

/// 解析颜色，四种写法都收：
/// - `#RRGGBB` / `0xRRGGBB` / 裸 6 位十六进制
/// - `#RGB` 三位的简写（每位翻倍展开，与 CSS 同规则）
/// - CSS 颜色名（那 30 条，大小写无关）
/// - `rgb(255, 94, 150)`，以及省掉前缀的裸三元组（分隔符见 [`parse_triplet`]）
///
/// 认不出来就返回 `None`，由配置层回落默认值。
pub fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(c) = named_color(s) {
        return Some(c);
    }
    if let Some(triplet) = parse_triplet(s) {
        return Some(triplet);
    }
    let hex = s
        .strip_prefix('#')
        .or_else(|| s.strip_prefix("0x"))
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    // 三位简写：`#f57` 就是 `#ff5577`，每位乘 17 正好是把那个半字节复制一遍
    if hex.len() == 3 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let nib = |i: usize| u32::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
        return Some(rgb(nib(0)?, nib(1)?, nib(2)?));
    }
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

/// 查 CSS 名表。大小写无关，靠比较时忽略而不是先转小写——转小写要为一次颜色解析
/// 分配一块 `String`。
fn named_color(s: &str) -> Option<u32> {
    NAMED.split(' ').find_map(|entry| {
        let (name, value) = entry.split_once('=')?;
        if !name.eq_ignore_ascii_case(s) {
            return None;
        }
        u32::from_str_radix(value, 16).ok()
    })
}

/// `rgb(r,g,b)` / `r,g,b` / `r g b`：三个 0-255 的十进制数。
///
/// 分隔符连全角逗号与分号都收，这是照 Catime 的口径（`color_parser.c:146` 那张
/// `separators[]` 表：`, ， ; ； 空格 |`）——同一句话在两边都能用才算对得上。
/// 没有分隔符也不带 `rgb` 前缀的串一律不算三元组，否则裸 6 位十六进制会被抢走。
fn parse_triplet(s: &str) -> Option<u32> {
    let body = match s.get(..4) {
        Some(p) if p.eq_ignore_ascii_case("rgb(") => s[4..].strip_suffix(')')?,
        _ => s,
    };
    // 一个分隔符都没有就不算三元组——那串该走十六进制那条路，
    // 否则裸 6 位（`102030`）会被当成"少写了分隔符的三元组"抢走
    if !body.contains([' ', '\t', ',', ';', '|', '，', '；']) {
        return None;
    }
    let mut parts = body
        .split([' ', '\t', ',', ';', '|', '，', '；'])
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.trim().parse::<u32>().ok());
    let r = parts.next()??;
    let g = parts.next()??;
    let b = parts.next()??;
    if parts.next().is_some() || r > 255 || g > 255 || b > 255 {
        return None;
    }
    Some(rgb(r, g, b))
}

/// 数字行的补零档位，对应配置项 `time_pad`（与 Catime 的三档 `TIME_FORMAT_*` 同形）。
///
/// 补零的另一半含义是**宽度固定**：`None` 档读数会从 `45s` 变成 `1:00`，整行左右
/// 抖动；`Zero` / `Full` 档不会，秒表与挂钟并排时更能看出谁在走。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pad {
    /// 最短：`45s` / `12:34` / `1:01:01`。v0.5.0 之前的行为，也是默认。
    #[default]
    None,
    /// 当前出现的单位各补两位：`00:45` / `12:34` / `01:01:01`。
    Zero,
    /// 永远 `h:mm:ss`：`00:00:45` / `00:12:34` / `01:01:01`。
    Full,
}

impl Pad {
    /// 配置值串；认 Catime 的 `none` / `zero` / `full`。
    pub fn from_name(name: &str) -> Option<Pad> {
        match name {
            "none" => Some(Pad::None),
            "zero" => Some(Pad::Zero),
            "full" => Some(Pad::Full),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Pad::None => "none",
            Pad::Zero => "zero",
            Pad::Full => "full",
        }
    }

    /// 托盘菜单上的标签。
    pub fn label(self) -> &'static str {
        match self {
            Pad::None => "不补零（45s）",
            Pad::Zero => "补两位（00:45）",
            Pad::Full => "总是时分秒（00:00:45）",
        }
    }
}

/// 补零档位的完整清单：托盘菜单与单测都从这里取，加一档不会漏登记。
pub const PADS: [Pad; 3] = [Pad::None, Pad::Zero, Pad::Full];

/// 秒（外加可选的百分位）→ 显示文本。三档补零见 [`Pad`]。
fn format_parts(total_secs: u32, cs: Option<u32>, pad: Pad) -> String {
    let frac = cs.map(|c| c % 100);
    // 不补零档在 1 分钟内走 `45s` / `45.32s` 这种带后缀的写法
    if pad == Pad::None && total_secs < 60 {
        return match frac {
            Some(f) => format!("{total_secs}.{f:02}s"),
            None => format!("{total_secs}s"),
        };
    }
    let (h, m, s) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    let tail = match frac {
        Some(f) => format!(".{f:02}"),
        None => String::new(),
    };
    match pad {
        Pad::Full => format!("{h:02}:{m:02}:{s:02}{tail}"),
        Pad::Zero if h > 0 => format!("{h:02}:{m:02}:{s:02}{tail}"),
        Pad::Zero => format!("{m:02}:{s:02}{tail}"),
        Pad::None if h > 0 => format!("{h}:{m:02}:{s:02}{tail}"),
        Pad::None => format!("{m}:{s:02}{tail}"),
    }
}

/// 秒数 → 显示文本：不补零档是 `45s` / `m:ss` / `h:mm:ss`，其余见 [`Pad`]。
pub fn format_time(secs: u32, pad: Pad) -> String {
    format_parts(secs, None, pad)
}

/// 百分之一秒数 → 显示文本：不补零档是 `45.32s` / `m:ss.cc` / `h:mm:ss.cc`。
///
/// 与 [`format_time`] 分开而不是加一个参数：隐藏百分秒时倒计时读的是向上取整的那一格
/// （`format_time`），显示百分秒时才向下取整到百分位——同一个数在这两种模式下
/// 本来就不该相等，合成一个函数只会把这件事藏起来。
pub fn format_centis(cs: u32, pad: Pad) -> String {
    format_parts(cs / 100, Some(cs % 100), pad)
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
        assert_eq!(format_time(0, Pad::None), "0s");
        assert_eq!(format_time(45, Pad::None), "45s");
        assert_eq!(format_time(59, Pad::None), "59s");
        assert_eq!(format_time(60, Pad::None), "1:00");
        assert_eq!(format_time(754, Pad::None), "12:34");
        assert_eq!(format_time(3599, Pad::None), "59:59");
        assert_eq!(format_time(3600, Pad::None), "1:00:00");
        assert_eq!(format_time(3661, Pad::None), "1:01:01");
    }

    /// 补零三档：宽度固定是这一档的实际目的，`s` 后缀随之去掉。
    #[test]
    fn padding_ladder() {
        for (secs, zero, full) in [
            (0, "00:00", "00:00:00"),
            (45, "00:45", "00:00:45"),
            (754, "12:34", "00:12:34"),
            (3661, "01:01:01", "01:01:01"),
        ] {
            assert_eq!(format_time(secs, Pad::Zero), zero, "{secs} 补两位不对");
            assert_eq!(format_time(secs, Pad::Full), full, "{secs} 全补不对");
        }
        // 百分秒档共用同一套补零
        assert_eq!(format_centis(4532, Pad::Zero), "00:45.32");
        assert_eq!(format_centis(4532, Pad::Full), "00:00:45.32");
        assert_eq!(format_centis(366101, Pad::Zero), "01:01:01.01");
    }

    /// 三档的值串都要能写回配置再读回来。
    #[test]
    fn pad_names_roundtrip() {
        for p in [Pad::None, Pad::Zero, Pad::Full] {
            assert_eq!(Pad::from_name(p.name()), Some(p));
            assert!(!p.label().is_empty());
        }
        assert_eq!(Pad::from_name("half"), None);
        assert_eq!(Pad::default(), Pad::None, "默认档必须是不改变旧行为的那个");
    }

    /// 百分秒档：秒位向下取整，百分位永远占两位。
    #[test]
    fn centisecond_formatting() {
        assert_eq!(format_centis(0, Pad::None), "0.00s");
        assert_eq!(format_centis(5, Pad::None), "0.05s");
        assert_eq!(format_centis(99, Pad::None), "0.99s");
        assert_eq!(format_centis(4532, Pad::None), "45.32s");
        assert_eq!(format_centis(5999, Pad::None), "59.99s");
        assert_eq!(format_centis(6000, Pad::None), "1:00.00");
        assert_eq!(format_centis(75423, Pad::None), "12:34.23");
        assert_eq!(format_centis(360000, Pad::None), "1:00:00.00");
        assert_eq!(format_centis(366101, Pad::None), "1:01:01.01");
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
    fn color_parsing() {
        assert_eq!(parse_color("0F0F14"), Some(0x0F0F14));
        assert_eq!(parse_color("#FFFFFF"), Some(0xFFFFFF));
        assert_eq!(parse_color("0xffcc50"), Some(0xFFCC50));
        assert_eq!(parse_color(" 10aabb "), Some(0x10AABB));
        assert_eq!(parse_color("XYZ"), None);
        assert_eq!(parse_color("12345"), None);
        assert_eq!(parse_color("1234567"), None);
        assert_eq!(parse_color(""), None);
    }

    /// 三位简写按 CSS 的规则每位翻倍，而不是左移补零——`#f57` 是 `#ff5577`。
    #[test]
    fn short_hex_expands_by_doubling_each_nibble() {
        assert_eq!(parse_color("#f57"), Some(0xFF5577));
        assert_eq!(parse_color("#000"), Some(0x000000));
        assert_eq!(parse_color("#fff"), Some(0xFFFFFF));
        assert_eq!(parse_color("#12g"), None);
    }

    /// 名表那 30 条是照 Catime 抄的，所以两边写出来的字面量必须能互相认。
    /// 我们比它宽一档：大小写无关（它 `strcmp` 只收小写）。
    #[test]
    fn css_names_are_case_insensitive() {
        assert_eq!(parse_color("red"), Some(0xFF0000));
        assert_eq!(parse_color("Red"), Some(0xFF0000));
        assert_eq!(parse_color("  GOLD "), Some(0xFFD700));
        assert_eq!(parse_color("tomato"), None, "名表只有那 30 条，不扩");
        // 名表里每一条都得能被自己解析回来，且不含 alpha 位
        for entry in NAMED.split(' ') {
            let (name, hex) = entry.split_once('=').expect("名表条目该是 name=rrggbb");
            let value = u32::from_str_radix(hex, 16).expect("值该是 6 位十六进制");
            assert_eq!(parse_color(name), Some(value), "{name} 解析错了");
            assert_eq!(value & 0xFF000000, 0, "{name} 不该带 alpha");
        }
    }

    /// `rgb()` 与裸三元组：分隔符收 `, ; | 空格` 与两个全角字符（Catime 的口径），
    /// 分量必须全在 0-255 且恰好三个。
    #[test]
    fn rgb_triplets_accept_the_same_separators_as_catime() {
        assert_eq!(parse_color("rgb(255,94,150)"), Some(0xFF5E96));
        assert_eq!(parse_color("rgb( 255 , 94 , 150 )"), Some(0xFF5E96));
        assert_eq!(parse_color("255,94,150"), Some(0xFF5E96));
        assert_eq!(parse_color("255 94 150"), Some(0xFF5E96));
        assert_eq!(parse_color("255|94|150"), Some(0xFF5E96));
        assert_eq!(parse_color("255；94；150"), Some(0xFF5E96), "全角分号");
        assert_eq!(parse_color("1，2，3"), Some(0x010203), "全角逗号");
        assert_eq!(parse_color("256,0,0"), None, "分量出界要拒，不能回绕");
        assert_eq!(parse_color("1,2"), None);
        assert_eq!(parse_color("1,2,3,4"), None);
        assert_eq!(parse_color("rgb(1,2,x)"), None);
        assert_eq!(parse_color("RGB(1,2,3)"), Some(0x010203), "前缀也大小写无关");
        assert_eq!(parse_color("rgba(1,2,3,255)"), None, "alpha 没地方放，宁可不收");
        // 关键取舍：不带分隔符也不带 `rgb` 前缀的串不许当三元组，
        // 否则裸 6 位十六进制会被抢走
        assert_eq!(parse_color("102030"), Some(0x102030));
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
