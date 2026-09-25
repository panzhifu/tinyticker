//! 32x32 托盘图标的像素绘制，以及悬停提示的文本。全部程序化生成，不引入图片资源。

use super::*;
use crate::gif;
use crate::sysinfo;

/// 32x32 时钟图标（深色表盘 + 白色表圈和指针），输出 ARGB32 网络字节序。
///
/// 与悬浮窗的位图字体同一取向：不引入任何图片资源文件。
pub(super) fn clock_pixmap(now: (u32, u32, u32)) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    let (h, m, _) = now;
    // 表盘位置以「分钟格」为单位：时针含分钟分量，否则一小时里指针会跳一下
    hand(
        &mut px,
        dial_dir((h % 12) as f32 * 5.0 + m as f32 / 12.0),
        8.0,
    );
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
            let (r, g, b) = if dist(dx, dy) > 12.5 {
                (255, 255, 255)
            } else {
                DARK
            };
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
        put(
            px,
            (CENTER + sx * t) as usize,
            (CENTER + sy * t) as usize,
            255,
            255,
            255,
        );
        put(
            px,
            (CENTER + sx * (t + 0.5)) as usize,
            (CENTER + sy * (t + 0.5)) as usize,
            255,
            255,
            255,
        );
    }
}

/// 占用表：深色内盘自底向上填到与 `percent` 对应的水位线，颜色按阈值分级。
/// 没有电池的机器选电池档时显示空心盘，与「0%」区分开。
pub(super) fn gauge_pixmap(percent: Option<u8>, charging: bool) -> (i32, i32, Vec<u8>) {
    let mut px = face();
    if let Some(p) = percent {
        // 充电中一律绿色：20% 的红色会让人以为快没电，而实际在涨
        let (r, g, b) = if charging {
            (80, 220, 120)
        } else {
            level_color(p)
        };
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
/// 文案走全局语言（托盘线程与主线程同进程，启动时已落定）。
pub(super) fn tooltip_text(src: &sysinfo::Sources) -> String {
    use crate::lang::tr;
    let mut s = format!(
        "CPU {}% · {} {}% · ↓ {} ↑ {}",
        src.cpu,
        tr("内存", "Mem"),
        src.mem,
        fmt_rate(src.net_down),
        fmt_rate(src.net_up)
    );
    if let Some(b) = src.battery {
        s.push_str(&format!(
            " · {} {}%{}",
            tr("电池", "Batt"),
            b.percent,
            if b.charging { "⚡" } else { "" }
        ));
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

/// 数字档的底：圆角方块。
///
/// 不用 [`face()`] 那个圆盘是因为算过账——内盘半径 12.5，在 y=7 那一行只剩 18 px 宽，
/// 而一行速率要 32 px。字会骑到白表圈上（第一版实测就是这样）。方块只把四角削掉，
/// 中间两行是满宽的。
fn num_face() -> Vec<u8> {
    let mut px = vec![0u8; S * S * 4];
    let r = 6.0f32;
    let edge = S as f32 - 1.0 - r;
    for y in 0..S {
        for x in 0..S {
            // 到最近那颗角圆心的距离：超过 r 就在被削掉的角上，留空
            let d = dist(
                x as f32 - (x as f32).clamp(r, edge),
                y as f32 - (y as f32).clamp(r, edge),
            );
            if d > r {
                continue;
            }
            // 最外一圈换成灰边：纯 DARK 在浅色主题下等于没有轮廓
            if d > r - 1.2 {
                put(&mut px, x, y, 90, 90, 100);
            } else {
                put(&mut px, x, y, DARK.0, DARK.1, DARK.2);
            }
        }
    }
    px
}

/// 数字档：把百分比烤进图标里。Catime 的 `CreatePercentIcon16`（`percent_text.c:60-135`）
/// 是同一件事，只是它用 `TextOutW` 而这里用内置点阵——二进制里不该有字体。
/// 没有电池的机器选这一档仍是空底，与"0%"区分开。
fn percent_pixmap(percent: Option<u8>, charging: bool) -> (i32, i32, Vec<u8>) {
    let mut px = num_face();
    if let Some(p) = percent {
        // 充电中一律绿色，与水位档同一条规则
        let c = if charging {
            (80, 220, 120)
        } else {
            level_color(p)
        };
        // 一律不压扁：丢列会把 `M` `0` 这类字形啃坏，实测比顶边更难看
        draw_row(&mut px, &format!("{p}%"), (S - 8) / 2, c, 0);
    }
    (S as i32, S as i32, px)
}

/// 网络的数字档是两行：上面一行是下行流量，下面一行是上行流量（下载在上的读法与
/// 资源管理器一致，配色沿用 `level_color` 那套绿/琥珀）。
fn net_pixmap(down: u64, up: u64) -> (i32, i32, Vec<u8>) {
    let mut px = num_face();
    // 不带 `D` / `U` 前缀：加上就五字，压扁会啃掉字形，不压又超出 32 px。方向靠
    // "上=下行流量、绿；下=上行流量、琥珀"这两条约定，与悬停提示里的 ↓↑ 同一顺序
    draw_row(&mut px, &rate_short(down), 7, (80, 220, 120), 0);
    draw_row(&mut px, &rate_short(up), 17, (255, 200, 80), 0);
    (S as i32, S as i32, px)
}

/// 给图标用的紧凑速率：最多四个字符（`0B` / `983B` / `332K` / `1.1M`）。
///
/// 与 [`fmt_rate`] 的分工是"给谁看"：悬停提示里单位要拼全（`1.2 MB/s`），图标里
/// 一个字符都嫌多。
fn rate_short(bps: u64) -> String {
    const UNITS: [char; 4] = ['B', 'K', 'M', 'G'];
    let mut v = bps as f64;
    let mut u = 0;
    while v >= 999.5 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    // 封顶那一档不再往上走，所以位数会失控；计数器被重置时确实可能算出荒谬的差值，
    // 而图标只有 32 px——宁可写 999G 也不许把字画出盘外
    let v = v.min(999.0);
    if u == 0 {
        format!("{v:.0}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// 把一行 ASCII 居中画进 32x32。`narrow` 是每个字丢掉的右列数：位序是 bit0 在最左，
/// 所以丢右边不影响字形骨架。
///
/// 需要丢列是因为 `D1.2M` 这种五字串按 8 px 一格要 40 px，图标只有 32 px；压到
/// 6 px 一格刚好。百分号那一档四字以内，`narrow = 0` 不压。
fn draw_row(px: &mut [u8], s: &str, y: usize, color: (u8, u8, u8), narrow: usize) {
    let w = 8 - narrow;
    let x0 = S.saturating_sub(s.len() * w) / 2;
    for (i, ch) in s.chars().enumerate() {
        if !ch.is_ascii() {
            continue;
        }
        for (row, bits) in crate::font8x8::FONT8X8_BASIC[usize::from(ch as u8)]
            .iter()
            .enumerate()
        {
            let py = y + row;
            if py >= S {
                continue;
            }
            for col in 0..w {
                if bits & (1 << col) != 0 {
                    let x = x0 + i * w + col;
                    if x < S {
                        put(px, x, py, color.0, color.1, color.2);
                    }
                }
            }
        }
    }
}

/// 负载 → 动图播放倍率。
///
/// Catime 给的是一张用户可改的 0-100 % → 倍率曲线（`ANIMATION_SPEED_MAP_10..100`，
/// 128 点容量，线性插值）。我们先用固定的两段直线：半载以下不干预，半载到满载之间
/// 线性掉到 1/4 速。理由是本项目的配置面已经够大，而"曲线编辑器"需要键盘。
pub(super) fn throttle_speed(percent: u8) -> f64 {
    if percent <= 50 {
        return 1.0;
    }
    // 50%→1.0，100%→0.25，中间线性；再往上夹住。下限是 1/4 速而不是 0——
    // 满载也不该把动画冻住，那看起来像程序卡了
    (1.0 - (percent as f64 - 50.0) * 0.015).clamp(0.25, 1.0)
}

/// 这一拍动图该按几倍速走。抽成函数是为了能测——不然"看哪个指标"这条分支只能靠
/// 把机器压到 50% 以上才验证得到。
pub(super) fn play_speed(throttle: Throttle, src: &sysinfo::Sources) -> f64 {
    match throttle {
        Throttle::Off => 1.0,
        Throttle::Cpu => throttle_speed(src.cpu),
        Throttle::Memory => throttle_speed(src.mem),
    }
}

/// 按 `mode` 生成当前该显示的图标。
pub(super) fn icon_pixmap(
    mode: IconMode,
    src: &sysinfo::Sources,
    now: (u32, u32, u32),
    player: &mut gif::Player,
    numbers: bool,
) -> (i32, i32, Vec<u8>) {
    match mode {
        IconMode::Clock => clock_pixmap(now),
        IconMode::Cpu if numbers => percent_pixmap(Some(src.cpu), false),
        IconMode::Memory if numbers => percent_pixmap(Some(src.mem), false),
        IconMode::Battery if numbers => percent_pixmap(
            src.battery.map(|b| b.percent),
            src.battery.is_some_and(|b| b.charging),
        ),
        IconMode::Network if numbers => net_pixmap(src.net_down, src.net_up),
        IconMode::Cpu => gauge_pixmap(Some(src.cpu), false),
        IconMode::Memory => gauge_pixmap(Some(src.mem), false),
        IconMode::Battery => gauge_pixmap(
            src.battery.map(|b| b.percent),
            src.battery.is_some_and(|b| b.charging),
        ),
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
        assert!(
            counts.windows(2).all(|w| w[0] < w[1]),
            "水位应随占用率单调上升: {counts:?}"
        );
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
        assert!(
            fmt_rate(u64::MAX).ends_with("GB/s"),
            "封顶在 GB/s，不外推到 TB"
        );
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
        assert_eq!(
            tooltip_text(&src),
            "CPU 12% · 内存 44% · ↓ 1.0 MB/s ↑ 2048 B/s"
        );
        let with_batt = sysinfo::Sources {
            battery: Some(sysinfo::Battery {
                percent: 87,
                charging: true,
            }),
            ..src
        };
        assert!(
            tooltip_text(&with_batt).ends_with("· 电池 87%⚡"),
            "充电标记该在末尾"
        );
    }

    /// 网络水位是对数刻度：静默归零、区间内分得开、封顶不溢出。
    #[test]
    fn net_level_is_log_scaled_and_bounded() {
        assert_eq!(net_level(0, 0), 0, "静默该是空盘");
        assert_eq!(net_level(900, 12), 0, "低于 1 KB/s 的零星心跳算静默");
        let small = net_level(64 * 1024, 0);
        let big = net_level(2 * 1024 * 1024, 0);
        assert!(
            0 < small && small < big && big < 100,
            "刻度要分得开: {small} {big}"
        );
        assert_eq!(net_level(10 * 1024 * 1024, 0), 100);
        assert_eq!(net_level(u64::MAX, 0), 100, "超量要夹紧而不是回绕");
        // 上下行取大：只有上传在跑也该看得见
        assert_eq!(net_level(0, 2 * 1024 * 1024), big);
    }

    /// 紧凑速率最多四个字符——图标里连 `D`/`U` 前缀一共五格，压扁后正好 30 px。
    #[test]
    fn rate_short_never_exceeds_four_chars() {
        assert_eq!(rate_short(0), "0B");
        assert_eq!(rate_short(983), "983B");
        assert_eq!(rate_short(340_000), "332K");
        assert_eq!(rate_short(1_200_000), "1.1M");
        // 边界与荒谬值都要夹紧（计数器被重置时算得出天文数字的差值）
        for v in [1u64, 1023, 1024, 999 * 1024, 1 << 30, u64::MAX] {
            let s = rate_short(v);
            assert!(s.chars().count() <= 4, "{v} 写成了 {s}");
        }
    }

    /// 数字档要真的把字画进底里，而且不许越界：`put` 不做边界检查，写出去就是踩内存。
    #[test]
    fn percent_digits_add_ink_without_leaking_outside() {
        let (_, _, hollow) = percent_pixmap(None, false);
        let (_, _, some) = percent_pixmap(Some(42), false);
        let (_, _, full) = percent_pixmap(Some(100), false);
        // `filled` 排除深色盘面与白环，但方块那圈灰边算内容，所以比的是增量
        let (h, s, f) = (filled(&hollow), filled(&some), filled(&full));
        assert!(s > h, "数字要添墨: {h} -> {s}");
        assert!(f > s, "多一位数字要多一些墨: {s} -> {f}");
        assert!(f - h < S * S / 3, "字不该把底涂满: 多了 {}", f - h);
        // 四角是被削掉的，字越界就会把这些位置涂脏
        for (x, y) in [(0, 0), (S - 1, 0), (0, S - 1), (S - 1, S - 1)] {
            assert_eq!(full[(y * S + x) * 4], 0, "角上该是空的");
        }
        // 充电一律绿，与水位档同一条规则：9% 平时是绿，充电时更不该变红
        let (_, _, charging) = percent_pixmap(Some(9), true);
        let green = |px: &[u8], i: usize| px[i + 2] > 150 && px[i + 1] < 150;
        assert!(
            (0..S * S).any(|k| green(&charging, k * 4)),
            "充电时该出现绿色，而不是 9% 的红"
        );
    }

    /// 选哪个指标决定倍率：`Off` 恒 1.0，其余两档各看各的那一路。
    #[test]
    fn play_speed_follows_the_chosen_metric() {
        let src = sysinfo::Sources {
            cpu: 90,
            mem: 10,
            net_down: 0,
            net_up: 0,
            battery: None,
        };
        assert_eq!(play_speed(Throttle::Off, &src), 1.0, "关掉限速就该原速");
        assert!(play_speed(Throttle::Cpu, &src) < 0.5, "CPU 90% 该慢下来");
        assert_eq!(play_speed(Throttle::Memory, &src), 1.0, "内存 10% 不该动");
    }

    /// 限速曲线：半载以下完全不干预，之后线性掉到 1/4 速，且不许掉成负数。
    #[test]
    fn throttle_curve_is_flat_then_linear() {
        assert_eq!(throttle_speed(0), 1.0);
        assert_eq!(throttle_speed(50), 1.0, "半载以下不该动画面");
        assert!(throttle_speed(75) < throttle_speed(60), "越忙越慢");
        assert!((throttle_speed(100) - 0.25).abs() < 1e-9, "满载 1/4 速");
        // 采样给的是 u8，理论到 100 封顶，但曲线本身也不许给出负倍率
        assert!(throttle_speed(u8::MAX) > 0.0);
    }

    /// 网络的两行按颜色分开数：绿行与琥珀行不许重叠，也不许贴着上下边。
    #[test]
    fn net_number_rows_are_two_separate_bands() {
        let (_, _, px) = net_pixmap(1_200_000, 340_000);
        let at = |x: usize, y: usize| {
            let i = (y * S + x) * 4;
            (px[i + 1], px[i + 2], px[i + 3])
        };
        let band = |pred: fn((u8, u8, u8)) -> bool| {
            let rows: Vec<usize> = (0..S).filter(|y| (0..S).any(|x| pred(at(x, *y)))).collect();
            (rows.first().copied(), rows.last().copied())
        };
        let down = band(|(r, g, _)| g > 150 && r < 150);
        let up = band(|(r, g, b)| r > 200 && g > 150 && b < 120);
        assert!(
            down.0.is_some() && up.0.is_some(),
            "两行都该有字: {down:?} {up:?}"
        );
        assert!(down.1 < up.0, "上下两行不许重叠: {down:?} {up:?}");
        assert!(
            down.0.unwrap() >= 4 && up.1.unwrap() < S - 4,
            "不许贴着上下边"
        );
    }
}
