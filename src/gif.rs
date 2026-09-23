//! 最小 GIF89a 解码器：只够把动图当托盘图标用。
//!
//! 输入是用户指定的文件，属于不可信数据，所以：
//! - 任何越界或畸形结构一律返回 `None`，**不 panic**；
//! - 画布像素数与帧数都有上限，防着 65535x65535 或上千帧的文件吃光内存；
//! - 颜色表条目数是 `2^(N+1)` 而不是 `2^(N+2)`——多算一倍会把后面每个块的
//!   偏移都带偏，且错得很安静。
//!
//! 分两步：先把每个图像块原样解析出来（子矩形 + 索引 + 调色板 + 处置方式），
//! 再顺序合成为整幅画面。合成交给调用方的帧已经是完整 RGBA，调用方不需要懂
//! 子矩形与回滚。

use crate::config;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// 画布像素上限。托盘图标只有 32x32，更大的图拒收。
const MAX_PIXELS: u32 = 128 * 128;
/// 帧数上限。
const MAX_FRAMES: usize = 64;
/// LZW 码最长 12 位，字典因此封顶。
const MAX_CODE: usize = 4096;
const NO_PREFIX: u16 = u16::MAX;

pub struct Frame {
    /// 整幅画布的 RGBA，长度 = 画布宽 × 高 × 4（尺寸在 [`Animation`] 上）
    pub rgba: Vec<u8>,
    pub delay: Duration,
}

pub struct Animation {
    pub width: u16,
    pub height: u16,
    pub frames: Vec<Frame>,
}

impl Animation {
    /// 播放到 `elapsed` 时刻该显示第几帧（循环播放）。
    pub fn frame_at(&self, elapsed: Duration) -> usize {
        if self.frames.len() <= 1 {
            return 0;
        }
        // Duration 不支持取余，一律换成毫秒整数
        let ms = |d: Duration| d.as_millis() as u64;
        let total: u64 = self.frames.iter().map(|f| ms(f.delay)).sum();
        if total == 0 {
            return 0;
        }
        let mut t = ms(elapsed) % total;
        for (i, f) in self.frames.iter().enumerate() {
            let d = ms(f.delay);
            if t < d {
                return i;
            }
            t -= d;
        }
        self.frames.len() - 1
    }
}

/// 一个图像块的原样内容（尚未合成）。
struct Raw {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    interlaced: bool,
    /// 行主序的颜色索引，长度 = width × height
    indices: Vec<u8>,
    palette: Vec<u8>,
    transparent: Option<u8>,
    delay: Duration,
    /// 0/1 原地留着，2 退回背景，3 退回画之前
    disposal: u8,
}

/// 单个 GIF 的文件大小上限。动图比文本大得多，但仍要挡住随手指向一个巨型文件。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 一个动图文件 + 播放进度。文件换了就重新解码。
pub struct Player {
    path: Option<PathBuf>,
    stamp: Option<(u64, SystemTime)>,
    anim: Option<Animation>,
    started: Instant,
}

impl Player {
    pub fn new(raw: Option<&str>) -> Self {
        Self {
            path: raw.map(config::expand_tilde).filter(|p| !p.as_os_str().is_empty()),
            stamp: None,
            anim: None,
            started: Instant::now(),
        }
    }

    /// 当前该显示的一帧及其画布尺寸；文件不可用或解不出时 `None`（调用方该退回静态图标）。
    pub fn current(&mut self) -> Option<(&Frame, u16, u16)> {
        let path = self.path.as_ref()?;
        if let Ok(meta) = fs::metadata(path) {
            let stamp = (meta.len(), meta.modified().ok()?);
            if Some(stamp) != self.stamp {
                self.stamp = Some(stamp);
                self.anim = None;
                self.started = Instant::now();
                if meta.len() > MAX_FILE_BYTES {
                    eprintln!("⚠️ tray_gif 超过 {MAX_FILE_BYTES} 字节，忽略：{}", path.display());
                } else if let Some(a) = fs::read(path).ok().as_deref().and_then(decode) {
                    self.anim = Some(a);
                }
            }
        } else {
            self.stamp = None;
            self.anim = None;
        }
        let anim = self.anim.as_ref()?;
        let frame = anim.frames.get(anim.frame_at(self.started.elapsed())).or(anim.frames.first())?;
        Some((frame, anim.width, anim.height))
    }
}

pub fn decode(bytes: &[u8]) -> Option<Animation> {
    if bytes.len() < 13 || &bytes[..6] != b"GIF89a" && &bytes[..6] != b"GIF87a" {
        return None;
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]);
    let height = u16::from_le_bytes([bytes[8], bytes[9]]);
    let packed = bytes[10];
    if width == 0 || height == 0 || u32::from(width) * u32::from(height) > MAX_PIXELS {
        return None;
    }
    let mut pos: usize = 13;
    let global = if packed & 0x80 != 0 {
        let n = table_bytes(packed);
        let t = bytes.get(pos..pos.checked_add(n)?)?.to_vec();
        pos += n;
        t
    } else {
        Vec::new()
    };

    let mut raws: Vec<Raw> = Vec::new();
    let mut gce = (Duration::from_millis(0), 0u8, None::<u8>);
    while pos < bytes.len() {
        match bytes[pos] {
            0x3b => break,
            0x21 if bytes.get(pos + 1) == Some(&0xf9) => {
                gce = parse_gce(bytes, pos)?;
                pos = skip_subblocks(bytes, pos + 2)?;
            }
            0x21 => pos = skip_subblocks(bytes, pos + 2)?,
            0x2c => {
                if raws.len() >= MAX_FRAMES {
                    break;
                }
                let (raw, next) = parse_image(bytes, pos, &global, width, height, gce)?;
                raws.push(raw);
                pos = next;
                gce = (Duration::from_millis(0), 0, None);
            }
            _ => return None,
        }
    }
    if raws.is_empty() {
        return None;
    }
    Some(composite(width, height, raws))
}

/// 把子矩形帧逐帧合成成整幅画面。
fn composite(width: u16, height: u16, raws: Vec<Raw>) -> Animation {
    let area = usize::from(width) * usize::from(height) * 4;
    let mut canvas = vec![0u8; area];
    let mut frames = Vec::with_capacity(raws.len());
    for raw in raws {
        // 处置方式作用于「本帧显示之后」，而快照此刻已经取好，所以立刻执行等价
        let saved = if raw.disposal == 3 { Some(region(&canvas, &raw, width)) } else { None };
        draw(&mut canvas, &raw, width, height);
        frames.push(Frame { rgba: canvas.clone(), delay: raw.delay });
        match raw.disposal {
            2 => erase(&mut canvas, &raw, width),
            3 => {
                if let Some(buf) = saved {
                    restore(&mut canvas, &buf, &raw, width);
                }
            }
            _ => {}
        }
    }
    Animation { width, height, frames }
}

fn draw(canvas: &mut [u8], raw: &Raw, canvas_w: u16, canvas_h: u16) {
    let w = usize::from(raw.width);
    for (i, &index) in raw.indices.iter().enumerate() {
        if Some(index) == raw.transparent {
            continue;
        }
        let entry = usize::from(index) * 3;
        let Some(rgb) = raw.palette.get(entry..entry + 3) else { continue };
        let (sx, sy) = map_pixel(i, w, usize::from(raw.height), raw.interlaced);
        let dx = usize::from(raw.left) + sx;
        let dy = usize::from(raw.top) + sy;
        if dx >= usize::from(canvas_w) || dy >= usize::from(canvas_h) {
            continue;
        }
        let d = (dy * usize::from(canvas_w) + dx) * 4;
        canvas[d] = rgb[0];
        canvas[d + 1] = rgb[1];
        canvas[d + 2] = rgb[2];
        canvas[d + 3] = 255;
    }
}

/// 索引序 → 画布内坐标；交错图要按 4 趟把行号重排回去。
fn map_pixel(i: usize, w: usize, h: usize, interlaced: bool) -> (usize, usize) {
    let row = i / w;
    let x = i % w;
    if !interlaced || row >= h {
        return (x, row.min(h.saturating_sub(1)));
    }
    (x, interlaced_row(row, h))
}

/// 交错 GIF 的 4 趟行序：起点 0/4/2/1，步长 8/8/4/2。
fn interlaced_row(row: usize, height: usize) -> usize {
    const START: [usize; 4] = [0, 4, 2, 1];
    const STEP: [usize; 4] = [8, 8, 4, 2];
    let mut r = row;
    for pass in 0..4 {
        if START[pass] >= height {
            continue;
        }
        let rows = (height - START[pass]).div_ceil(STEP[pass]);
        if r < rows {
            return START[pass] + r * STEP[pass];
        }
        r -= rows;
    }
    row
}

fn region(canvas: &[u8], raw: &Raw, canvas_w: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::from(raw.width) * usize::from(raw.height) * 4);
    for y in 0..usize::from(raw.height) {
        for x in 0..usize::from(raw.width) {
            let s = ((usize::from(raw.top) + y) * usize::from(canvas_w) + usize::from(raw.left) + x) * 4;
            out.extend_from_slice(canvas.get(s..s + 4).unwrap_or(&[0; 4]));
        }
    }
    out
}

fn restore(canvas: &mut [u8], buf: &[u8], raw: &Raw, canvas_w: u16) {
    for y in 0..usize::from(raw.height) {
        for x in 0..usize::from(raw.width) {
            let s = (y * usize::from(raw.width) + x) * 4;
            let d = ((usize::from(raw.top) + y) * usize::from(canvas_w) + usize::from(raw.left) + x) * 4;
            let Some(px) = buf.get(s..s + 4) else { continue };
            if let Some(slot) = canvas.get_mut(d..d + 4) {
                slot.copy_from_slice(px);
            }
        }
    }
}

/// disposal=2：本帧区域退回背景。本项目的背景是全透明。
fn erase(canvas: &mut [u8], raw: &Raw, canvas_w: u16) {
    for y in 0..usize::from(raw.height) {
        for x in 0..usize::from(raw.width) {
            let d = ((usize::from(raw.top) + y) * usize::from(canvas_w) + usize::from(raw.left) + x) * 4;
            if let Some(slot) = canvas.get_mut(d..d + 4) {
                slot.copy_from_slice(&[0; 4]);
            }
        }
    }
}

/// 颜色表字节数：条目数 `2^(N+1)`，每项 RGB 3 字节。
fn table_bytes(packed: u8) -> usize {
    (1 << ((packed & 7) + 1)) * 3
}

fn skip_subblocks(bytes: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let n = *bytes.get(pos)?;
        if n == 0 {
            return Some(pos + 1);
        }
        pos += usize::from(n) + 1;
    }
}

/// `(delay, disposal, transparent)`
fn parse_gce(bytes: &[u8], pos: usize) -> Option<(Duration, u8, Option<u8>)> {
    // 21 F9 <bs=4> <packed> <delay lo> <delay hi> <trans> <term>
    let b = bytes.get(pos + 2..pos + 8)?;
    if b[0] < 4 {
        return None;
    }
    let cs = u32::from(b[2]) | (u32::from(b[3]) << 8);
    // delay=0 的 GIF 满屏都是，按老规矩当 100ms
    let delay = if cs == 0 { Duration::from_millis(100) } else { Duration::from_millis(u64::from(cs) * 10) };
    Some((delay, b[1] & 7, if b[1] & 0x10 != 0 { Some(b[4]) } else { None }))
}

fn parse_image(
    bytes: &[u8],
    pos: usize,
    global: &[u8],
    canvas_w: u16,
    canvas_h: u16,
    gce: (Duration, u8, Option<u8>),
) -> Option<(Raw, usize)> {
    let h = bytes.get(pos + 1..pos + 10)?;
    let (left, top, width, height, packed) = (
        u16::from_le_bytes([h[0], h[1]]),
        u16::from_le_bytes([h[2], h[3]]),
        u16::from_le_bytes([h[4], h[5]]),
        u16::from_le_bytes([h[6], h[7]]),
        h[8],
    );
    // 子矩形必须整个落在画布内，否则后面的偏移计算全不可信
    if width == 0 || height == 0 {
        return None;
    }
    if left.checked_add(width)? > canvas_w || top.checked_add(height)? > canvas_h {
        return None;
    }
    let mut at = pos + 10;
    let palette = if packed & 0x80 != 0 {
        let n = table_bytes(packed);
        let t = bytes.get(at..at.checked_add(n)?)?.to_vec();
        at += n;
        t
    } else {
        global.to_vec()
    };
    if palette.len() < 3 {
        return None;
    }
    let min_code_size = *bytes.get(at)?;
    at += 1;
    let mut data = Vec::new();
    loop {
        let n = *bytes.get(at)?;
        if n == 0 {
            at += 1;
            break;
        }
        data.extend_from_slice(bytes.get(at + 1..at + 1 + usize::from(n))?);
        at += usize::from(n) + 1;
    }
    let expect = usize::from(width) * usize::from(height);
    let indices = lzw_decode(&data, min_code_size, expect)?;
    let raw = Raw {
        left,
        top,
        width,
        height,
        interlaced: packed & 0x40 != 0,
        indices,
        palette,
        transparent: gce.2,
        delay: gce.0,
        disposal: gce.1,
    };
    Some((raw, at))
}

/// LZW 解压。GIF 码流按 LSB 优先逐字节累积。
fn lzw_decode(data: &[u8], min_code_size: u8, expect: usize) -> Option<Vec<u8>> {
    if !(2..=8).contains(&min_code_size) {
        return None;
    }
    let clear = 1u16 << min_code_size;
    let end = clear + 1;
    let mut code_size = u32::from(min_code_size) + 1;
    let mut prefix = vec![NO_PREFIX; MAX_CODE];
    let mut suffix = vec![0u8; MAX_CODE];
    let mut dict_size = usize::from(end) + 1;
    let mut prev: Option<u16> = None;
    let mut out: Vec<u8> = Vec::with_capacity(expect);
    let mut bits: u32 = 0;
    let mut count: u32 = 0;
    let mut at = 0usize;

    // 字面量码 0..clear 直接是颜色索引：前缀为空、后缀就是自己
    fn init(prefix: &mut [u16], suffix: &mut [u8], clear: u16) {
        for p in prefix.iter_mut() {
            *p = NO_PREFIX;
        }
        for (i, s) in suffix.iter_mut().enumerate() {
            *s = i as u8;
        }
        for i in usize::from(clear)..MAX_CODE {
            prefix[i] = NO_PREFIX;
            suffix[i] = 0;
        }
    }
    init(&mut prefix, &mut suffix, clear);

    'outer: loop {
        while count < code_size {
            if at >= data.len() {
                break 'outer;
            }
            bits |= u32::from(data[at]) << count;
            at += 1;
            count += 8;
        }
        let code = (bits & ((1u32 << code_size) - 1)) as u16;
        bits >>= code_size;
        count -= code_size;

        if code == end {
            break;
        }
        if code == clear {
            init(&mut prefix, &mut suffix, clear);
            dict_size = usize::from(end) + 1;
            code_size = u32::from(min_code_size) + 1;
            prev = None;
            continue;
        }
        // K/KY：码值正好是下一个待分配槽位。必须先把这条目补进字典再展开，
        // 否则它尚无前驱，只能走出一个像素，整帧就废了（这个错误表现为
        // 「每个码只吐 2 字节」，而不是报错，很难看出）。
        let defined = usize::from(code) < dict_size;
        let first = if defined {
            chain_first(&prefix, &suffix, code)?
        } else if usize::from(code) == dict_size {
            chain_first(&prefix, &suffix, prev?)?
        } else {
            return None;
        };
        if !defined {
            add_entry(&mut prefix, &mut suffix, &mut dict_size, &mut code_size, prev?, first)?;
        }
        emit(&mut out, &prefix, &suffix, code)?;
        if defined && let Some(p) = prev {
            add_entry(&mut prefix, &mut suffix, &mut dict_size, &mut code_size, p, first)?;
        }
        prev = Some(code);
    }
    // 码流被截断但像素已凑够是真实文件里的常见形态；不够就是坏文件
    if out.len() < expect {
        return None;
    }
    out.truncate(expect);
    Some(out)
}

/// 新增一条字典记录，并按需把码宽加一位。
fn add_entry(
    prefix: &mut [u16],
    suffix: &mut [u8],
    dict_size: &mut usize,
    code_size: &mut u32,
    prev: u16,
    first: u8,
) -> Option<()> {
    if *dict_size >= MAX_CODE {
        return None;
    }
    prefix[*dict_size] = prev;
    suffix[*dict_size] = first;
    *dict_size += 1;
    // 解码器的字典与编码器同步增长：到 2^width 就加宽一位
    if *dict_size == (1usize << *code_size) && *code_size < 12 {
        *code_size += 1;
    }
    Some(())
}

fn emit(out: &mut Vec<u8>, prefix: &[u16], suffix: &[u8], code: u16) -> Option<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut c = code;
    loop {
        let idx = usize::from(c);
        if idx >= MAX_CODE || buf.len() > MAX_CODE {
            return None;
        }
        buf.push(suffix[idx]);
        let p = prefix[idx];
        if p == NO_PREFIX {
            break;
        }
        c = p;
    }
    out.extend(buf.iter().rev());
    Some(())
}

/// 一条码链的第一个字节（给新条目定尾用）。
fn chain_first(prefix: &[u16], suffix: &[u8], code: u16) -> Option<u8> {
    let mut c = code;
    loop {
        let idx = usize::from(c);
        if idx >= MAX_CODE {
            return None;
        }
        let p = prefix[idx];
        if p == NO_PREFIX {
            return Some(suffix[idx]);
        }
        c = p;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 素材由 ImageMagick 生成，期望值取自 magick 自己的解码结果（它是已知正确的解码器）
    const M2: &[u8] = include_bytes!("../tests/fixtures/m2.gif");
    const INTER: &[u8] = include_bytes!("../tests/fixtures/inter.gif");
    const TRANS: &[u8] = include_bytes!("../tests/fixtures/trans.gif");
    const BIG: &[u8] = include_bytes!("../tests/fixtures/big.gif");
    /// 单帧 4x2，索引 5 带透明标志位。magick 不肯写这个位，透明只能自己造字节测。
    const TINY: &[u8] = &[0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x04, 0x00, 0x02, 0x00, 0x83, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x1f, 0x35, 0x22, 0x3e, 0x6a, 0x33, 0x5d, 0x9f, 0x44, 0x7c, 0xd4, 0x55, 0x9b, 0x09, 0x66, 0xba, 0x3e, 0x77, 0xd9, 0x73, 0x88, 0xf8, 0xa8, 0x99, 0x17, 0xdd, 0xaa, 0x36, 0x12, 0xbb, 0x55, 0x47, 0xcc, 0x74, 0x7c, 0xdd, 0x93, 0xb1, 0xee, 0xb2, 0xe6, 0xff, 0xd1, 0x1b, 0x21, 0xf9, 0x04, 0x11, 0x08, 0x00, 0x05, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x00, 0x04, 0x07, 0x10, 0x14, 0x10, 0x04, 0x28, 0x23, 0x02, 0x00, 0x3b];
    /// 两帧，第二帧是 2x2 子矩形 + disposal=1：右半必须留着第一帧的颜色。
    const TINY_LEAVE: &[u8] = &[0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x04, 0x00, 0x02, 0x00, 0x83, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x1f, 0x35, 0x22, 0x3e, 0x6a, 0x33, 0x5d, 0x9f, 0x44, 0x7c, 0xd4, 0x55, 0x9b, 0x09, 0x66, 0xba, 0x3e, 0x77, 0xd9, 0x73, 0x88, 0xf8, 0xa8, 0x99, 0x17, 0xdd, 0xaa, 0x36, 0x12, 0xbb, 0x55, 0x47, 0xcc, 0x74, 0x7c, 0xdd, 0x93, 0xb1, 0xee, 0xb2, 0xe6, 0xff, 0xd1, 0x1b, 0x21, 0xf9, 0x04, 0x01, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x00, 0x04, 0x07, 0x30, 0x84, 0x10, 0x42, 0x08, 0x21, 0x02, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00, 0x04, 0x04, 0xf0, 0x9c, 0x73, 0x22, 0x00, 0x3b];
    /// 三帧，第二帧 disposal=2：它画过的左半该被擦回透明，第三帧时左半空、右半是索引 3。
    const TINY_ERASE: &[u8] = &[0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x04, 0x00, 0x02, 0x00, 0x83, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x1f, 0x35, 0x22, 0x3e, 0x6a, 0x33, 0x5d, 0x9f, 0x44, 0x7c, 0xd4, 0x55, 0x9b, 0x09, 0x66, 0xba, 0x3e, 0x77, 0xd9, 0x73, 0x88, 0xf8, 0xa8, 0x99, 0x17, 0xdd, 0xaa, 0x36, 0x12, 0xbb, 0x55, 0x47, 0xcc, 0x74, 0x7c, 0xdd, 0x93, 0xb1, 0xee, 0xb2, 0xe6, 0xff, 0xd1, 0x1b, 0x21, 0xf9, 0x04, 0x01, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x02, 0x00, 0x00, 0x04, 0x07, 0x30, 0x84, 0x10, 0x42, 0x08, 0x21, 0x02, 0x00, 0x21, 0xf9, 0x04, 0x02, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00, 0x04, 0x04, 0xf0, 0x9c, 0x73, 0x22, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x05, 0x00, 0x00, 0x00, 0x2c, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0x00, 0x00, 0x04, 0x04, 0x70, 0x8c, 0x31, 0x22, 0x00, 0x3b];
    fn px(a: &Animation, i: usize, x: u16, y: u16) -> [u8; 4] {
        let o = (usize::from(y) * usize::from(a.width) + usize::from(x)) * 4;
        [a.frames[i].rgba[o], a.frames[i].rgba[o + 1], a.frames[i].rgba[o + 2], a.frames[i].rgba[o + 3]]
    }

    #[test]
    fn decodes_two_solid_frames_with_delays() {
        let a = decode(M2).expect("m2.gif 该能解");
        assert_eq!((a.width, a.height), (32, 32));
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frames[0].delay, Duration::from_millis(70), "7cs = 70ms");
        assert_eq!(px(&a, 0, 0, 0), [0, 17, 34, 255]);
        assert_eq!(px(&a, 0, 31, 31), [0, 17, 34, 255]);
        assert_eq!(px(&a, 1, 0, 0), [238, 51, 17, 255]);
        assert_eq!(px(&a, 1, 31, 31), [238, 51, 17, 255]);
    }

    /// 交错存储的同一张图必须解出与非交错完全相同的像素——去交错错了这里必炸。
    #[test]
    fn interlaced_matches_sequential() {
        let plain = decode(M2).unwrap();
        let inter = decode(INTER).expect("交错 GIF 该能解");
        assert_eq!(inter.frames.len(), 2);
        for i in 0..2 {
            assert_eq!(inter.frames[i].rgba, plain.frames[i].rgba, "第 {i} 帧去交错结果不一致");
        }
    }

    #[test]
    fn transparent_index_is_skipped_not_painted() {
        let a = decode(TINY).expect("手搓的最小 GIF 该能解");
        assert_eq!((a.width, a.height), (4, 2));
        assert_eq!(a.frames.len(), 1);
        assert_eq!(a.frames[0].delay, Duration::from_millis(80), "8cs = 80ms");
        // 调色板第 i 项 = (17i, 31i, 53i) mod 256
        assert_eq!(px(&a, 0, 0, 0), [0, 0, 0, 255]);
        assert_eq!(px(&a, 0, 3, 0), [17, 31, 53, 255]);
        assert_eq!(px(&a, 0, 0, 1), [34, 62, 106, 255]);
        assert_eq!(px(&a, 0, 3, 1), [51, 93, 159, 255]);
        assert_eq!(px(&a, 0, 1, 0), [0, 0, 0, 0], "透明像素不该被画成实心");
        assert_eq!(px(&a, 0, 2, 1), [0, 0, 0, 0]);
    }

    /// 透明标志位没设时，GCE 里的索引字段必须被忽略（规范如此；magick 自己就不守）。
    #[test]
    fn transparency_flag_gates_the_index() {
        let mut off = TINY.to_vec();
        let k = off.windows(2).position(|w| w == [0x21, 0xf9]).unwrap();
        off[k + 3] &= !0x10;
        let a = decode(&off).expect("清掉标志位也该能解");
        assert_eq!(px(&a, 0, 1, 0), [85, 155, 9, 255], "标志位清了还跳过，就是把无关索引当透明用了");
    }

    /// disposal=1（do not dispose）：上一帧留在底下。
    #[test]
    fn disposal_leave_keeps_the_previous_frame() {
        let a = decode(TINY_LEAVE).unwrap();
        assert_eq!(a.frames.len(), 2);
        assert_eq!(px(&a, 0, 0, 0), [17, 31, 53, 255], "第一帧铺满索引 1");
        assert_eq!(px(&a, 1, 0, 0), [119, 217, 115, 255], "第二帧的子矩形");
        assert_eq!(px(&a, 1, 3, 0), [17, 31, 53, 255], "右半该保留第一帧");
    }

    /// disposal=2（restore to background）：本帧画过的区域在下一帧之前该被擦回透明。
    #[test]
    fn disposal_erase_restores_the_background() {
        let a = decode(TINY_ERASE).unwrap();
        assert_eq!(a.frames.len(), 3);
        assert_eq!(px(&a, 1, 0, 0), [119, 217, 115, 255], "第二帧自己该看得见");
        assert_eq!(px(&a, 2, 0, 0), [0, 0, 0, 0], "第三帧时左半该已被擦掉");
        assert_eq!(px(&a, 2, 3, 0), [51, 93, 159, 255], "第三帧的右半");
    }

    #[test]
    fn honors_non_square_canvas() {
        let a = decode(BIG).unwrap();
        assert_eq!((a.width, a.height), (48, 40));
        assert_eq!(px(&a, 0, 47, 39), [18, 52, 86, 255]);
        assert_eq!(px(&a, 1, 47, 39), [101, 67, 33, 255]);
    }

    #[test]
    fn frame_schedule_loops() {
        let a = decode(M2).unwrap();
        assert_eq!(a.frame_at(Duration::from_millis(0)), 0);
        assert_eq!(a.frame_at(Duration::from_millis(69)), 0);
        assert_eq!(a.frame_at(Duration::from_millis(70)), 1);
        // 一圈 140ms：139ms 仍在第二帧，140ms 回到第一帧
        assert_eq!(a.frame_at(Duration::from_millis(139)), 1);
        assert_eq!(a.frame_at(Duration::from_millis(140)), 0);
        assert_eq!(a.frame_at(Duration::from_secs(3600)), 0);
    }

    #[test]
    fn truncated_input_never_panics() {
        for src in [M2, INTER, TRANS, BIG, TINY, TINY_LEAVE, TINY_ERASE] {
            for n in 0..src.len() {
                // 只要求不 panic、不返回越界的帧；能不能解出来不作保证
                if let Some(a) = decode(&src[..n]) {
                    assert_eq!(a.frames[0].rgba.len(), usize::from(a.width) * usize::from(a.height) * 4);
                }
            }
        }
    }

    #[test]
    fn rejects_garbage_and_oversized_canvases() {
        assert!(decode(b"not a gif at all").is_none());
        assert!(decode(&[]).is_none());
        assert!(decode(b"GIF89a").is_none());
        // 65535x65535 的画布：像素数远超上限，必须在分配前就拒掉
        let mut hdr = *b"GIF89a\xFF\xFF\xFF\xFF\x00\x00\x00";
        assert!(decode(&hdr).is_none());
        hdr[11] = 0;
        assert!(decode(&hdr).is_none());
    }

    #[test]
    fn color_table_is_2_pow_n_plus_one_entries() {
        // 锁住那个「多算一倍」的坑：N=0 是 2 个条目 = 6 字节，不是 12
        assert_eq!(table_bytes(0), 6);
        assert_eq!(table_bytes(7), 768);
    }
}
