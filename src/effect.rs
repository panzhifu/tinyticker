//! 文字特效与渐变色：把文字先栅格化成单通道覆盖度图，在其上做盒模糊 / 腐蚀 /
//! 位移 / 梯度求边，再合成回预乘 ARGB 画布。
//!
//! 算法对齐 Catime 的 `src/drawing/drawing_effect_*.c`，**但常数按缩放重定**：
//! Catime 作用在几十到上百像素高的 TTF 字形上，padding 12-20、模糊半径 4-12；
//! 我们的字形只有 `8 × scale` 像素（scale 通常 1-5），照抄常数会把字糊成一团。
//! 因此这里所有半径都以「位图像素格」为单位（即 `scale` 的倍数），
//! 视觉上是同一族效果，不是逐像素复刻。

use crate::render::{parse_color, premultiply, Canvas};
use crate::text;

/// 渐变流动一圈的毫秒数（Catime 的时钟路径同样是 2000ms）。
const GRADIENT_PERIOD_MS: u32 = 2000;

/// 渐变停靠点上限（Catime 的 `GRADIENT_MAX_STOPS` 是 20）。
const MAX_STOPS: usize = 20;

/// 文字特效。值串与 Catime 的 `TEXT_EFFECT` 配置项逐字一致。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// 平面绘制（默认）。
    None,
    /// 辉光：模糊副本加法叠加 + 本体压顶。
    Glow,
    /// 玻璃：偏移影子 + 斜面高光 + 竖向 sheen。
    Glass,
    /// 霓虹：腐蚀求管壁 + 双半径光晕 + 亮过阈值转白芯。
    Neon,
    /// 全息：RGB 三通道错位 + 梯度幅度平方描边。
    Holographic,
    /// 液态：正弦位移高度图 + 坡度高光（动画）。
    Liquid,
    /// 水波：value noise 位移 + 模糊辉光（动画）。
    Aqua,
    /// 复古：硬边偏移投影，无模糊。
    Retro,
}

/// 菜单顺序。
pub const EFFECTS: [Effect; 8] = [
    Effect::None,
    Effect::Glow,
    Effect::Glass,
    Effect::Neon,
    Effect::Holographic,
    Effect::Liquid,
    Effect::Aqua,
    Effect::Retro,
];

impl Effect {
    pub fn from_name(name: &str) -> Option<Effect> {
        Some(match name {
            "none" => Effect::None,
            "glow" => Effect::Glow,
            "glass" => Effect::Glass,
            "neon" => Effect::Neon,
            "holographic" => Effect::Holographic,
            "liquid" => Effect::Liquid,
            "aqua" => Effect::Aqua,
            "retro" => Effect::Retro,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Effect::None => "none",
            Effect::Glow => "glow",
            Effect::Glass => "glass",
            Effect::Neon => "neon",
            Effect::Holographic => "holographic",
            Effect::Liquid => "liquid",
            Effect::Aqua => "aqua",
            Effect::Retro => "retro",
        }
    }

    /// 托盘菜单里的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Effect::None => "无",
            Effect::Glow => "辉光",
            Effect::Glass => "玻璃",
            Effect::Neon => "霓虹灯管",
            Effect::Holographic => "全息",
            Effect::Liquid => "液态流动",
            Effect::Aqua => "水波",
            Effect::Retro => "复古投影",
        }
    }

    /// 需要持续重绘的只有这两个；其余特效画一次就定住。
    pub fn animated(self) -> bool {
        matches!(self, Effect::Liquid | Effect::Aqua)
    }
}

/// 横向渐变色：`_` 分隔的若干停靠点，如 `#FF5E96_#56C6FF`。
///
/// 与 Catime 同规则：1 个停靠点就是单色，2 个是静止渐变，
/// **超过 2 个自动横向流动**（Catime 的 `isAnimated = count > 2`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gradient {
    stops: Vec<u32>,
}

impl Gradient {
    pub fn solid(color: u32) -> Self {
        Self { stops: vec![color] }
    }

    /// 解析渐变串；任一段不是颜色就整体返回 `None`，由调用方回落默认值。
    pub fn parse(s: &str) -> Option<Gradient> {
        let stops: Vec<u32> = s.split('_').map(parse_color).collect::<Option<Vec<_>>>()?;
        if stops.is_empty() || stops.len() > MAX_STOPS {
            return None;
        }
        Some(Self { stops })
    }

    pub fn is_gradient(&self) -> bool {
        self.stops.len() > 1
    }

    pub fn animated(&self) -> bool {
        self.stops.len() > 2
    }

    /// 最后一个停靠点。复古特效的影子用它（Catime 渐变路径同样是 endColor）。
    pub fn tail(&self) -> u32 {
        *self.stops.last().unwrap_or(&0)
    }

    /// 写回配置的形式。单色沿用旧的裸 16 进制写法，这样老配置被重写时不会整行变动；
    /// 多停靠点才写成 `#RRGGBB_#RRGGBB`。
    pub fn to_config_string(&self) -> String {
        if self.stops.len() == 1 {
            return format!("{:06x}", self.stops[0] & 0xFFFFFF);
        }
        self.stops.iter().map(|c| format!("#{c:06x}")).collect::<Vec<_>>().join("_")
    }

    /// 取画布第 `x` 列的颜色。Catime 按整个窗口表面横向采样，这里保持一致。
    pub fn at(&self, x: i32, width: u32, phase_ms: u32) -> u32 {
        let n = self.stops.len();
        if n == 1 {
            return self.stops[0];
        }
        let mut u = x.clamp(0, width.saturating_sub(1) as i32) as f32 / width.max(1) as f32;
        if self.animated() {
            let p = (phase_ms % GRADIENT_PERIOD_MS) as f32 / GRADIENT_PERIOD_MS as f32;
            u = pingpong(u + p);
        }
        let f = (u * (n - 1) as f32).clamp(0.0, (n - 1) as f32);
        let i = (f as usize).min(n - 2);
        mix(self.stops[i], self.stops[i + 1], f - i as f32)
    }
}

/// 三角波，周期 2、值域 [0,1]。用它而不是直接取模，是为了让任意调色板流动时
/// 都不出现接缝——Catime 那边靠用户手写镜像调色板达到同样效果。
fn pingpong(v: f32) -> f32 {
    let v = v - (v * 0.5).floor() * 2.0;
    if v < 1.0 { v } else { 2.0 - v }
}

/// 两个 0xRRGGBB 按 `t ∈ [0,1]` 线性插值。
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        (x + (y - x) * t).round().clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 单通道覆盖度图（0-255）。特效的几何全在它上面算。
#[derive(Clone)]
struct Mask {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Mask {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![0; (w * h) as usize] }
    }

    fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.w || y >= self.h {
            0
        } else {
            self.px[(y * self.w + x) as usize]
        }
    }

    /// 钳位采样：模糊窗口越出边界时按最外一行/列取值（Catime 同样钳位）。
    fn clamped(&self, x: i32, y: i32) -> u8 {
        if self.w == 0 || self.h == 0 {
            return 0;
        }
        let x = x.clamp(0, self.w as i32 - 1) as u32;
        let y = y.clamp(0, self.h as i32 - 1) as u32;
        self.get(x, y)
    }

    fn put(&mut self, x: u32, y: u32, v: u8) {
        if x < self.w && y < self.h {
            self.px[(y * self.w + x) as usize] = v;
        }
    }

    /// 取已有值与 `v` 的较大者：文字重叠时覆盖度不该被后画的字削掉。
    fn raise(&mut self, x: u32, y: u32, v: u8) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) as usize;
            self.px[i] = self.px[i].max(v);
        }
    }

    /// 边长 `size` 的实心方块（一个位图像素放大 `scale` 倍）。
    fn block(&mut self, x: i32, y: i32, size: u32, v: u8) {
        for dy in 0..size as i32 {
            for dx in 0..size as i32 {
                let (px, py) = (x + dx, y + dy);
                if px >= 0 && py >= 0 {
                    self.raise(px as u32, py as u32, v);
                }
            }
        }
    }

    /// 可分离单趟盒模糊。名字里的「高斯」在 Catime 那边也是误称。
    fn blurred(&self, r: u32) -> Mask {
        if r == 0 {
            return self.clone();
        }
        let win = 2 * r + 1;
        let mut hpass = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let mut sum = 0u32;
                for k in -(r as i32)..=r as i32 {
                    sum += self.clamped(x as i32 + k, y as i32) as u32;
                }
                hpass.put(x, y, (sum / win) as u8);
            }
        }
        let mut out = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let mut sum = 0u32;
                for k in -(r as i32)..=r as i32 {
                    sum += hpass.clamped(x as i32, y as i32 + k) as u32;
                }
                out.put(x, y, (sum / win) as u8);
            }
        }
        out
    }

    /// 4 邻域取最小：霓虹用它求管壁（轮廓 = 本体 - 腐蚀结果）。
    fn eroded(&self, r: u32) -> Mask {
        let d = r.max(1) as i32;
        let mut out = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let p = self.get(x, y);
                let v = p
                    .min(self.clamped(x as i32 - d, y as i32))
                    .min(self.clamped(x as i32 + d, y as i32))
                    .min(self.clamped(x as i32, y as i32 - d))
                    .min(self.clamped(x as i32, y as i32 + d));
                out.put(x, y, v);
            }
        }
        out
    }

    /// 逐像素饱和相减。
    fn subtract(&self, other: &Mask) -> Mask {
        let mut out = Mask::new(self.w, self.h);
        for (i, v) in out.px.iter_mut().enumerate() {
            *v = self.px[i].saturating_sub(other.px[i]);
        }
        out
    }

    /// 整体平移，空出来的位置留空（影子层用）。
    fn shifted(&self, dx: i32, dy: i32) -> Mask {
        let mut out = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let (sx, sy) = (x as i32 - dx, y as i32 - dy);
                if sx >= 0 && sy >= 0 {
                    out.put(x, y, self.get(sx as u32, sy as u32));
                }
            }
        }
        out
    }

    /// 整体缩放覆盖度：`v × num / den`。
    fn gain(&self, num: u32, den: u32) -> Mask {
        let mut out = Mask::new(self.w, self.h);
        for (i, v) in out.px.iter_mut().enumerate() {
            *v = ((self.px[i] as u32 * num) / den.max(1)) as u8;
        }
        out
    }
}

/// 一行待绘文字。`run` 已经由 [`crate::text`] 按基线栅格化并带绝对坐标，
/// `scale` 只用来定特效的外扩半径与模糊半径（一切以位图像素格为单位）。
pub struct Row<'a> {
    pub run: &'a text::Run,
    pub scale: u32,
}

/// 按特效绘制若干行文字。`phase_ms` 是动画时钟（静态特效忽略它）。
pub fn draw(canvas: &mut Canvas, rows: &[Row], grad: &Gradient, effect: Effect, phase_ms: u32) {
    let (w, h) = canvas.size();
    if w == 0 || h == 0 {
        return;
    }
    // 单色 + 无特效：走平面绘制的快路，一个临时缓冲都不开
    if effect == Effect::None && !grad.is_gradient() {
        let color = premultiply(grad.at(0, w, phase_ms), 0xFF);
        for row in rows {
            canvas.draw_run_centered(row.run, color);
        }
        return;
    }
    // 特效需要的最大外扩：模糊半径 + 位移量，取各特效的上限
    let scale = rows.iter().map(|r| r.scale).max().unwrap_or(1).max(1);
    let pad = effect_pad(effect, scale);
    let mask = build_mask(w + 2 * pad, h + 2 * pad, rows, (pad as i32, pad as i32));
    match effect {
        Effect::None => composite(canvas, &mask, pad, grad, phase_ms, 255),
        Effect::Retro => retro(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Glow => glow(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Neon => neon(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Glass => glass(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Holographic => holographic(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Liquid => liquid(canvas, &mask, pad, grad, phase_ms, scale),
        Effect::Aqua => aqua(canvas, &mask, pad, grad, phase_ms, scale),
    }
}

/// 每个特效需要的外扩像素（含本体偏移与最大模糊半径）。
fn effect_pad(effect: Effect, scale: u32) -> u32 {
    let r = |cells: u32| cells * scale;
    match effect {
        Effect::None => 0,
        Effect::Retro => r(2),
        Effect::Glow => r(4),
        Effect::Neon => r(4),
        Effect::Glass => r(3),
        Effect::Holographic => r(5),
        Effect::Liquid => r(3),
        // 水波的噪声位移幅度最大，外扩要给足
        Effect::Aqua => r(4),
    }
}

/// 把各行文字栅格化成覆盖度图；`off` 是整张图的外扩平移。
///
/// 两种字形都要过这里，因为特效作用的是覆盖度而不是像素：点阵留 0/255 的硬边，
/// TTF 给 0-255 的灰度边。混排行的居中按 [`text::Run::width`]（实测推进量）算。
fn build_mask(w: u32, h: u32, rows: &[Row], off: (i32, i32)) -> Mask {
    let mut m = Mask::new(w, h);
    for row in rows {
        let run = row.run;
        let x0 = ((w as i32 - run.width as i32) / 2).max(0) + off.0;
        for g in &run.glyphs {
            let gx = x0 + g.x;
            let gy = g.y + off.1;
            match &g.body {
                text::GlyphBody::Bits { bits, cell } => {
                    let cell = *cell;
                    for (r, &byte) in bits.iter().enumerate() {
                        if byte == 0 {
                            continue;
                        }
                        for c in 0..8u32 {
                            if byte & (1 << c) != 0 {
                                let (dx, dy) = (c * cell, r as u32 * cell);
                                m.block(gx + dx as i32, gy + dy as i32, cell, 255);
                            }
                        }
                    }
                }
                text::GlyphBody::Gray { gray, w: gw, h: gh, .. } => {
                    for y in 0..*gh {
                        for x in 0..*gw {
                            let v = gray[(y * gw + x) as usize];
                            if v == 0 {
                                continue;
                            }
                            let (px, py) = (gx + x as i32, gy + y as i32);
                            if px >= 0 && py >= 0 {
                                m.raise(px as u32, py as u32, v);
                            }
                        }
                    }
                }
            }
        }
    }
    m
}

/// 遮罩按渐变上色后 src-over 合成。
fn composite(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, gain: u8) {
    let (w, _) = canvas.size();
    for y in 0..mask.h {
        for x in 0..mask.w {
            let a = mask.get(x, y);
            if a == 0 {
                continue;
            }
            let a = (a as u32 * gain as u32 / 255) as u8;
            let color = grad.at(x as i32 - pad as i32, w, phase_ms);
            canvas.over(x as i32 - pad as i32, y as i32 - pad as i32, premultiply(color, a));
        }
    }
}

/// 遮罩加法叠加（光晕与高光）。
fn composite_add(
    canvas: &mut Canvas,
    mask: &Mask,
    pad: u32,
    grad: &Gradient,
    phase_ms: u32,
    gain: u8,
) {
    let (w, _) = canvas.size();
    for y in 0..mask.h {
        for x in 0..mask.w {
            let a = mask.get(x, y);
            if a == 0 {
                continue;
            }
            let a = (a as u32 * gain as u32 / 255) as u8;
            let color = grad.at(x as i32 - pad as i32, w, phase_ms);
            canvas.add(x as i32 - pad as i32, y as i32 - pad as i32, premultiply(color, a));
        }
    }
}

// ---------------------------------------------------------------------------
// 各特效
// ---------------------------------------------------------------------------

/// 复古：硬边偏移投影，无模糊。影子取渐变末色，没有渐变就按亮度自动反色
/// （Catime 的单色路径就是这个判据：暗字配白影、亮字配黑影）。
fn retro(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let off = scale as i32;
    let shadow = if grad.is_gradient() {
        Gradient::solid(grad.tail())
    } else {
        let c = grad.at(0, canvas.size().0, phase_ms);
        let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
        let luma = (r * 299 + g * 587 + b * 114) / 1000;
        Gradient::solid(if luma < 120 { 0xFFFFFF } else { 0x000000 })
    };
    let shadow_mask = mask.shifted(off, off).gain(150, 255);
    composite(canvas, &shadow_mask, pad, &shadow, phase_ms, 255);
    composite(canvas, mask, pad, grad, phase_ms, 255);
}

/// 辉光：两层不同半径的光晕加法叠加，本体压顶。
///
/// 只叠一层在亮背景上几乎看不出来，所以紧贴本体再补一圈小半径。
fn glow(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let s = scale.max(1);
    composite_add(canvas, &mask.blurred(2 * s), pad, grad, phase_ms, 220);
    composite_add(canvas, &mask.blurred(s), pad, grad, phase_ms, 150);
    composite(canvas, mask, pad, grad, phase_ms, 255);
}

/// 霓虹：腐蚀求管壁，远光晕 + 紧灯管 + 亮过阈值的白芯。
///
/// 本体不铺平色——Catime 的霓虹是「替换」字形而不是在字形上再叠实色，
/// 铺上去灯管感就被压成一块方块。
fn neon(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let s = scale.max(1);
    let rim = mask.subtract(&mask.eroded(s));
    let tube = rim.blurred(s);
    let ambient = mask.blurred(2 * s);
    // 环境光压到 80/255：再高整个字就被光晕糊成一团
    composite_add(canvas, &ambient, pad, grad, phase_ms, 55);
    composite_add(canvas, &tube, pad, grad, phase_ms, 200);
    // 白芯阈值 190、斜率 4：只点亮管壁最亮的一条，阈值低了字会被白斑吞掉
    let mut core = Mask::new(mask.w, mask.h);
    for y in 0..mask.h {
        for x in 0..mask.w {
            let v = tube.get(x, y);
            if v > 190 {
                core.put(x, y, ((v - 190) as u32 * 4).min(255) as u8);
            }
        }
    }
    composite_add(canvas, &core, pad, &Gradient::solid(0xFFFFFF), phase_ms, 255);
    composite(canvas, &rim, pad, grad, phase_ms, 255);
}

/// 玻璃：偏移影子 + 斜面（本体与左上/右下偏移之差）+ 竖向 sheen + 高光。
fn glass(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let s = scale.max(1);
    let soft = mask.blurred(s);
    let off = s as i32;
    // 黑色偏移影子
    composite(canvas, &soft.shifted(off, off), pad, &Gradient::solid(0x000000), phase_ms, 150);
    // 斜面：左上受光、右下折射
    let up = mask.subtract(&mask.shifted(off, off));
    let down = mask.subtract(&mask.shifted(-off, -off));
    let white = Gradient::solid(0xFFFFFF);
    for y in 0..mask.h {
        for x in 0..mask.w {
            let src = mask.get(x, y);
            if src == 0 {
                continue;
            }
            // 竖向 sheen：顶部亮、底部暗
            let sheen = 255u32.saturating_sub(y * 255 / mask.h.max(1)) >> 3;
            let a = ((src as u32 * 3) / 4 + sheen).min(255) as u8;
            let color = grad.at(x as i32 - pad as i32, canvas.size().0, phase_ms);
            canvas.over(x as i32 - pad as i32, y as i32 - pad as i32, premultiply(color, a));
        }
    }
    // 高光：斜面差值强的地方叠白。权重压得低，否则本体颜色会被洗成一片白
    // 权重压得很低：位图字体的笔画只有 1-2 格宽，几乎每个像素都算「边缘」，
    // 按 Catime 那种大字号的强度叠白会把渐变整个洗掉
    composite_add(canvas, &up.gain(40, 255), pad, &white, phase_ms, 255);
    composite_add(canvas, &down.gain(16, 255), pad, &white, phase_ms, 255);
}

/// 全息：三通道各按错位的覆盖度取样叠出彩虹边 + 梯度幅度平方当亮描边 + 本体。
fn holographic(
    canvas: &mut Canvas,
    mask: &Mask,
    pad: u32,
    grad: &Gradient,
    phase_ms: u32,
    scale: u32,
) {
    let s = scale.max(1);
    let soft = mask.blurred(s).blurred(s);
    let d = s as i32;
    composite_add(canvas, &soft.shifted(d, 0), pad, &Gradient::solid(0xFF3030), phase_ms, 170);
    composite_add(canvas, &soft, pad, &Gradient::solid(0x30FF60), phase_ms, 170);
    composite_add(canvas, &soft.shifted(-d, 0), pad, &Gradient::solid(0x4060FF), phase_ms, 170);
    // 描边：梯度幅度 ×85/255 后再平方，只留最陡的那一条
    let mut rim = Mask::new(mask.w, mask.h);
    for y in 0..mask.h {
        for x in 0..mask.w {
            let l = soft.clamped(x as i32 - d, y as i32) as i32;
            let r = soft.clamped(x as i32 + d, y as i32) as i32;
            let u = soft.clamped(x as i32, y as i32 - d) as i32;
            let b = soft.clamped(x as i32, y as i32 + d) as i32;
            let mag = ((r - l).unsigned_abs() + (b - u).unsigned_abs()).min(510);
            let v = (mag * 85 / 255).min(255);
            rim.put(x, y, (v * v / 255).min(200) as u8);
        }
    }
    composite_add(canvas, &rim, pad, &Gradient::solid(0xFFFFFF), phase_ms, 255);
    composite(canvas, mask, pad, grad, phase_ms, 235);
}

/// 正弦查表：256 项，值域 ±1024（定点），用的时候再按需缩放。
fn sine_lut() -> Vec<i32> {
    (0..256).map(|i| ((i as f32 * std::f32::consts::TAU / 256.0).sin() * 1024.0) as i32).collect()
}

/// 液态流动一圈的毫秒数（Catime 用 2048 项 LUT × 0.815 步进得出 2513ms，这里同周期）。
const LIQUID_PERIOD_MS: u32 = 2513;
/// 三个波源互相错开 1/3 周期（Catime 的 +682 / +1365 换算到 256 项）。
const LIQUID_PHASE2: i32 = 85;
const LIQUID_PHASE3: i32 = 170;
/// 纵向波用的 y 系数：Catime 的 `screenY * 222 >> 8`，即每 1.15px 推进一格。
const LIQUID_Y_SCALE: i32 = 222;

/// 液态：模糊成高度图，三段不同相位的正弦叠加做位移，再按坡度高光。
fn liquid(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let height = mask.blurred(scale.max(1));
    let lut = sine_lut();
    let (w, h) = (mask.w, mask.h);
    let t = (phase_ms as i64 * 256 / LIQUID_PERIOD_MS as i64) as i32;
    // 幅度 ±1 格：Catime 的 ±4px 是相对几十像素高的字形，我们只有 8×scale
    let amp = scale as i32;
    let wave = |idx: i32| -> i32 { lut[idx.rem_euclid(256) as usize] * amp / 1024 };
    for y in 0..h {
        let yterm = (y as i32 * LIQUID_Y_SCALE) >> 8;
        for x in 0..w {
            let t1 = x as i32 * 2 + t;
            let t2 = (x as i32 / 2) + yterm + t + LIQUID_PHASE2;
            let t3 = -(x as i32 / 2) + yterm + t + LIQUID_PHASE3;
            let (w1, w2, w3) = (wave(t1), wave(t2), wave(t3));
            let wx = (w1 + w2) / 2;
            let wy = (wave(t1 + 62) + w2 - w3) / 2;
            let v = height.clamped(x as i32 + wx, y as i32 + wy);
            if v < 24 {
                continue; // 边缘淡出，与 Catime 的 mass<24 同判据
            }
            let a = ((v as u32 - 24) << 3).min(255) as u8;
            let color = grad.at(x as i32 - pad as i32, canvas.size().0, phase_ms);
            canvas.over(x as i32 - pad as i32, y as i32 - pad as i32, premultiply(color, a));
        }
    }
    // 高光：取横向坡度大的地方叠白
    let slope = height.subtract(&height.shifted(1, 0));
    composite_add(canvas, &slope.gain(90, 255), pad, &Gradient::solid(0xFFFFFF), phase_ms, 255);
}

/// 水波：value noise 位移 + 模糊辉光偏移 + 本体。
fn aqua(canvas: &mut Canvas, mask: &Mask, pad: u32, grad: &Gradient, phase_ms: u32, scale: u32) {
    let (w, h) = (mask.w, mask.h);
    // 噪声晶格要粗到「几个字形格一个随机点」。按 1/2 分辨率取点的话每 2px 就是一个
    // 独立随机值，双线性也救不回来，16px 高的字会被搅成碎片
    let step = (4 * scale).max(8) as f32;
    let (nw, nh) = ((w as f32 / step) as u32 + 3, (h as f32 / step) as u32 + 3);
    let seed = (phase_ms / 14000 * 7) as i32; // RIPPLE_FLOW_MS / CYCLES
    let mut grid = vec![0u8; (nw * nh) as usize];
    for y in 0..nh {
        for x in 0..nw {
            grid[(y * nw + x) as usize] = value_noise(x as i32, y as i32, seed);
        }
    }
    let sample_grid = |x: u32, y: u32| -> u8 {
        let fx = x as f32 / step;
        let fy = y as f32 / step;
        let x0 = fx as u32;
        let y0 = fy as u32;
        // 小数部分要放大到 0-255 才能当插值权重；忘了放大等于退化成最近邻，
        // 位移场会变成一格一格的硬块，字被切得稀碎
        let tx = ((fx - x0 as f32) * 255.0) as u32;
        let ty = ((fy - y0 as f32) * 255.0) as u32;
        let g = |dx: u32, dy: u32| {
            *grid.get(((y0 + dy) * nw + (x0 + dx)) as usize).unwrap_or(&0) as u32
        };
        let a = g(0, 0) * (255 - tx) / 255 + g(1, 0) * tx / 255;
        let b = g(0, 1) * (255 - tx) / 255 + g(1, 1) * tx / 255;
        ((a * (255 - ty) + b * ty) / 255) as u8
    };
    let disp = scale as i32;
    let warped = {
        let mut m = Mask::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let n = sample_grid(x, y) as i32;
                let ox = (n - 128) * disp / 128;
                let oy = (sample_grid(x + 1, y + 1) as i32 - 128) * disp / 128;
                m.put(x, y, mask.clamped(x as i32 + ox, y as i32 + oy));
            }
        }
        m
    };
    let soft = warped.blurred(scale.max(1));
    let off = scale as i32;
    // 偏移辉光（Catime 读 j-shadowOffset，alpha = glow × 90/255）
    composite_add(canvas, &soft.shifted(off, off).gain(90, 255), pad, grad, phase_ms, 255);
    composite(canvas, &warped, pad, grad, phase_ms, 255);
}

/// 整数哈希 → [0,255] 的 value noise。常数取自 Catime 的 aqua_noise.c。
fn value_noise(x: i32, y: i32, seed: i32) -> u8 {
    let h = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263))
        .wrapping_add((seed as u32).wrapping_mul(2246822519));
    let h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    ((h ^ (h >> 16)) & 0xFF) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_names_roundtrip() {
        for e in EFFECTS {
            assert_eq!(Effect::from_name(e.name()), Some(e));
        }
        assert_eq!(Effect::from_name("bloom"), None);
        // 只有液态与水波需要持续重绘
        for e in EFFECTS {
            let animates = matches!(e, Effect::Liquid | Effect::Aqua);
            assert_eq!(e.animated(), animates, "{} 的动画属性不对", e.name());
        }
    }

    #[test]
    fn gradient_parsing_follows_catime_syntax() {
        let g = Gradient::parse("#FF5E96_#56C6FF").unwrap();
        assert!(g.is_gradient() && !g.animated());
        assert_eq!(g.tail(), 0x56C6FF);
        // 3 个以上自动流动
        let g3 = Gradient::parse("#FF0000_#00FF00_#0000FF").unwrap();
        assert!(g3.animated());
        // 单色不是渐变
        let one = Gradient::parse("#ABCDEF").unwrap();
        assert!(!one.is_gradient() && !one.animated());
        assert_eq!(one.at(1234, 10, 0), 0xABCDEF);
        // 任一段非法就整体失败
        assert!(Gradient::parse("#FF5E96_不是颜色").is_none());
        assert!(Gradient::parse("").is_none());
    }

    #[test]
    fn gradient_endpoints_hit_the_stops() {
        let g = Gradient::parse("#000000_#FFFFFF").unwrap();
        assert_eq!(g.at(0, 100, 0), 0x000000);
        // 采样按列中心，最后一列是 99/100 而不是 1，留一点量化余量
        let tail = g.at(99, 100, 0);
        assert!(tail & 0xFF >= 0xFA && (tail >> 16) >= 0xFA, "末段该接近白: {tail:#08x}");
        let mid = g.at(50, 100, 0);
        assert!((0x7A..=0x86).contains(&(mid & 0xFF)), "中点该接近灰: {mid:#08x}");
        // 三停靠点：正中间那列该正好是中间那个颜色
        let g3 = Gradient::parse("#000000_#FF0000_#000000").unwrap();
        let c = g3.at(50, 100, 0);
        assert_eq!((c >> 16) & 0xFF, 0xFF, "中点该是红的");
        assert_eq!(c & 0x00FFFF, 0, "中点不该带绿蓝");
    }

    /// 流动渐变必须无缝：相位回到起点时颜色也得回到起点。
    #[test]
    fn animated_gradient_loops_without_a_seam() {
        let g = Gradient::parse("#112233_#445566_#778899").unwrap();
        let at = |ms| g.at(30, 100, ms);
        assert_eq!(at(0), at(GRADIENT_PERIOD_MS), "一整圈后该回到原色");
        // 半圈处不该出现跳变：相邻 1ms 的差值要小
        let a = at(1000);
        let b = at(1001);
        for shift in [16, 8, 0] {
            let d = ((a >> shift) & 0xFF) as i32 - ((b >> shift) & 0xFF) as i32;
            assert!(d.abs() < 12, "相位 {shift} 处跳变 {d}");
        }
    }

    #[test]
    fn mix_is_channel_wise_lerp() {
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
        // 越界的 t 要被钳住
        assert_eq!(mix(0x102030, 0x405060, -1.0), 0x102030);
        assert_eq!(mix(0x102030, 0x405060, 2.0), 0x405060);
    }

    #[test]
    fn box_blur_spreads_a_single_pixel() {
        let mut m = Mask::new(9, 9);
        m.put(4, 4, 255);
        let b = m.blurred(1);
        // 中心被 3×3 平均后变暗，四邻被抬起来
        assert!(b.get(4, 4) < 255 && b.get(4, 4) > 0);
        assert!(b.get(3, 4) > 0 && b.get(5, 4) > 0);
        assert!(b.get(0, 0) == 0, "离得远的地方不该被糊到");
        // 半径 0 是恒等
        assert_eq!(m.blurred(0).get(4, 4), 255);
    }

    #[test]
    fn erode_and_subtract_isolate_an_outline() {
        let mut m = Mask::new(11, 11);
        for y in 2..9 {
            for x in 2..9 {
                m.put(x, y, 255);
            }
        }
        let rim = m.subtract(&m.eroded(1));
        // 实心方块只有最外一圈是轮廓
        assert!(rim.get(2, 2) > 0, "角上该有轮廓");
        assert!(rim.get(5, 5) == 0, "中心不该有轮廓");
    }

    /// 每个特效都要能在 1× 到 3× 缩放下画完不 panic，且真的改变像素。
    #[test]
    fn every_effect_paints_at_every_scale() {
        for e in EFFECTS {
            for scale in [1u32, 2, 5] {
                let grad = Gradient::parse("#FF5E96_#56C6FF_#FFE066").unwrap();
                let mut buf = vec![0xFF000000u32; 200 * 60];
                {
                    let mut c = Canvas::new(&mut buf, 200, 60);
                    let num = text::shape("25:00", scale, 10);
                    let status = text::shape("WORK 1", scale, 40);
                    let rows = [Row { run: &num, scale }, Row { run: &status, scale }];
                    draw(&mut c, &rows, &grad, e, 700);
                }
                let painted = buf.iter().filter(|p| **p != 0xFF000000).count();
                assert!(painted > 20, "{} 在 {scale}× 下几乎没画东西（{painted} 像素）", e.name());
            }
        }
    }

    /// 空文本、零尺寸画布都不该 panic——特效路径比平面路径更容易踩到除零。
    #[test]
    fn degenerate_inputs_are_boring_not_fatal() {
        let grad = Gradient::solid(0xFFFFFF);
        let empty = text::shape("", 2, 0);
        let rows = [Row { run: &empty, scale: 2 }];
        let mut buf: Vec<u32> = Vec::new();
        let mut c = Canvas::new(&mut buf, 0, 0);
        for e in EFFECTS {
            draw(&mut c, &rows, &grad, e, 0);
        }
        assert!(buf.is_empty());
    }

    /// 两条绘制路径必须落在同一批像素上：单色无特效走平面快路（不开临时缓冲），
    /// 一旦有渐变就改走覆盖度图。二者 footprint 不同就说明 mask 那边漏字或多字。
    #[test]
    fn flat_and_mask_paths_agree_on_footprint() {
        let run = text::shape("1:00", 2, 10);
        let color = premultiply(0xFFCC50, 0xFF);
        let mut flat = vec![0u32; 200 * 60];
        {
            let mut c = Canvas::new(&mut flat, 200, 60);
            c.draw_run_centered(&run, color);
        }
        let mut masked = vec![0u32; 200 * 60];
        {
            let mut c = Canvas::new(&mut masked, 200, 60);
            let rows = [Row { run: &run, scale: 2 }];
            // 两个停靠点同色：`is_gradient()` 为真，强制走覆盖度图那条路
            draw(&mut c, &rows, &Gradient::parse("FFCC50_FFCC50").unwrap(), Effect::None, 0);
        }
        let lit = |buf: &[u32]| {
            buf.iter()
                .enumerate()
                .filter(|(_, p)| **p != 0)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        let (a, b) = (lit(&flat), lit(&masked));
        assert!(!a.is_empty(), "平面路径一个字都没画");
        assert_eq!(a, b, "两条路径的着墨位置不一致");
    }
}
