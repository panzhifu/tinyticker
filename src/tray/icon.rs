//! 32x32 托盘图标的像素绘制，以及悬停提示的文本。全部程序化生成，不引入图片资源。

use crate::gif;
use crate::sysinfo;
use super::*;

/// 32x32 时钟图标（深色表盘 + 白色表圈和指针），输出 ARGB32 网络字节序。
///
/// 与悬浮窗的位图字体同一取向：不引入任何图片资源文件。
pub(super) fn clock_pixmap(now: (u32, u32, u32)) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    let (h, m, _) = now;
    // 表盘位置以「分钟格」为单位：时针含分钟分量，否则一小时里指针会跳一下
    hand(&mut px, dial_dir((h % 12) as f32 * 5.0 + m as f32 / 12.0), 8.0);
    hand(&mut px, dial_dir(m as f32), 12.0);
    (S as i32, S as i32, px)
}

/// 32x32 表盘底：白色圆环 + 深色填充，圆外全透明。
pub(super) fn face() -> Vec<u8> {
    let mut px = vec![0u8; S * S * 4]; // 圆外全透明
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f32 - CENTER, y as f32 - CENTER);
            if dist(dx, dy) > 15.0 {
                continue;
            }
            let (r, g, b) = if dist(dx, dy) > 12.5 { (255, 255, 255) } else { DARK };
            put(&mut px, x, y, r, g, b);
        }
    }
    px
}

/// 60 个整分钟方向的单位向量（0 = 12 点，顺时针），×4096 定点。
///
/// 用固定 6° 增量旋转累加生成，而不是调 `sin`/`cos`：后者会让我们去链 libm，
/// 而本项目的卖点是 `ldd` 里只有 libc 与 libgcc_s。所选整数 (4074, 428) 的模长
/// 比 4096 大 0.01%，转满一圈累计误差不到 1 像素。
pub(super) const DIAL: [(i32, i32); 60] = {
    let mut t = [(0i32, 0i32); 60];
    let (mut x, mut y) = (0i32, -4096i32); // 12 点方向：屏幕 y 轴向下，故取负
    let mut i = 0;
    while i < 60 {
        t[i] = (x, y);
        let (nx, ny) = ((x * 4074 - y * 428) >> 12, (x * 428 + y * 4074) >> 12);
        (x, y) = (nx, ny);
        i += 1;
    }
    t
};

/// 第 60 格回到起点，因此 `pos` 可取任意 [0, 60) 的实数，相邻两格线性插值。
pub(super) fn dial_dir(pos: f32) -> (f32, f32) {
    let lo = pos.floor() as usize % 60;
    let hi = (lo + 1) % 60;
    let f = pos - pos.floor();
    let (a, b) = (DIAL[lo], DIAL[hi]);
    (
        (a.0 as f32 + (b.0 - a.0) as f32 * f) / 4096.0,
        (a.1 as f32 + (b.1 - a.1) as f32 * f) / 4096.0,
    )
}

/// 从圆心沿单位向量 `(sx, sy)` 画一条 `len` 长的 2px 指针。
pub(super) fn hand(px: &mut [u8], (sx, sy): (f32, f32), len: f32) {
    for step in 0..(len * 2.0) as usize {
        let t = step as f32 / 2.0;
        // 2px 粗：沿指针方向再错开半像素画一次
        put(px, (CENTER + sx * t) as usize, (CENTER + sy * t) as usize, 255, 255, 255);
        put(px, (CENTER + sx * (t + 0.5)) as usize, (CENTER + sy * (t + 0.5)) as usize, 255, 255, 255);
    }
}

/// 占用表：深色内盘自底向上填到与 `percent` 对应的水位线，颜色按阈值分级。
/// 没有电池的机器选电池档时显示空心盘，与「0%」区分开。
pub(super) fn gauge_pixmap(percent: Option<u8>, charging: bool) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    if let Some(p) = percent {
        // 充电中一律绿色：20% 的红色会让人以为快没电，而实际在涨
        let (r, g, b) = if charging { (80, 220, 120) } else { level_color(p) };
        // 水位线的 y 偏移：0% 在顶端（不填充），100% 在底端（填满内盘）
        let line = 12.5 - p as f32 * 0.25;
        for y in 0..S {
            for x in 0..S {
                let (dx, dy) = (x as f32 - CENTER, y as f32 - CENTER);
                if dist(dx, dy) > 12.5 || dy < line {
                    continue;
                }
                put(&mut px, x, y, r, g, b);
            }
        }
    }
    (S as i32, S as i32, px)
}

/// 字节速率 → 人话。50 KB/s 以下直接给整数字节，以上按 1024 递进到 mantissa 落在
/// `[50, 1024)` 的那一档，保留一位小数（`51199 B/s` / `50.0 KB/s` / `2.0 MB/s`）。
pub(super) fn fmt_rate(bps: u64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    if bps < 51_200 {
        return format!("{bps} {}", UNITS[0]);
    }
    let mut v = bps as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// 悬停提示的正文：把已经采到的指标拼成一行。没有电池就不写那一段。
pub(super) fn tooltip_text(src: &sysinfo::Sources) -> String {
    let mut s = format!(
        "CPU {}% · 内存 {}% · ↓ {} ↑ {}",
        src.cpu,
        src.mem,
        fmt_rate(src.net_down),
        fmt_rate(src.net_up)
    );
    if let Some(b) = src.battery {
        s.push_str(&format!(" · 电池 {}%{}", b.percent, if b.charging { "⚡" } else { "" }));
    }
    s
}

/// 网络水位表的刻度：上下行取大的那个，按**对数**映到 0-100。
///
/// 用对数是因为带宽跨六个数量级：线性刻度的话，浏览网页与满速下载会挤在同一格水位上。
/// 区间取 `1 KB/s = 空盘 … 10 MB/s = 满盘`——低于 1 KB/s 的零星心跳算静默，否则桌面
/// 挂着不动也会显示三成满。
pub(super) fn net_level(down: u64, up: u64) -> u8 {
    const NET_MIN_BPS: u64 = 1024;
    const NET_MAX_BPS: u64 = 10 * 1024 * 1024;
    let rate = down.max(up);
    if rate < NET_MIN_BPS {
        return 0;
    }
    // 按"字节数的二进制位长"铺开：每翻一倍涨固定一格
    let bits = |v: u64| 64 - v.leading_zeros();
    let min_bits = bits(NET_MIN_BPS);
    let span = bits(NET_MAX_BPS) - min_bits;
    ((bits(rate) - min_bits) * 100 / span).min(100) as u8
}

/// 按 `mode` 生成当前该显示的图标。
pub(super) fn icon_pixmap(
    mode: IconMode,
    src: &sysinfo::Sources,
    now: (u32, u32, u32),
    player: &mut gif::Player,
) -> (i32, i32, Vec<u8>) {
    match mode {
        IconMode::Clock => clock_pixmap(now),
        IconMode::Cpu => gauge_pixmap(Some(src.cpu), false),
        IconMode::Memory => gauge_pixmap(Some(src.mem), false),
        IconMode::Battery => {
            gauge_pixmap(src.battery.map(|b| b.percent), src.battery.is_some_and(|b| b.charging))
        }
        IconMode::Network => gauge_pixmap(Some(net_level(src.net_down, src.net_up)), false),
        // 动图解不出（没配 / 文件坏了）就退回真实时表盘，图标位不能空着
        IconMode::Gif => match player.current().map(|(f, w, h)| gif_pixmap(f, w, h)) {
            Some(px) => (S as i32, S as i32, px),
            None => clock_pixmap(now),
        },
    }
}

/// 把动图的一帧最近邻采样到 32x32 并转成 SNI 的 A,R,G,B 字节序。
/// 尺寸固定成 32 是为了不动 `PIX_W`/`PIX_H`：宿主自己会再缩放，我们只保证一格一像素。
pub(super) fn gif_pixmap(frame: &gif::Frame, width: u16, height: u16) -> Vec<u8> {
    let (fw, fh) = (usize::from(width), usize::from(height));
    let mut px = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let sx = x * fw / S;
            let sy = y * fh / S;
            let o = (sy * fw + sx) * 4;
            let i = (y * S + x) * 4;
            px[i] = frame.rgba[o + 3];
            px[i + 1] = frame.rgba[o];
            px[i + 2] = frame.rgba[o + 1];
            px[i + 3] = frame.rgba[o + 2];
        }
    }
    px
}

pub(super) fn dist(dx: f32, dy: f32) -> f32 {
    (dx * dx + dy * dy).sqrt()
}

pub(super) fn level_color(percent: u8) -> (u8, u8, u8) {
    match percent {
        0..=59 => (80, 220, 120),
        60..=84 => (255, 200, 80),
        _ => (240, 90, 90),
    }
}

pub(super) const S: usize = 32;
pub(super) const CENTER: f32 = 15.5;
pub(super) const DARK: (u8, u8, u8) = (15, 15, 20);

/// SNI 的 IconPixmap 是网络字节序（大端）的 A,R,G,B 四字节。
pub(super) fn put(px: &mut [u8], x: usize, y: usize, r: u8, g: u8, b: u8) {
    let i = (y * 32 + x) * 4;
    px[i] = 255;
    px[i + 1] = r;
    px[i + 2] = g;
    px[i + 3] = b;
}

// ---------------------------------------------------------------------------
// 对外接口
// ---------------------------------------------------------------------------


#[cfg(test)]
mod tests {
    use super::*;


    /// 占用表里水位填充的像素数（排除白色圆环与深色底）。
    fn filled(px: &[u8]) -> usize {
        const RING: (u8, u8, u8) = (255, 255, 255);
        px.chunks(4)
            .filter(|p| p[0] != 0 && !matches!((p[1], p[2], p[3]), RING | DARK))
            .count()
    }

    #[test]
    fn gauge_fills_monotonically() {
        let counts = [0u8, 25, 50, 75, 100].map(|p| filled(&gauge_pixmap(Some(p), false).2));
        assert!(counts.windows(2).all(|w| w[0] < w[1]), "水位应随占用率单调上升: {counts:?}");
        // 100% 时整个内盘被填满（内盘半径 12.5）
        assert!(counts[4] > 450, "满盘像素过少: {}", counts[4]);
        // 0% 只剩水位线那一条缝
        assert!(counts[0] < 10, "0% 不该有明显填充: {}", counts[0]);
        // 25% 的水位在内盘下沿到圆心之间
        assert!(counts[1] > counts[0] && counts[1] < counts[2]);
    }

    #[test]
    fn dial_table_matches_true_trig() {
        // 表本身是定点增量旋转的产物；拿真三角函数对一遍，误差按 1px（半径 12）内算。
        // 只在测试里用 sin/cos——它们不会进 release 二进制，libm 因此仍不在 ldd 里。
        for i in 0..60 {
            let deg = i as f32 * 6.0;
            let ideal = (deg.to_radians().sin(), -deg.to_radians().cos());
            let got = dial_dir(i as f32);
            assert!(
                (got.0 - ideal.0).abs() < 0.01 && (got.1 - ideal.1).abs() < 0.01,
                "第 {i} 格偏了: {got:?} vs {ideal:?}"
            );
            let len = (got.0 * got.0 + got.1 * got.1).sqrt();
            assert!((len - 1.0).abs() < 0.01, "第 {i} 格不是单位向量: {len}");
        }
        // 四个正点方向必须落在轴上
        assert_eq!(dial_dir(0.0), (0.0, -1.0));
        assert!(dial_dir(15.0).0 > 0.99 && dial_dir(15.0).1.abs() < 0.01);
        assert!(dial_dir(30.0).1 > 0.99 && dial_dir(30.0).0.abs() < 0.01);
        assert!(dial_dir(45.0).0 < -0.99 && dial_dir(45.0).1.abs() < 0.01);
        // 插值必须单调：同一象限里序号越大 x 越大
        for i in 0..14 {
            assert!(dial_dir(i as f32 + 0.5).0 > dial_dir(i as f32).0);
        }
    }

    #[test]
    fn empty_battery_shows_hollow_face() {
        // 没有电池的机器选 battery 档：只有表盘，不画饼
        assert_eq!(filled(&gauge_pixmap(None, false).2), 0);
    }

    #[test]
    fn clock_hands_stay_inside_the_face() {
        // 任一时刻都不该越出内盘或 panic（put 不做边界检查，越界即 panic）
        for m in (0..60).chain([59]) {
            for h in [0u32, 3, 6, 9, 12, 18, 23] {
                let px = clock_pixmap((h, m, 0)).2;
                assert_eq!(px.len(), S * S * 4);
            }
        }
    }

    /// 速率单位按 1024 递进，小流量保留整数字节。
    #[test]
    fn rate_formatting() {
        assert_eq!(fmt_rate(0), "0 B/s");
        assert_eq!(fmt_rate(51_199), "51199 B/s", "50 KB/s 以下直接给字节");
        assert_eq!(fmt_rate(51_200), "50.0 KB/s");
        assert_eq!(fmt_rate(2 * 1024 * 1024), "2.0 MB/s");
        assert_eq!(fmt_rate(40u64 * 1024 * 1024 * 1024), "40.0 GB/s");
        assert!(fmt_rate(u64::MAX).ends_with("GB/s"), "封顶在 GB/s，不外推到 TB");
    }

    /// 提示正文：没电池就不写那一段，有就把充电标记带上。
    #[test]
    fn tooltip_lists_what_was_sampled() {
        let src = sysinfo::Sources {
            cpu: 12,
            mem: 44,
            battery: None,
            net_down: 1024 * 1024,
            net_up: 2048,
        };
        assert_eq!(tooltip_text(&src), "CPU 12% · 内存 44% · ↓ 1.0 MB/s ↑ 2048 B/s");
        let with_batt = sysinfo::Sources {
            battery: Some(sysinfo::Battery { percent: 87, charging: true }),
            ..src
        };
        assert!(tooltip_text(&with_batt).ends_with("· 电池 87%⚡"), "充电标记该在末尾");
    }

    /// 网络水位是对数刻度：静默归零、区间内分得开、封顶不溢出。
    #[test]
    fn net_level_is_log_scaled_and_bounded() {
        assert_eq!(net_level(0, 0), 0, "静默该是空盘");
        assert_eq!(net_level(900, 12), 0, "低于 1 KB/s 的零星心跳算静默");
        let small = net_level(64 * 1024, 0);
        let big = net_level(2 * 1024 * 1024, 0);
        assert!(0 < small && small < big && big < 100, "刻度要分得开: {small} {big}");
        assert_eq!(net_level(10 * 1024 * 1024, 0), 100);
        assert_eq!(net_level(u64::MAX, 0), 100, "超量要夹紧而不是回绕");
        // 上下行取大：只有上传在跑也该看得见
        assert_eq!(net_level(0, 2 * 1024 * 1024), big);
    }
}
