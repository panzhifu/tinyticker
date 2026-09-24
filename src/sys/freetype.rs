//! libfreetype 的 C ABI 声明层。
//!
//! 只用运行时 `dlopen` 取符号，不引任何 crate，也不带来构建期的 `-dev` 依赖——
//! 与 [`crate::sys::wayland`] / [`crate::sys::x11`] / [`crate::sys::dbus`] 同一手法。
//! 注意这里**不写 `extern "C" {}` 声明块**：那会让链接器去找 `-lfreetype`，
//! 而本项目根本没有链它。所有入口都是从 [`crate::sys::Lib::func`] 拿到的函数指针。
//!
//! # 关于结构体偏移
//!
//! 这里不重述 `FT_FaceRec` / `FT_GlyphSlotRec` 的完整字段表，只按**探测出来的字节偏移**
//! 读需要的那几个字段（见下面的 `OFF_*`）。两个理由：
//!
//! 1. 中间那些本模块用不到的字段（`FT_Generic`、`FT_BBox`、`FT_Bitmap_Size*`、
//!    `FT_Outline`、`FT_SubGlyph`…）一旦镜像错一个类型，错位会静默传染到后面所有字段；
//!    只按已知偏移读就没有这个面。
//! 2. 偏移是拿 `cc` 对着 `freetype.h` 打 `offsetof` 得到的，不是抄的。FreeType 自 2.x
//!    起维持公开结构体的二进制兼容（`glyph_index` 那格在 2.10 前是同名同宽的 reserved，
//!    所以偏移没动），但我们不拿这个承诺保命。
//!
//! 万一偏移对不上，最坏结果是读出一组荒唐的 `rows`/`pitch`/`pixel_mode`，被 [`FreeType::ink`]
//! 里的守卫判为不可用并退回内置位图——不会去解引坏指针。

use std::ffi::{CStr, CString, c_char, c_void};

/// 探测来源：headers = FreeType 2.14.3，LP64。
/// `FT_Pos` / `FT_Long` / `FT_Fixed` 都是 8 字节。
pub type FtPos = i64;
/// `FT_Int` / `FT_Error`。
pub type FtError = i32;
/// `FT_UInt`。
pub type FtUInt = u32;
/// `FT_ULong`（码位就按它传）。
pub type FtULong = u64;

// —— FT_FaceRec（sizeof == 248）———————————————————————————————
const FACE_FACE_FLAGS: usize = 16;
const FACE_FAMILY_NAME: usize = 40;
const FACE_UNITS_PER_EM: usize = 136;
const FACE_ASCENDER: usize = 138;
const FACE_GLYPH: usize = 152;

const FACE_FLAG_SCALABLE: FtPos = 0x01;

// —— FT_GlyphSlotRec（sizeof == 304）——————————————————————————
/// `FT_Vector advance` @128，取它的 `.x`。26.6 定点（2.13 起对缩放字体也是 26.6）。
const SLOT_ADVANCE_X: usize = 128;
/// `FT_Bitmap bitmap` @152，内部按 `FT_Bitmap`（sizeof 40）展开：
/// rows 0 / width 4 / pitch 8 / buffer 16 / num_grays 24 / pixel_mode 26 / palette 32。
const SLOT_ROWS: usize = 152;
const SLOT_WIDTH: usize = 156;
const SLOT_PITCH: usize = 160;
const SLOT_BUFFER: usize = 168;
const SLOT_PIXEL_MODE: usize = 178;
const SLOT_LEFT: usize = 192;
const SLOT_TOP: usize = 196;

/// `FT_PIXEL_MODE_GRAY`：每像素一个 0-255 的覆盖度字节。
const PIXEL_MODE_GRAY: u8 = 2;

/// `FT_ENCODING_UNICODE`，即 `FT_TAG('u','n','i','c')`。
const ENCODING_UNICODE: FtULong = 0x756E_6963;

/// `FT_LOAD_RENDER`。
const LOAD_RENDER: FtError = 0x4;

/// 接受的字形位图边长上限。CJK 在 12-60 px 之间远不到它的一半，
/// 超出只可能是偏移错位，一律拒。
const MAX_DIM: u32 = 192;

/// 一个 `.ttc` 里最多试几个子字面。Noto Sans CJK 是 5（SC/TC/JP/KR/HK）。
const MAX_SUBFACES: i64 = 8;

type FnInit = unsafe extern "C" fn(*mut *mut c_void) -> FtError;
type FnNewFace =
    unsafe extern "C" fn(*mut c_void, *const c_char, FtPos, *mut *mut c_void) -> FtError;
type FnSelectCharmap = unsafe extern "C" fn(*mut c_void, FtULong) -> FtError;
type FnSetPixelSizes = unsafe extern "C" fn(*mut c_void, FtUInt, FtUInt) -> FtError;
type FnLoadGlyph = unsafe extern "C" fn(*mut c_void, FtUInt, FtError) -> FtError;
type FnGetCharIndex = unsafe extern "C" fn(*mut c_void, FtULong) -> FtUInt;

/// 一次栅格的产物。像素是**拷出来的**：freetype 的槽位缓冲下一次 `FT_Load_Glyph`
/// 就复用了，不能把它当 `'static` 往外发。
#[derive(Clone)]
pub struct Ink {
    pub w: u32,
    pub h: u32,
    /// 字形左边界相对当前笔位的偏移（像素）。
    pub left: i32,
    /// 基线到字形顶的距离（像素）。
    pub top: i32,
    /// 水平推进量，已按 26.6 定点换算并向上取整（0 宽的标点也留 1 px，免得笔位不动）。
    pub advance: u32,
    /// `w * h` 行主序覆盖度，值 0-255。
    pub gray: Vec<u8>,
}

/// 已打开的库 + 选好的字面。任一步失败都不构造它，上层据此退回内置位图。
pub struct FreeType {
    face: *mut c_void,
    set_sizes: FnSetPixelSizes,
    load_glyph: FnLoadGlyph,
    char_index: FnGetCharIndex,
    /// 字体自报的家族名，只用于日志与子字面挑选。
    pub family: String,
    units_per_em: u32,
    /// 字体自带的正 ascender（em 单位）。0 表示不可信。
    ascender: i32,
}

impl FreeType {
    /// 在 `path` 上挑一个最合适中文字形的字面。`.ttc` 会挑名字里带 `SC` 的那个，
    /// 否则同一码位拿到的是日文写法（「直」「骨」「关」的点画差异）。
    pub fn open(path: &str) -> Option<Self> {
        let lib = crate::sys::Lib::open("libfreetype.so.6")?;
        let init: FnInit = lib.func("FT_Init_FreeType")?;
        let new_face: FnNewFace = lib.func("FT_New_Face")?;
        let set_sizes: FnSetPixelSizes = lib.func("FT_Set_Pixel_Sizes")?;
        let load_glyph: FnLoadGlyph = lib.func("FT_Load_Glyph")?;
        let char_index: FnGetCharIndex = lib.func("FT_Get_Char_Index")?;
        // 老 libtool 版本理论上可能不导这个符号；选不上就沿用字体默认的 charmap。
        let select: Option<FnSelectCharmap> = lib.func("FT_Select_Charmap");

        let mut library = std::ptr::null_mut::<c_void>();
        // SAFETY: 函数指针由 dlsym 从本模块签名常量所指的确切符号取得，
        // 非空即存在；参数是 freetype 拥有的合法对象。
        if unsafe { init(&mut library) } != 0 || library.is_null() {
            return None;
        }
        let cname = CString::new(path).ok()?;

        let mut best: Option<(u8, Self)> = None;
        for index in 0..MAX_SUBFACES {
            let mut face = std::ptr::null_mut::<c_void>();
            // SAFETY: 同上；`cname` 在本次调用内活着，`library` 已由 init 初始化。
            if unsafe { new_face(library, cname.as_ptr(), index, &mut face) } != 0 || face.is_null()
            {
                // 非集合字体在 index 1 就该失败，循环自然收尾。
                break;
            }
            if let Some(ft) = adopt(face, set_sizes, load_glyph, char_index, select) {
                let score = score_family(&ft.family);
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, ft));
                }
                // 已经拿到明确的大陆简体（或它就是唯一字面），不必把集合全开一遍。
                if score >= 3 {
                    break;
                }
            }
        }
        // 故意不 dlclose：库映射活到进程结束，关掉了上面的函数指针就全废了。
        best.map(|(_, ft)| ft)
    }

    /// `ch` 在本字面里的字形 id；`0` 是这个字体画不出这个码位。
    #[must_use]
    pub fn has(&self, ch: char) -> bool {
        // SAFETY: 指针来自 dlsym，`self.face` 由 FT_New_Face 产出且我们不释放它。
        unsafe { (self.char_index)(self.face, ch as FtULong) != 0 }
    }

    /// 以 `px` 像素高光栅化 `ch`。任何一步不对都返回 `None`，由调用方退回位图。
    pub fn ink(&self, ch: char, px: u32) -> Option<Ink> {
        // SAFETY: 同 has()。本方法只在主线程的绘制路径被调用，不会与自身竞争。
        let face = self.face;
        let glyph = unsafe { (self.char_index)(face, ch as FtULong) };
        if glyph == 0 {
            return None;
        }
        if unsafe { (self.set_sizes)(face, 0, px.max(1)) } != 0 {
            return None;
        }
        if unsafe { (self.load_glyph)(face, glyph, LOAD_RENDER) } != 0 {
            return None;
        }
        let slot = read_ptr(self.face, FACE_GLYPH)?;
        let rows = read_u32(slot, SLOT_ROWS);
        let width = read_u32(slot, SLOT_WIDTH);
        let pitch = read_i32(slot, SLOT_PITCH);
        let mode = read_u8(slot, SLOT_PIXEL_MODE);
        let buffer = read_ptr(slot, SLOT_BUFFER)?;

        // —— 守卫：偏移错位时这几项几乎必然荒唐，先判死再碰那块内存 ——
        if rows == 0 || width == 0 || rows > MAX_DIM || width > MAX_DIM || mode != PIXEL_MODE_GRAY {
            return None;
        }
        let stride = pitch.unsigned_abs();
        if stride < width {
            return None;
        }
        // 负 pitch 是自底向上，首行在 buffer + (rows-1)*|pitch|。
        let span = (rows - 1) as i64 * pitch as i64 + width as i64;
        if span <= 0 || u32::try_from(span).ok()? > MAX_DIM * MAX_DIM {
            return None;
        }
        let base = buffer as *const u8;
        let len = (rows * stride) as usize;
        let gray = unsafe {
            let mut out = vec![0u8; (rows * width) as usize];
            for r in 0..rows as usize {
                let src = if pitch < 0 {
                    base.add((rows as usize - 1 - r) * stride as usize)
                } else {
                    base.add(r * stride as usize)
                };
                let dst = out.as_mut_ptr().add(r * width as usize);
                std::ptr::copy_nonoverlapping(src, dst, width as usize);
            }
            // `len` 只用来提醒读者：我们刻意只拷每行的前 width 字节，丢掉行尾填充。
            debug_assert!(len >= out.len());
            out
        };
        // advance 是 26.6 定点；向上取整保证笔位一定前进。
        let advance = (read_i64(slot, SLOT_ADVANCE_X).max(0) + 63) / 64;
        Some(Ink {
            w: width,
            h: rows,
            left: read_i32(slot, SLOT_LEFT),
            top: read_i32(slot, SLOT_TOP),
            advance: u32::try_from(advance).unwrap_or(1).max(1),
            gray,
        })
    }

    /// em 单位 → 像素的 ascender，用于给混排行定基线。字体没报就按 `px` 的 0.8 估。
    #[must_use]
    pub fn ascent_px(&self, px: u32) -> i32 {
        if self.units_per_em == 0 || self.ascender <= 0 {
            return (px as i32 * 4 / 5).max(1);
        }
        (self.ascender as i64 * px as i64 / self.units_per_em as i64) as i32
    }
}

/// 字面已 `FT_New_Face` 成功，检查它能不能用并包起来。
fn adopt(
    face: *mut c_void,
    set_sizes: FnSetPixelSizes,
    load_glyph: FnLoadGlyph,
    char_index: FnGetCharIndex,
    select: Option<FnSelectCharmap>,
) -> Option<FreeType> {
    let flags = read_i64(face, FACE_FACE_FLAGS);
    // 只吃轮廓字体：非 scalable 的位图字体在 FT_LOAD_RENDER 下不带缩放会失败，
    // 而我们已经自带 8x8 点阵，不需要第二套。
    // 注意 FIXED_SIZES（另附位码点）**不是**排除理由——不少 CJK 字体轮廓与位码点都有，
    // 我们走缩放轮廓那条路照样能用。
    if flags & FACE_FLAG_SCALABLE == 0 {
        return None;
    }
    if let Some(select) = select {
        // 选不上也不致命：多数字体 charmap[0] 就是 Unicode。
        // SAFETY: 指针来自 dlsym，face 由 FT_New_Face 产出。
        unsafe { select(face, ENCODING_UNICODE) };
    }
    if unsafe { set_sizes(face, 0, 16) } != 0 {
        return None;
    }
    let family = match read_ptr(face, FACE_FAMILY_NAME) {
        Some(p) => unsafe { CStr::from_ptr(p as *const c_char) }
            .to_string_lossy()
            .into_owned(),
        None => String::new(),
    };
    Some(FreeType {
        face,
        set_sizes,
        load_glyph,
        char_index,
        units_per_em: read_u32(face, FACE_UNITS_PER_EM),
        ascender: read_i16(face, FACE_ASCENDER) as i32,
        family,
    })
}

/// 子字面偏好分：越大越合中文意。`>= 3` 视为已经够好，停止扫描。
fn score_family(family: &str) -> u8 {
    let f = family.to_ascii_uppercase();
    if f.contains("CJK SC")
        || f.contains("HAN SIMPLIFIED")
        || f.contains("HEI")
        || f.contains("SONG")
    {
        4
    } else if f.contains("CJK") || f.contains("HAN") {
        // 是 CJK 集合里的一份，但没标明地区（TC/JP/KR/HK 都会落到这里）——能用，不优先。
        2
    } else {
        // 非 CJK 字体一般是单字面，第一个就是它。
        3
    }
}

// —— 偏移读取：一律未对齐读，避免依赖 Rust 结构体的填充规则 ————————

fn read_u8(base: *mut c_void, off: usize) -> u8 {
    unsafe { (base as *const u8).add(off).read_unaligned() }
}

fn read_i16(base: *mut c_void, off: usize) -> i16 {
    unsafe { (base as *const u8).add(off).cast::<i16>().read_unaligned() }
}

fn read_u32(base: *mut c_void, off: usize) -> u32 {
    unsafe { (base as *const u8).add(off).cast::<u32>().read_unaligned() }
}

fn read_i32(base: *mut c_void, off: usize) -> i32 {
    unsafe { (base as *const u8).add(off).cast::<i32>().read_unaligned() }
}

fn read_i64(base: *mut c_void, off: usize) -> FtPos {
    unsafe {
        (base as *const u8)
            .add(off)
            .cast::<FtPos>()
            .read_unaligned()
    }
}

fn read_ptr(base: *mut c_void, off: usize) -> Option<*mut c_void> {
    let p = unsafe {
        (base as *const u8)
            .add(off)
            .cast::<*mut c_void>()
            .read_unaligned()
    };
    (!p.is_null()).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subface_scoring_prefers_simplified() {
        assert!(score_family("Noto Sans CJK SC") > score_family("Noto Sans CJK JP"));
        assert!(score_family("Noto Sans CJK SC") > score_family("Noto Sans CJK TC"));
        assert_eq!(
            score_family("Noto Sans CJK JP"),
            score_family("Noto Sans CJK KR")
        );
        // 单字面的拉丁字体应该一发就停
        assert!(score_family("Terminess Nerd Font") >= 3);
    }

    /// 偏移表是从 C 探测来的，这里守一道：字段之间不许重叠、顺序不许倒。
    #[test]
    fn probed_offsets_are_ordered_and_disjoint() {
        let slot = [
            SLOT_ADVANCE_X,
            SLOT_ROWS,
            SLOT_WIDTH,
            SLOT_PITCH,
            SLOT_BUFFER,
            SLOT_PIXEL_MODE,
            SLOT_LEFT,
            SLOT_TOP,
        ];
        let mut prev = 0;
        for o in slot {
            assert!(o > prev, "槽位字段顺序倒了：{o} 不在 {prev} 之后");
            prev = o;
        }
        // FT_Bitmap 在槽里占 40 字节，bitmap_left 紧跟其后。
        assert_eq!(SLOT_LEFT, SLOT_ROWS + 40);
        let face = [
            FACE_FACE_FLAGS,
            FACE_FAMILY_NAME,
            FACE_UNITS_PER_EM,
            FACE_ASCENDER,
            FACE_GLYPH,
        ];
        for w in face.windows(2) {
            assert!(w[1] > w[0]);
        }
    }

    /// 没有 freetype 或没有字体时不能 panic——上层要能安静退回位图。
    #[test]
    fn opening_a_missing_font_is_none_not_panic() {
        assert!(FreeType::open("/nonexistent/no-such-font.ttf").is_none());
    }
}
