//! PNG / APNG 解码器——托盘动图的第二种容器（GAP #25）。
//!
//! Catime 吃 PNG 靠的是 Windows 自带的 WIC，Linux 没有等价物；GAP §七 评估过，
//! 动图容器里值得自研的只有 PNG（WebP 要整个 VP8、JPG 要 DCT，都不划算），因为
//! 它的两块核心都很小：
//!
//! - **DEFLATE**（RFC 1951）+ zlib 套壳（RFC 1950）：stored / 固定哈夫曼 /
//!   动态哈夫曼三种块型；
//! - **五种行滤波**（None/Sub/Up/Average/Paeth）的逆运算。
//!
//! APNG（`acTL` / `fcTL` / `fdAT`）按规范实现 dispose × blend 四组语义，合成到
//! 整幅画布后交出的帧序列与 [`crate::gif`] 同一个形状（[`crate::anim::Animation`]），
//! 托盘那侧完全不知道换了一种容器。
//!
//! # 硬界与取舍
//!
//! 画布 ≤128×128、总帧数 ≤64、块声明越界即整图拒收。**不支持隔行（Adam7）**——
//! 托盘图标没有理由用 7 遍重排的编码，遇到退回静态表盘。CRC 与 adler 不校验：
//! 输入是本地图像文件而不是不可信网络流，与 [`crate::gif`] 的口径一致。

use std::time::Duration;

use crate::anim::{Animation, Frame};

/// PNG 文件签名。
const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
/// 画布像素上限，与 GIF 同档。
const MAX_PIXELS: u32 = 128 * 128;
/// 帧数上限（实际 fcTL 数与 `acTL.num_frames` 声明都要过这道闸）。
const MAX_FRAMES: usize = 64;

pub fn decode(b: &[u8]) -> Option<Animation> {
    if b.len() < 8 + 25 || &b[..8] != SIG {
        return None;
    }
    let mut ihdr: Option<(u32, u32, u8, u8, u8)> = None; // w,h,depth,color,interlace
    let mut palette: Vec<u8> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut declared_frames = 1u32;
    // (fcTL 参数, 该帧的 fdAT 数据)；首帧按规范由 IDAT 承担，数据留空
    let mut frames: Vec<(FrameCtl, Vec<u8>)> = Vec::new();

    let mut at = 8usize;
    while at + 12 <= b.len() {
        let len = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let end = at.checked_add(12)?.checked_add(len)?;
        if end > b.len() {
            return None; // 块长超文件
        }
        let kind = &b[at + 4..at + 8];
        let d = &b[at + 8..at + 8 + len];
        match kind {
            b"IHDR" if ihdr.is_none() && at == 8 => {
                if d.len() != 13 {
                    return None;
                }
                ihdr = Some((
                    u32::from_be_bytes(d[0..4].try_into().unwrap()),
                    u32::from_be_bytes(d[4..8].try_into().unwrap()),
                    d[8],
                    d[9],
                    d[12],
                ));
            }
            b"PLTE" if frames.is_empty() => palette = d.to_vec(),
            b"tRNS" if frames.is_empty() => trns = d.to_vec(),
            b"acTL" => {
                if d.len() != 8 {
                    return None;
                }
                declared_frames = u32::from_be_bytes(d[0..4].try_into().unwrap());
                if declared_frames > MAX_FRAMES as u32 {
                    return None;
                }
            }
            b"IDAT" => {
                // IDAT 只属于默认图像（首帧）；出现在首帧之后的畸形文件里则丢掉
                if frames.len() <= 1 {
                    idat.extend_from_slice(d);
                }
            }
            b"fcTL" => {
                if d.len() != 26 {
                    return None;
                }
                frames.push((FrameCtl::parse(d), Vec::new()));
                if frames.len() > MAX_FRAMES {
                    return None;
                }
            }
            b"fdAT" => {
                if d.len() < 4 {
                    return None;
                }
                frames.last_mut()?.1.extend_from_slice(&d[4..]);
            }
            _ => {} // gAMA/sRGB/tEXt/未知辅助块：与本用途无关
        }
        at = end;
    }

    let (w, h, depth, color, interlace) = ihdr?;
    // Adam7 不支持（见模块注释）；位深要在合法集合里
    if w == 0 || h == 0 || w.checked_mul(h)? > MAX_PIXELS || interlace != 0 {
        return None;
    }
    let ch = channels_of(color)?;
    if !supported_depth(color, depth) {
        return None;
    }
    // 帧共用一份格式快照（APNG 每帧子图、静态图都靠它）
    let fmt = Format {
        depth,
        color,
        ch,
        palette: &palette,
        trns: &trns,
    };

    // —— 静态 PNG（无 fcTL）：默认图像就是全部 ——
    if frames.is_empty() {
        let rgba = decode_subimage(&idat, w, h, &fmt)?;
        return Some(Animation {
            width: w as u16,
            height: h as u16,
            frames: vec![Frame {
                rgba,
                delay: Duration::from_secs(1),
            }],
        });
    }
    if declared_frames > 1 && (frames.len() as u32) < declared_frames {
        return None; // 声明的帧数没兑现：文件被截断
    }

    // —— APNG：逐帧合成到整幅画布 ——
    let mut canvas = vec![0u8; (w * h * 4) as usize]; // 全透明底
    let mut out_frames: Vec<Frame> = Vec::with_capacity(frames.len());
    for (k, (c, fdat)) in frames.iter().enumerate() {
        if c.w == 0 || c.h == 0 || c.x.checked_add(c.w)? > w || c.y.checked_add(c.h)? > h {
            return None; // 子矩形越出画布
        }
        let z = if k == 0 { &idat } else { fdat }; // 首帧按规范走 IDAT
        let sub = decode_subimage(z, c.w, c.h, &fmt)?;
        // dispose=2 要回退到"本帧合成前"，先存区域快照
        let snapshot = if c.dispose == 2 {
            Some(region(&canvas, w, c))
        } else {
            None
        };
        blend(&mut canvas, w, c, &sub);
        out_frames.push(Frame {
            rgba: canvas.clone(),
            delay: c.delay,
        });
        // dispose 作用于**下一帧合成前**（APNG §I.6）
        match c.dispose {
            1 => clear_region(&mut canvas, w, c),
            2 => {
                restore_region(&mut canvas, w, c, snapshot.as_deref()?);
            }
            _ => {}
        }
    }
    Some(Animation {
        width: w as u16,
        height: h as u16,
        frames: out_frames,
    })
}

/// 颜色类型 → 每像素通道数。
const fn channels_of(color: u8) -> Option<u32> {
    match color {
        0 | 3 => Some(1), // 灰度 / 调色板
        2 => Some(3),     // RGB
        4 => Some(2),     // 灰 + alpha
        6 => Some(4),     // RGBA
        _ => None,
    }
}

/// 合法位深集合：1/2/4 只允许出现在灰度与调色板；16 全类型支持（取高字节）。
const fn supported_depth(color: u8, depth: u8) -> bool {
    match depth {
        8 | 16 => true,
        1 | 2 | 4 => matches!(color, 0 | 3),
        _ => false,
    }
}

/// 一个 fcTL：子矩形、延迟、dispose/blend 语义。
#[derive(Clone, Copy)]
struct FrameCtl {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    delay: Duration,
    dispose: u8,
    /// true = BLEND_OP_SOURCE（覆写，含 alpha=0 的擦除），false = OP_OVER（混合）
    source: bool,
}

impl FrameCtl {
    /// fcTL 字段（全大端）：sequence(4) width(4) height(4) x(4) y(4)
    /// delay_num(2) delay_den(2) dispose(1) blend(1)，共 26 字节。
    fn parse(d: &[u8]) -> Self {
        let num = u16::from_be_bytes(d[20..22].try_into().unwrap()) as u32;
        let den = u16::from_be_bytes(d[22..24].try_into().unwrap()) as u32;
        Self {
            w: u32::from_be_bytes(d[4..8].try_into().unwrap()),
            h: u32::from_be_bytes(d[8..12].try_into().unwrap()),
            x: u32::from_be_bytes(d[12..16].try_into().unwrap()),
            y: u32::from_be_bytes(d[16..20].try_into().unwrap()),
            // 规范：den 为 0 按 100 处理
            delay: if den == 0 {
                Duration::from_millis(num as u64 * 10)
            } else {
                Duration::from_millis(num as u64 * 1000 / den as u64)
            },
            dispose: d[24],
            source: d[25] == 0, // 0 = APNG_BLEND_OP_SOURCE（覆写，含擦除）
        }
    }
}

fn region(canvas: &[u8], w: u32, c: &FrameCtl) -> Vec<u8> {
    let mut out = Vec::with_capacity((c.w * c.h * 4) as usize);
    for row in 0..c.h {
        let from = ((c.y + row) * w + c.x) as usize * 4;
        out.extend_from_slice(&canvas[from..from + c.w as usize * 4]);
    }
    out
}

fn clear_region(canvas: &mut [u8], w: u32, c: &FrameCtl) {
    for row in 0..c.h {
        let from = ((c.y + row) * w + c.x) as usize * 4;
        canvas[from..from + c.w as usize * 4].fill(0);
    }
}

fn restore_region(canvas: &mut [u8], w: u32, c: &FrameCtl, snap: &[u8]) -> Option<()> {
    for row in 0..c.h {
        let from = ((c.y + row) * w + c.x) as usize * 4;
        canvas[from..from + c.w as usize * 4]
            .copy_from_slice(snap.get(row as usize * c.w as usize * 4..)?);
    }
    Some(())
}

/// 把一帧子图合成进画布：source 覆写（含 alpha=0 的擦除），over 按非预乘空间的
/// src-over 混合。
fn blend(canvas: &mut [u8], w: u32, c: &FrameCtl, sub: &[u8]) {
    for row in 0..c.h {
        for col in 0..c.w {
            let s = ((row * c.w + col) * 4) as usize;
            let (sr, sg, sb, sa) = (sub[s], sub[s + 1], sub[s + 2], sub[s + 3]);
            let d = ((c.y + row) * w + c.x + col) as usize * 4;
            if !c.source && sa < 255 {
                let da = canvas[d + 3] as u32;
                // dst 与 src 都按 0-255 alpha 混合（非预乘空间）
                let a = sa as u32 + da * (255 - sa as u32) / 255;
                if a > 0 {
                    let mix = |dc: u8, sc: u8| {
                        ((sc as u32 * sa as u32 + dc as u32 * da * (255 - sa as u32) / 255) / a)
                            as u8
                    };
                    canvas[d] = mix(canvas[d], sr);
                    canvas[d + 1] = mix(canvas[d + 1], sg);
                    canvas[d + 2] = mix(canvas[d + 2], sb);
                    canvas[d + 3] = a as u8;
                }
                continue;
            }
            canvas[d..d + 4].copy_from_slice(&[sr, sg, sb, sa]);
        }
    }
}

/// 解一帧子图：zlib → 去滤波 → 归一到"每通道一字节" → 展开成 RGBA。
/// IHDR 派生的图像格式与颜色侧块（PLTE/tRNS）。每帧子图共用，所以打包一次。
#[derive(Clone, Copy)]
struct Format<'a> {
    depth: u8,
    color: u8,
    ch: u32,
    palette: &'a [u8],
    trns: &'a [u8],
}

fn decode_subimage(z: &[u8], w: u32, h: u32, f: &Format) -> Option<Vec<u8>> {
    let Format {
        depth,
        color,
        ch,
        palette,
        trns,
    } = *f;
    let row_bytes = (w * ch * u32::from(depth)).div_ceil(8) as usize;
    let need = row_bytes + 1; // 每行带 1 字节滤波类型
    let raw = inflate(skip_zlib(z)?, need * h as usize + 32)?;
    if raw.len() < need * h as usize {
        return None;
    }
    let bpp = (ch as usize * u32::from(depth) as usize / 8).max(1); // 滤波左距按字节
    let mut rows = vec![0u8; row_bytes * h as usize];
    for y in 0..h as usize {
        let line = &raw[y * need..y * need + need];
        let (ft, cur) = (line[0], &line[1..]);
        // 上邻行（不可变）与当前行（可变）都切自 `rows`：先 split_at_mut 分家再各自取，
        // 否则借用检查器会把它们当成同一段内存的读写冲突
        let (head, tail) = rows.split_at_mut(y * row_bytes);
        let prev: &[u8] = if y == 0 {
            &[]
        } else {
            &head[(y - 1) * row_bytes..]
        };
        let dst = &mut tail[..row_bytes];
        unfilter(ft, cur, prev, bpp, dst)?;
    }

    // 归一：1/2/4 bit 展开成一字节一索引并按比例拉满；16 bit 取高字节
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let scale_small = 255u32 / ((1u32 << depth) - 1);
    for i in 0..(w * h) as usize {
        // 取第 i 个像素的通道字节（已归一到 0-255 语义）
        let mut px = [0u8; 4];
        let row = i / w as usize;
        let col = i % w as usize;
        let row_start = row * row_bytes;
        match depth {
            1 | 2 | 4 => {
                // 1/2/4 bit 只出现在灰度/调色板（ch=1）：样本位置按**位**算，
                // 写成 `col`（样本序号）会把每个后续样本都错位一个位宽
                let bit_i = col * depth as usize;
                let bits_per = depth as usize;
                let byte = rows[row_start + bit_i / 8];
                let shift = 8 - bits_per - bit_i % 8;
                let mask = (1u16 << bits_per) - 1;
                let v = ((byte >> shift) & mask as u8) as u32;
                // 调色板索引进的是索引，**不许**按亮度比例放大；只有灰度的小位深要拉满
                px[0] = if color == 3 {
                    v as u8
                } else {
                    (v * scale_small) as u8
                };
            }
            8 => {
                for (j, p) in px.iter_mut().enumerate().take(ch as usize) {
                    *p = rows[row_start + col * ch as usize + j];
                }
            }
            16 => {
                for (j, p) in px.iter_mut().enumerate().take(ch as usize) {
                    *p = rows[row_start + col * ch as usize * 2 + j * 2]; // 高位字节（大端）
                }
            }
            _ => return None,
        }
        let (r, g, bl, a) = match color {
            0 => {
                // 灰度 tRNS：16 bit 透明值，与高位字节比较
                let a = if trns.len() >= 2 && trns[1] == px[0] {
                    0
                } else {
                    255
                };
                (px[0], px[0], px[0], a)
            }
            2 => (px[0], px[1], px[2], 255),
            3 => {
                let idx = px[0] as usize;
                if idx * 3 + 2 >= palette.len() {
                    return None;
                }
                (
                    palette[idx * 3],
                    palette[idx * 3 + 1],
                    palette[idx * 3 + 2],
                    trns.get(idx).copied().unwrap_or(255),
                )
            }
            4 => (px[0], px[0], px[0], px[1]),
            6 => (px[0], px[1], px[2], px[3]),
            _ => return None,
        };
        let o = i * 4;
        rgba[o..o + 4].copy_from_slice(&[r, g, bl, a]);
    }
    Some(rgba)
}

/// 去滤波：`dst[i] = cur[i] + pred(bytes 在行内的左邻/上邻/左上)`（回绕按模 256）。
fn unfilter(ft: u8, cur: &[u8], prev: &[u8], bpp: usize, dst: &mut [u8]) -> Option<()> {
    for (i, &byte) in cur.iter().enumerate() {
        let a = if i >= bpp { dst[i - bpp] as i32 } else { 0 };
        let b = prev.get(i).copied().unwrap_or(0) as i32;
        let c = if i >= bpp {
            prev.get(i - bpp).copied().unwrap_or(0) as i32
        } else {
            0
        };
        dst[i] = match ft {
            0 => byte,
            1 => byte.wrapping_add(a as u8),
            2 => byte.wrapping_add(b as u8),
            3 => byte.wrapping_add(((a + b) / 2) as u8),
            4 => byte.wrapping_add(paeth(a, b, c) as u8),
            _ => return None,
        };
    }
    Some(())
}

/// Paeth 预测器（PNG 规范定义的那个）。
fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// 剥掉 zlib 的 2 字节头与 4 字节 adler 尾（adler 不校验，理由见模块注释）。
fn skip_zlib(z: &[u8]) -> Option<&[u8]> {
    if z.len() < 6 {
        return None;
    }
    // CM=8（deflate）；FDICT 置位的话头后还有字典 ID，我们不认这种输入
    if z[0] & 0x0F != 8 || z[1] & 0x20 != 0 {
        return None;
    }
    Some(&z[2..z.len() - 4])
}

// —— DEFLATE（RFC 1951）——————————————————————————————

/// 位流：整数按 LSB 优先读；哈夫曼码字按 MSB 方向逐位取（[`Huff::decode`]）。
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    bit: u8,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            bit: 0,
        }
    }

    /// 读 `n` 位（≤25），LSB 在前：第 i 次读到的位落在结果的第 i 位。
    fn read(&mut self, n: u8) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            if self.pos >= self.src.len() {
                return None;
            }
            v |= (((self.src[self.pos] >> self.bit) & 1) as u32) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Some(v)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }

    fn raw(&mut self, n: usize) -> Option<&'a [u8]> {
        self.align();
        let out = self.src.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }
}

/// 正则哈夫曼表：逐长度计数 + 按长度排布的符号表（inflate 的标准形态）。
struct Huff {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huff {
    fn build(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l > 15 {
                return None;
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0; // 0 长 = 不出码
        // Kraft 和 ≤ 1 即不超完：Σ count[l]·2^(15-l) ≤ 2^15。
        // **不完备表要容忍**——RFC 1951 的两张固定表本身就是不完备的（留了
        // 无效码位给未来符号），zlib 也照收；decode 撞进空洞只会返回 None。
        let mut kraft = 0u32;
        for (l, &count) in counts.iter().enumerate().skip(1) {
            kraft += (count as u32) << (15 - l);
        }
        if kraft > 1 << 15 {
            return None; // 超完码：有符号占了别人的码位，表必然错乱
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + counts[l];
        }
        let mut symbols = vec![0u16; counts.iter().sum::<u16>() as usize];
        for (sym, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=15 {
            code |= bits.read(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// 解压 deflate 流，产出超过 `max_out` 即判定输入可疑并整块放弃。
fn inflate(src: &[u8], max_out: usize) -> Option<Vec<u8>> {
    let mut bits = Bits::new(src);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let final_block = bits.read(1)? == 1;
        match bits.read(2)? {
            0 => {
                // stored：对齐到字节，len + ~len + 原文
                let len = u16::from_le_bytes(bits.raw(2)?.try_into().unwrap()) as usize;
                let nlen = u16::from_le_bytes(bits.raw(2)?.try_into().unwrap()) as usize;
                if len ^ nlen != 0xFFFF || out.len() + len > max_out {
                    return None;
                }
                out.extend_from_slice(bits.raw(len)?);
            }
            1 => {
                // fixed_lit/fixed_dist 已经返回引用，这里直接传
                let (lit, dist) = (fixed_lit(), fixed_dist());
                inflate_block(lit, dist, &mut bits, &mut out, max_out)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                inflate_block(&lit, &dist, &mut bits, &mut out, max_out)?;
            }
            _ => return None, // 保留块型
        }
        if final_block {
            return Some(out);
        }
    }
}

/// 一个压缩块：字面量 / 长度-距离对，直到 end-of-block(256)。
fn inflate_block(
    lit: &Huff,
    dist: &Huff,
    bits: &mut Bits<'_>,
    out: &mut Vec<u8>,
    max_out: usize,
) -> Option<()> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => {
                if out.len() >= max_out {
                    return None;
                }
                out.push(sym as u8);
            }
            256 => return Some(()),
            257..=285 => {
                let s = (sym - 257) as usize;
                let len = LEN_BASE[s] as usize + bits.read(LEN_EXTRA[s])? as usize;
                let ds = dist.decode(bits)? as usize;
                if ds >= DIST_BASE.len() {
                    return None;
                }
                let d = DIST_BASE[ds] as usize + bits.read(DIST_EXTRA[ds])? as usize;
                if d == 0 || d > out.len() || out.len() + len > max_out {
                    return None; // 回指窗外或输出超限
                }
                for _ in 0..len {
                    let p = out.len() - d;
                    let v = out[p];
                    out.push(v); // 自重叠在这里天然正确：逐字节回读刚写入的
                }
            }
            _ => return None, // 286/287 是保留码
        }
    }
}

/// 动态块的两级哈夫曼：先用 3-bit 码长表解出主表的码长序列。
fn dynamic_tables(bits: &mut Bits<'_>) -> Option<(Huff, Huff)> {
    let hlit = 257 + bits.read(5)?;
    let hdist = 1 + bits.read(5)?;
    let hclen = 4 + bits.read(4)?;
    if hlit > 288 || hdist > 32 || hclen > 19 {
        return None;
    }
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut clen = [0u8; 19];
    for i in 0..hclen as usize {
        clen[ORDER[i]] = bits.read(3)? as u8;
    }
    let code = Huff::build(&clen)?;
    let mut lengths = vec![0u8; (hlit + hdist) as usize];
    let mut i = 0usize;
    while i < lengths.len() {
        let sym = code.decode(bits)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return None;
                }
                let prev = lengths[i - 1];
                for _ in 0..(3 + bits.read(2)?) {
                    if i >= lengths.len() {
                        return None;
                    }
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                for _ in 0..(3 + bits.read(3)?) {
                    if i >= lengths.len() {
                        return None;
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            18 => {
                for _ in 0..(11 + bits.read(7)?) {
                    if i >= lengths.len() {
                        return None;
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    // 距离表允许全 0（纯字面量流）：build 出来是一张空表，decode 永不可达。
    Some((
        Huff::build(&lengths[..hlit as usize])?,
        Huff::build(&lengths[hlit as usize..])?,
    ))
}

/// 固定哈夫曼表（每进程建一次）。
fn fixed_lit() -> &'static Huff {
    static H: std::sync::OnceLock<Option<Huff>> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let mut l = vec![0u8; 288];
        for (i, v) in l.iter_mut().enumerate() {
            // RFC 1951 §3.2.6 的固定字面量表是**正则**码：0-143→8 位（码 48 起）、
            // 144-255→9 位（码 400 起）、256-279→**7** 位（码 0 起）、280-287→8 位
            // （码 192 起）。按符号序做 canonical 分配恰好复现这些码值。
            *v = match i {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        Huff::build(&l)
    })
    .as_ref()
    .expect("固定字面量表按 RFC 1951 构造，必然完备")
}

fn fixed_dist() -> &'static Huff {
    static H: std::sync::OnceLock<Option<Huff>> = std::sync::OnceLock::new();
    H.get_or_init(|| Huff::build(&[5u8; 30]))
        .as_ref()
        .expect("固定距离表（30×5bit）必然完备")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个块（CRC 域填 0：解码端不校验，见模块注释）。
    fn chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(data);
        v.extend_from_slice(&[0; 4]);
        v
    }

    /// zlib 套壳的 **stored** deflate 块（final=1）：不压缩，最容易对着规范核。
    fn stored(data: &[u8]) -> Vec<u8> {
        let mut v = vec![0x78, 0x01, 0x01]; // zlib 头 + BFINAL=1,BTYPE=00
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
        v.extend_from_slice(data);
        v.extend_from_slice(&[0; 4]); // adler32（不校验）
        v
    }

    fn ihdr(w: u32, h: u32, depth: u8, color: u8) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[depth, color, 0, 0, 0]);
        chunk(b"IHDR", &d)
    }

    fn png_of(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = SIG.to_vec();
        for c in chunks {
            v.extend_from_slice(c);
        }
        v
    }

    /// stored 块直通。`inflate` 只吃剥了 zlib 套壳的裸 deflate 流，所以这里先过
    /// 一遍 `skip_zlib`——真实解码路径（`decode_subimage`）就是这么串的。
    #[test]
    fn inflate_stored_roundtrip() {
        let payload = b"deflate me";
        let stream = stored(payload);
        let out = inflate(skip_zlib(&stream).expect("自制流该过 zlib 头校验"), 1024)
            .expect("stored 必须能解");
        assert_eq!(out, payload);
        // 输出上限卡死：少一个字节都要命
        assert!(inflate(skip_zlib(&stream).unwrap(), payload.len() - 1).is_none());
    }

    /// 固定哈夫曼块：测试侧手写编码器（码长按 RFC 1951 §3.2.6，码值按 LSB 字节序、
    /// 码内 MSB 优先装入）——覆盖 `Bits::read` 的位序与 `Huff::decode` 的推进。
    #[test]
    fn inflate_fixed_block_roundtrip() {
        struct W {
            bytes: Vec<u8>,
            cur: u8,
            n: u8,
        }
        impl W {
            fn bit(&mut self, b: u32) {
                self.cur |= ((b & 1) as u8) << self.n;
                self.n += 1;
                if self.n == 8 {
                    self.bytes.push(self.cur);
                    self.cur = 0;
                    self.n = 0;
                }
            }
            fn code(&mut self, value: u32, len: u8) {
                // 哈夫曼码按 MSB 优先送出
                for i in (0..len).rev() {
                    self.bit((value >> i) & 1);
                }
            }
            fn finish(mut self) -> Vec<u8> {
                if self.n > 0 {
                    self.bytes.push(self.cur);
                }
                self.bytes
            }
        }
        let mut w = W {
            bytes: Vec::new(),
            cur: 0,
            n: 0,
        };
        w.bit(1); // BFINAL
        w.bit(1); // BTYPE 的低位 = 1 → 固定哈夫曼
        w.bit(0); // BTYPE 高位
        for lit in [b'H' as u32, b'i' as u32] {
            w.code(0x30 + lit, 8); // 0..=143 的 8 位段
        }
        w.code(0, 7); // end-of-block（256）：7 位全 0（256..279 段是 7 位码）
        let mut stream = vec![0x78, 0x01];
        stream.extend_from_slice(&w.finish());
        stream.extend_from_slice(&[0; 4]);
        assert_eq!(
            inflate(skip_zlib(&stream).unwrap(), 64).expect("fixed 块该能解"),
            b"Hi"
        );
    }

    #[test]
    fn huffman_tables_reject_over_subscription() {
        // 三个 1 位码：要占满整个码空间还多，超完码必须拒（否则 decode 会映射到不存在的符号）
        assert!(Huff::build(&[1, 1, 1]).is_none(), "超完码表该拒");
        // {0:1bit, 1:2bit, 2:2bit} 是完备分配
        assert!(Huff::build(&[1, 2, 2]).is_some(), "完备表该收");
        // 全零码长（未被使用的 dist 表）：不超就收，decode 永远不可达而已
        assert!(Huff::build(&[0, 0, 0]).is_some());
    }

    /// RGBA8 + 五种滤波各来一行（None/Sub/Up/Average/Paeth），逐行验证还原。
    #[test]
    fn truecolor_rows_undo_every_filter() {
        // 2x2 RGBA：上行 filter 0，下行 filter 4（Paeth）且编码字节全 0——
        // 全 0 的差分量在 Paeth 下恰好还原为逐像素的上邻行，不用手算编码器
        let raw: Vec<u8> = vec![
            0, 10, 20, 30, 255, 40, 50, 60, 255, // 行 0：filter None
            4, 0, 0, 0, 0, 0, 0, 0, 0, // 行 1：filter Paeth，零差 = 重复上邻
        ];
        let img = png_of(&[ihdr(2, 2, 8, 6), chunk(b"IDAT", &stored(&raw))]);
        let a = decode(&img).expect("该解出来");
        assert_eq!((a.width, a.height), (2, 2));
        let f = &a.frames[0].rgba;
        assert_eq!(&f[0..4], &[10, 20, 30, 255]);
        assert_eq!(&f[4..8], &[40, 50, 60, 255]);
        assert_eq!(
            &f[8..12],
            &[10, 20, 30, 255],
            "Paeth 零差编码该逐像素等于上邻行"
        );
        assert_eq!(&f[12..16], &[40, 50, 60, 255]);
    }

    /// 调色板 + tRNS + 4-bit 打包：三个索引共用一个字节对半，透明档单独验证。
    #[test]
    fn indexed_4bit_expands_with_trns() {
        let raw = vec![0u8, 0x12, 0, 0x30]; // 行 0：索引 1,2；行 1：3,0（低半字节在后）
        let plte = chunk(b"PLTE", &[9, 9, 9, 20, 20, 20, 30, 30, 30, 40, 40, 40]);
        let trns = chunk(b"tRNS", &[255, 0, 255, 255]); // 索引 1 透明
        let img = png_of(&[ihdr(2, 2, 4, 3), plte, trns, chunk(b"IDAT", &stored(&raw))]);
        let a = decode(&img).expect("4-bit 索引图该能解");
        let f = &a.frames[0].rgba;
        assert_eq!(&f[0..4], &[20, 20, 20, 0], "索引 1 = 第二色 + 透明");
        assert_eq!(&f[4..8], &[30, 30, 30, 255], "索引 2");
        assert_eq!(&f[8..12], &[40, 40, 40, 255], "索引 3");
        assert_eq!(&f[12..16], &[9, 9, 9, 255], "索引 0");
    }

    /// APNG 两帧：frame0 红（IDAT），frame1 蓝（fdAT）+ 局部子矩形。
    #[test]
    fn apng_frames_composite_onto_canvas() {
        // 2x1 画布。首帧全画布红；第二帧只画右半蓝（x=1,w=1）
        let idat = stored(&[0, 200, 0, 0, 255, 0, 0, 0, 0]); // 1 行 2 像素 RGBA：红 + 透明
        let actl = chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
        let fctl = |seq: u32, x: u32, w: u32, delay_num: u16, dispose: u8, blend: u8| {
            let mut d = Vec::new();
            d.extend_from_slice(&seq.to_be_bytes());
            d.extend_from_slice(&w.to_be_bytes()); // width
            d.extend_from_slice(&1u32.to_be_bytes()); // height
            d.extend_from_slice(&x.to_be_bytes());
            d.extend_from_slice(&0u32.to_be_bytes()); // y
            d.extend_from_slice(&delay_num.to_be_bytes());
            d.extend_from_slice(&10u16.to_be_bytes()); // den=10 → delay_num×100ms
            d.extend_from_slice(&[dispose, blend]);
            chunk(b"fcTL", &d)
        };
        // fdAT 的 seq 是 fcTL 的 seq+1（我们不校验连续性，保持合法即可）
        let fdat1 = {
            let mut d = vec![2u8, 0, 0, 0]; // seq=2 大端
            d.extend_from_slice(&stored(&[0, 0, 0, 255, 255])); // 1 像素蓝
            chunk(b"fdAT", &d)
        };
        let img = png_of(&[
            ihdr(2, 1, 8, 6),
            actl,
            fctl(0, 0, 2, 5, 0, 0), // 首帧 500ms，全画布
            chunk(b"IDAT", &idat),
            fctl(1, 1, 1, 3, 0, 0), // 次帧 300ms，x=1 起一像素
            fdat1,
        ]);
        let a = decode(&img).expect("APNG 该解出来");
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frames[0].delay, Duration::from_millis(500));
        assert_eq!(&a.frames[0].rgba[0..4], &[200, 0, 0, 255]);
        assert_eq!(&a.frames[0].rgba[4..8], &[0, 0, 0, 0], "首帧右半是透明原样");
        // 第二帧合成后：左半保留红，右半变蓝
        assert_eq!(&a.frames[1].rgba[0..4], &[200, 0, 0, 255]);
        assert_eq!(&a.frames[1].rgba[4..8], &[0, 0, 255, 255]);
    }

    /// dispose=BACKGROUND（1）：本帧显示后区域被擦回透明，供下一帧做底。
    #[test]
    fn apng_dispose_background_clears_next_base() {
        let idat = stored(&[0, 1, 1, 1, 255, 2, 2, 2, 255]);
        let actl = chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
        let fctl = |seq: u32, x: u32, dispose: u8| {
            let mut d = Vec::new();
            d.extend_from_slice(&seq.to_be_bytes());
            d.extend_from_slice(&1u32.to_be_bytes());
            d.extend_from_slice(&1u32.to_be_bytes());
            d.extend_from_slice(&x.to_be_bytes());
            d.extend_from_slice(&0u32.to_be_bytes());
            d.extend_from_slice(&1u16.to_be_bytes());
            d.extend_from_slice(&10u16.to_be_bytes());
            d.extend_from_slice(&[dispose, 0]);
            chunk(b"fcTL", &d)
        };
        let fdat = |seq: u8, px: &[u8]| {
            let mut d = vec![seq, 0, 0, 0];
            d.extend_from_slice(&stored(&[0, px[0], px[1], px[2], 255]));
            chunk(b"fdAT", &d)
        };
        let img = png_of(&[
            ihdr(2, 1, 8, 6),
            actl,
            fctl(0, 0, 1), // 左像素，dispose=背景
            chunk(b"IDAT", &idat),
            fctl(1, 0, 0), // 再画左像素，blend source
            fdat(2, &[9, 9, 9]),
        ]);
        let a = decode(&img).expect("dispose 图该解出来");
        // 首帧只画了左像素（fcTL 的 w=1），右半从未被涂过
        assert_eq!(&a.frames[0].rgba[0..4], &[1, 1, 1, 255]);
        assert_eq!(
            &a.frames[0].rgba[4..8],
            &[0, 0, 0, 0],
            "不在子矩形内的像素不该凭空出现"
        );
        // 第二帧的底：左半已被 dispose=背景擦透，source 覆写后仍是 9；右半依旧透明
        assert_eq!(&a.frames[1].rgba[0..4], &[9, 9, 9, 255]);
        assert_eq!(
            &a.frames[1].rgba[4..8],
            &[0, 0, 0, 0],
            "擦除区域外无人动过，仍是透明原样"
        );
    }

    /// 畸形输入一律 `None` 不 panic：截断、坏块长、超限画布、隔行。
    #[test]
    fn malformed_input_never_panics() {
        let good = png_of(&[
            ihdr(1, 1, 8, 6),
            chunk(b"IDAT", &stored(&[0, 1, 2, 3, 255])),
        ]);
        for cut in 0..good.len() {
            let _ = decode(&good[..cut]); // 不许 panic 即通过
        }
        // 声明 4096x4096 的画布：超限拒
        let huge = png_of(&[ihdr(4096, 4096, 8, 6), chunk(b"IDAT", &stored(&[0]))]);
        assert!(decode(&huge).is_none());
        // 隔行（interlace=1）明确不支持
        let mut inter = ihdr(2, 2, 8, 6);
        assert_eq!(inter[8 + 8 + 4], 0); // 原 interlace 字节在 IHDR data 的第 12 位
        inter.insert(inter.len() - 4 - 1, 1); // 把 interlace 改成 1（CRC 前）
        let img = png_of(&[inter, chunk(b"IDAT", &stored(&[0; 12]))]);
        assert!(decode(&img).is_none());
        // 非法位深
        assert!(
            decode(&png_of(&[
                ihdr(1, 1, 7, 6),
                chunk(b"IDAT", &stored(&[0; 4]))
            ]))
            .is_none()
        );
    }
}
