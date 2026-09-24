//! 字形层：内置 8x8 位图**优先**，位图覆盖不到的码位才交给运行期 dlopen 的 libfreetype。
//!
//! # 为什么是"位图优先"而不是"整体换成 TTF"
//!
//! 数字行是这个挂件的主体，也是它观感的来源：8x8 点阵按整数倍放大后是锐利的像素风，
//! 每个特效的模糊半径与位移量都是照这个尺寸标定的（见 [`crate::effect`] 的模块注释）。
//! 整行换成轮廓字体会把这套标定的前提抽掉，还会让纯 ASCII 用户的画面无故改变。
//!
//! 所以规则是一条直线：**能查表就查表，查不到才去问 freetype。**
//! 全 ASCII 的一帧里 freetype 一次都不会被调用，输出与引入本模块之前逐字节相同。
//!
//! # 线程
//!
//! 字形栅格只发生在主线程的绘制路径里（`Widget::build_frame` → `Frame::paint`）；
//! 托盘线程只画 32x32 的程序化图标，不碰本模块。[`Shared`] 的 `unsafe impl Sync`
//! 担保的就是这件事。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::config::{TextFont, expand_tilde};
use crate::font8x8::FONT8X8_BASIC;
use crate::sys::freetype::{FreeType, Ink};

/// TTF 字形的像素高 = `12 × cell`。比 `8 × cell` 的点阵盒高出一截，多出的部分向上长进
/// 两行之间那条 `4 × cell` 的缝隙，所以布局不必为它改算法。
const TTF_PX_PER_CELL: u32 = 12;

/// 缓存里最多留多少个 (码位, 像素高) 组合。状态行实际用到的量级是几十个，
/// 这个上限只是防一手 pathological 的长外部文本。
const CACHE_CAP: usize = 2048;

/// 包一层 `Sync`。
struct Shared(FreeType);
// SAFETY: 本模块所有入口都只在主线程的绘制路径上调用（见模块注释）。`FreeType` 内部
// 是裸指针 + 函数指针，既非 Send 也非 Sync，这两条 impl 担保的是我们自己的用法，
// 不是 freetype 线程安全。
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

/// `None` = 试过且没有可用字体。记下来就不会每帧重试一遍目录扫描。
static FACE: OnceLock<Option<Shared>> = OnceLock::new();

/// (码位, 像素高) → 栅格结果；值是 `Option<Ink>`，`None` 是"这个字体画不出这个码位"的负缓存。
type GlyphCache = Mutex<HashMap<(u32, u32), Option<Ink>>>;

static CACHE: OnceLock<GlyphCache> = OnceLock::new();

/// 按策略打开字体后端。只在启动时调一次（`OnceLock`，重复调用只生效一次）。
///
/// 返回一句要转达给用户的警告；没话说就返回 `None`。
pub fn init(policy: &TextFont) -> Option<String> {
    let mut warning: Option<String> = None;
    FACE.get_or_init(|| {
        let face = match policy {
            TextFont::Off => None,
            TextFont::Auto => discover().and_then(|p| FreeType::open(&p)),
            TextFont::Path(p) => {
                let expanded = expand_tilde(p);
                match FreeType::open(&expanded.to_string_lossy()) {
                    Some(ft) => Some(ft),
                    None => {
                        warning = Some(format!("⚠️ text_font = {p} 打不开，改用自动发现的字体"));
                        discover().and_then(|q| FreeType::open(&q))
                    }
                }
            }
        };
        match (face, policy) {
            (None, TextFont::Auto) => None,
            // 明确要了字体却没有后端：说清楚，免得当成"支持中文"来用。
            (None, TextFont::Path(p)) => {
                if warning.is_none() {
                    warning = Some(format!(
                        "⚠️ text_font = {p} 与自动发现都不可用，非 ASCII 将留空位"
                    ));
                }
                None
            }
            (None, TextFont::Off) => None,
            (Some(ft), _) => Some(Shared(ft)),
        }
    });
    warning
}

// —— 字体发现 ——————————————————————————————————————————————

/// 各发行版放 CJK 的位置差得比想象中大，先试一批绝对路径（覆盖 Arch / Debian /
/// Fedora 的常见包名），不中再扫目录。顺序即偏好：无衬线优先、SC 优先。
const KNOWN: &[&str] = &[
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/adobe-source-han-sans/SourceHanSansSC-Regular.otf",
    "/usr/share/fonts/source-han-sans/SourceHanSansSC-Regular.otf",
    "/usr/share/fonts/wenquanyi/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/TTF/LXGWWenKai-Regular.ttf",
    "/usr/share/fonts/truetype/lxgw-wenkai/LXGWWenKai-Regular.ttf",
];

/// 要扫的字体根目录。`$XDG_DATA_HOME` 缺失或非绝对路径时退回 `~/.local/share`。
fn font_roots() -> Vec<PathBuf> {
    let mut v = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        && !data.is_empty()
    {
        let p = PathBuf::from(&data);
        if p.is_absolute() {
            v.push(p.join("fonts"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let h = PathBuf::from(home);
        v.push(h.join(".local/share/fonts"));
        v.push(h.join(".fonts"));
    }
    v
}

/// 文件名偏好分：越大越适合当状态行的中文体，0 = 不收。
fn font_rank(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    if !(n.ends_with(".ttf") || n.ends_with(".otf") || n.ends_with(".ttc") || n.ends_with(".otc")) {
        return 0;
    }
    let cjk = n.contains("cjk")
        || n.contains("han")
        || n.contains("wqy")
        || n.contains("wenquanyi")
        || n.contains("lxgw")
        || n.contains("hei");
    if cjk {
        // 衬线与只含日/韩/繁字形的分区，同一码位写法不同：能用，但排后面。
        if n.contains("serif")
            || n.contains("mincho")
            || n.contains("-jp")
            || n.contains("-kr")
            || n.contains("-tc")
            || n.contains("hk")
        {
            return 2;
        }
        return 4;
    }
    // 非 CJK 的拉丁体也收：它补的是 Latin-1、符号这些点阵里没有的码位。
    1
}

/// 第一个能开出字面的字体路径：先查 [`KNOWN`]，再按名字偏好扫字体根。
fn discover() -> Option<String> {
    let found = KNOWN
        .iter()
        .copied()
        .find(|p| Path::new(p).is_file())
        .map(str::to_owned);
    if let Some(p) = found {
        // KNOWN 里放的都是按事实挑的主流路径，但仍要确认开得出字面，
        // 否则一个损坏的文件会把整条发现流程钉死。
        if FreeType::open(&p).is_some() {
            return Some(p);
        }
    }
    let mut candidates = collect_candidates();
    // 同分按路径字典序，保证多次启动选到同一个字体。
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    candidates
        .into_iter()
        .map(|(_, p)| p)
        .find(|p| FreeType::open(&p.to_string_lossy()).is_some())
        .map(|p| p.to_string_lossy().into_owned())
}

/// 递归收字体文件，带上偏好分。限深度、限总条目数，跳过点开头的目录。
fn collect_candidates() -> Vec<(u8, PathBuf)> {
    const MAX_DEPTH: usize = 4;
    const MAX_ENTRIES: usize = 20_000;
    let mut out = Vec::new();
    let mut budget = MAX_ENTRIES;
    for root in font_roots() {
        let mut stack = vec![(root, 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if depth >= MAX_DEPTH || budget == 0 {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = e.path();
                if path.is_dir() {
                    stack.push((path, depth + 1));
                } else {
                    let rank = font_rank(&name);
                    if rank > 0 {
                        out.push((rank, path));
                    }
                }
            }
        }
    }
    out
}

// —— 栅格 ————————————————————————————————————————————————

/// 一个已定位的字形。坐标是设备像素、绝对值（相对传入本模块的 `top`）。
pub struct Glyph {
    pub x: i32,
    pub y: i32,
    pub body: GlyphBody,
}

/// 两种栅格形态：点阵按整数放大，TTF 逐像素覆盖度。
pub enum GlyphBody {
    /// 8x8 点阵，按 `cell` 整数放大；bit0 是最左列。
    Bits { bits: [u8; 8], cell: u32 },
    /// `w × h` 行主序的 0-255 覆盖度，逐像素 1:1；`ascent` 是基线到字形顶的距离。
    Gray {
        gray: Vec<u8>,
        w: u32,
        h: u32,
        ascent: i32,
    },
}

/// 一行的栅格结果。
pub struct Run {
    pub glyphs: Vec<Glyph>,
    /// 水平推进总量，用于居中与命中框。
    pub width: u32,
    /// 实际包围盒的上下沿。CJK 会比点阵盒向上多出几像素，所以 `top` 可以小于传入值。
    pub top: i32,
    pub bottom: i32,
}

/// 把一行文本栅格化。`cell` 是点阵的整数放大倍数，`top` 是**点阵盒**上沿；
/// 基线取 `top + 8 × cell`，两种字形都按这条基线对齐。
#[must_use]
pub fn shape(text: &str, cell: u32, top: i32) -> Run {
    let cell = cell.max(1);
    let baseline = top + (8 * cell) as i32;
    // 初值就是点阵盒：没有字形时（空串、或整串都画不出）包围盒仍等于名义盒。
    let mut run = Run {
        glyphs: Vec::new(),
        width: 0,
        top,
        bottom: baseline,
    };
    let mut x = 0i32;
    for ch in text.chars() {
        match glyph(ch, cell) {
            Some((advance, body)) => {
                let gy = match &body {
                    GlyphBody::Bits { .. } => baseline - (8 * cell) as i32,
                    GlyphBody::Gray { ascent, .. } => baseline - *ascent,
                };
                let gh = match &body {
                    GlyphBody::Bits { .. } => (8 * cell) as i32,
                    GlyphBody::Gray { h, .. } => *h as i32,
                };
                run.top = run.top.min(gy);
                run.bottom = run.bottom.max(gy + gh);
                run.glyphs.push(Glyph { x, y: gy, body });
                x += advance as i32;
                run.width += advance;
            }
            // 两边都画不出：留一格点阵宽的空位，与引入本模块之前的行为一致。
            None => {
                x += (8 * cell) as i32;
                run.width += 8 * cell;
            }
        }
    }
    run
}

/// 截出放得进 `max_w` 像素的最长前缀。外部文本源可能很长，不截的话居中的起点
/// 会被压到 0、右边直接裁掉。
#[must_use]
pub fn fit(text: &str, cell: u32, max_w: u32) -> String {
    let cell = cell.max(1);
    let mut out = String::new();
    let mut used = 0u32;
    for ch in text.chars() {
        let a = advance_of(ch, cell);
        if used.saturating_add(a) > max_w {
            break;
        }
        used += a;
        out.push(ch);
    }
    out
}

/// 单个码位的推进量；画不出时按一格点阵宽算（与 `shape` 留空位的规则一致）。
fn advance_of(ch: char, cell: u32) -> u32 {
    match glyph(ch, cell) {
        Some((a, _)) => a,
        None => 8 * cell,
    }
}

/// 取一个字形：点阵优先，查不到才问 freetype。
fn glyph(ch: char, cell: u32) -> Option<(u32, GlyphBody)> {
    let code = ch as usize;
    // ① 内置点阵：ASCII 全走这里，不碰 freetype。
    if code < 128 {
        return Some((
            8 * cell,
            GlyphBody::Bits {
                bits: FONT8X8_BASIC[code],
                cell,
            },
        ));
    }
    // ② freetype。没有后端就到此为止，非 ASCII 照旧留空位。
    let ink = rasterize(ch, TTF_PX_PER_CELL * cell)?;
    Some((
        ink.advance.max(1),
        GlyphBody::Gray {
            gray: ink.gray,
            w: ink.w,
            h: ink.h,
            ascent: ink.top.max(0),
        },
    ))
}

/// 带缓存的栅格。`None` = 没有 TTF 后端，或这个字体画不出这个码位。
fn rasterize(ch: char, px: u32) -> Option<Ink> {
    let face = FACE.get_or_init(|| None).as_ref()?;
    let key = (ch as u32, px);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().ok()?;
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let ink = face.0.ink(ch, px);
    if ink.is_none() && cache.len() >= CACHE_CAP {
        cache.clear();
    }
    let out = ink.clone();
    cache.insert(key, ink);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯 ASCII 一行不该碰 freetype：宽度必须还是 `chars × 8 × cell`，
    /// 包围盒必须正好是那个点阵盒。这条是"默认画面不变"的守门测试。
    #[test]
    fn ascii_run_is_exactly_the_bitmap_metrics() {
        for cell in [1, 2, 3, 7] {
            let r = shape("12:34", cell, 0);
            assert_eq!(r.width, 5 * 8 * cell);
            assert_eq!((r.top, r.bottom), (0, (8 * cell) as i32));
            assert_eq!(r.glyphs.len(), 5);
            assert!(matches!(r.glyphs[0].body, GlyphBody::Bits { cell: c, .. } if c == cell));
        }
    }

    /// 两边都画不出的码位留一格空位，而不是零宽把后面的字挤过来。
    /// 用 U+0378（Unicode 未分配）而不是表情符号：后者在某些字体里真有字形，
    /// 那样这条测试就只在部分机器上成立。
    #[test]
    fn unknown_codepoint_leaves_a_hole() {
        let r = shape("a\u{0378}b", 2, 0);
        assert_eq!(r.width, 3 * 8 * 2);
        assert_eq!(r.glyphs.len(), 2, "画不出的那个不该产出一个字形");
        assert_eq!(r.glyphs[1].x, 2 * 8 * 2, "第三个字要排在空位之后");
    }

    #[test]
    fn fit_stops_at_the_width_budget() {
        // 一格点阵宽 = 8 × cell
        assert_eq!(
            fit("1234567890", 1, 40),
            "12345",
            "40px 放得下整五格，不该退让"
        );
        assert_eq!(
            fit("1234567890", 1, 39),
            "1234",
            "差一像素就该整字退让，不留半格"
        );
        assert_eq!(fit("1234", 1, 40), "1234", "放得下不该被截");
        assert_eq!(fit("", 1, 40), "");
        assert_eq!(fit("1234", 1, 0), "", "预算为零就不该留字");
        assert_eq!(fit("1234", 1, u32::MAX), "1234", "预算拉满不许溢出");
    }

    /// `cell` 传 0 会把推进量算成 0，`fit` 就会无限收字——守一道。
    #[test]
    fn zero_cell_is_treated_as_one() {
        assert_eq!(shape("abcd", 0, 0).width, 4 * 8);
        assert_eq!(fit("abcd", 0, 16), "ab");
    }

    #[test]
    fn font_rank_prefers_sans_cjk() {
        assert!(font_rank("NotoSansCJK-Regular.ttc") > font_rank("NotoSerifCJK-Regular.ttc"));
        assert!(font_rank("NotoSansCJK-Regular.ttc") > font_rank("NotoSansCJK-JP.ttc"));
        assert!(font_rank("wqy-microhei.ttc") >= 4);
        assert!(font_rank("TerminessNerdFont.ttf") > 0, "拉丁体也要能补符号");
        assert_eq!(font_rank("README.md"), 0);
    }

    /// 码位 ≥128 **绝不**从点阵表里取——那是这次改动真正的不变量：点阵只覆盖 ASCII，
    /// 高码位要么由 TTF 出一个灰度字形，要么整格留空。两种后端环境下都成立。
    #[test]
    fn high_codepoints_never_come_from_the_bitmap() {
        for ch in ['中', '文', '\u{00e9}', '\u{2460}'] {
            if let Some((_, body)) = glyph(ch, 2) {
                assert!(matches!(body, GlyphBody::Gray { .. }), "{ch} 竟然来自点阵");
            }
        }
        // U+0378 是 Unicode 明确未分配的码位，任何字体都不该有它：必须留一格空位
        let hole = shape("\u{0378}", 2, 0);
        assert!(hole.glyphs.is_empty());
        assert_eq!(hole.width, 8 * 2);
    }

    /// `fit` 与 `shape` 必须用同一套推进量：否则外部文本源那一行会"截过了却仍然画不下"
    /// 或者"明明放得下却被截掉"。这条与主机有没有字体无关，因为两边算的是同一个东西。
    #[test]
    fn fit_never_overflows_the_budget() {
        let mixed = "编译完成 55% 中 ab";
        for cell in [1u32, 2, 4] {
            for budget in [0u32, 7, 8, 23, 40, 100, 160, 4000] {
                let kept = fit(mixed, cell, budget);
                let used = shape(&kept, cell, 0).width;
                assert!(
                    used <= budget,
                    "cell={cell} 预算 {budget} 却排下了 {used}：{kept:?}"
                );
                // 剩下的第一个字符确实放不下了，才允许停在这里
                if let Some(next) = mixed.chars().nth(kept.chars().count()) {
                    let would_be = used + shape(&next.to_string(), cell, 0).width;
                    assert!(
                        would_be > budget,
                        "预算 {budget} 还能放下 {next:?}，不该截断"
                    );
                }
            }
        }
    }

    /// 字体根不该把不存在的目录塞进来当成一次必然失败的 read_dir。
    #[test]
    fn font_roots_are_absolute_and_deduped() {
        let roots = font_roots();
        assert!(!roots.is_empty());
        assert!(roots.iter().all(|p| p.is_absolute()));
        let mut seen = std::collections::HashSet::new();
        assert!(
            roots.iter().all(|p| seen.insert(p.clone())),
            "不该有重复的根"
        );
    }
}
