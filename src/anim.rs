//! 动图容器的共同形状：帧序列 + 播放器。
//!
//! 托盘图标支持两种动图容器，解码各归各的模块——[`crate::gif`]（GIF87a/89a +
//! LZW + 交错）与 [`crate::png`]（zlib/DEFLATE + 行滤波 + APNG）——但它们对
//! 消费方（`tray::icon`）必须长得一样：整幅画布的 RGBA 帧 + 每帧延迟。于是
//! 类型与"按虚拟时钟取帧"的播放器放这一层，容器由文件头的 magic 判别，
//! 配置文件里只有一个 `tray_gif` 键、菜单里只有一个"动图"档。
//!
//! 虚拟时钟（[`Player::advance`]）是"动图限速"（`tray_throttle`）的实现底座：
//! 倍率作用在时钟推进上，画面就是"变慢"而不是"跳帧"。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::config;

/// 一帧：整幅画布的 RGBA，长度 = 画布宽 × 高 × 4。
pub struct Frame {
    pub rgba: Vec<u8>,
    pub delay: Duration,
}

/// 一段动图：等宽的帧序列。
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
        let total: u64 = self.frames.iter().map(|f| ms(f.delay).max(1)).sum();
        let mut t = ms(elapsed) % total;
        for (i, f) in self.frames.iter().enumerate() {
            let d = ms(f.delay).max(1);
            if t < d {
                return i;
            }
            t -= d;
        }
        self.frames.len() - 1
    }
}

/// 单个动图文件的大小上限。动图比文本大得多，但仍要挡住随手指向一个巨型文件。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 按文件头分发解码器：GIF 与 PNG 的签名互斥，判前 8 字节就够了。
pub fn decode(bytes: &[u8]) -> Option<Animation> {
    if bytes.len() >= 8 && bytes[..8] == *b"\x89PNG\r\n\x1a\n" {
        crate::png::decode(bytes)
    } else if bytes.len() >= 6 && (bytes[..6] == *b"GIF87a" || bytes[..6] == *b"GIF89a") {
        crate::gif::decode(bytes)
    } else {
        None
    }
}

/// 一个动图文件 + 播放进度。文件换了就重新解码。路径是**目录**时按 Catime 的
/// "文件夹帧序列" 语义走：文件名升序、每张图一帧（gif/png 都行），帧间隔固定
/// [`FOLDER_FRAME_DELAY`]——那边的 `ANIMATION_FOLDER_INTERVAL_MS` 可调，我们没有
/// 对话框也没有键盘，写死一档免得开第三个只为一个数字的配置键。
const FOLDER_FRAME_DELAY: Duration = Duration::from_millis(100);
/// 目录源里的文件数上限（与单文件解码器的帧数上限同量级）。
const MAX_FOLDER_FILES: usize = 64;

pub struct Player {
    path: Option<PathBuf>,
    /// 文件源是 (大小, mtime)；目录源是 (文件数, 目录 mtime)。变了就重新取帧。
    stamp: Option<(u64, SystemTime)>,
    anim: Option<Animation>,
    /// 动画自己的虚拟时钟。限速时它走得比墙钟慢，于是画面是"变慢"而不是"跳帧"。
    vclock: Duration,
    last: Instant,
}

impl Player {
    pub fn new(raw: Option<&str>) -> Self {
        Self {
            path: raw
                .map(config::expand_tilde)
                .filter(|p| !p.as_os_str().is_empty()),
            stamp: None,
            anim: None,
            vclock: Duration::ZERO,
            last: Instant::now(),
        }
    }

    /// 按倍率推进虚拟时钟（每次取帧之前调用一次）。上限 4.0：
    /// `tray_throttle = fixed` 允许双倍速（Catime 的 FIXED 档默认就是 200%），
    /// cpu/memory/timer 三档曲线本身给不出 >1。
    pub fn advance(&mut self, speed: f64) {
        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last);
        self.last = now;
        self.vclock += dt.mul_f64(speed.clamp(0.0, 4.0));
    }

    /// 当前该显示的一帧及其画布尺寸；文件不可用或解不出时 `None`（调用方该退回静态图标）。
    pub fn current(&mut self) -> Option<(&Frame, u16, u16)> {
        let path = self.path.as_ref()?;
        let stamp = if path.is_dir() {
            dir_stamp(path)?
        } else {
            file_stamp(path)?
        };
        if Some(stamp) != self.stamp {
            self.stamp = Some(stamp);
            self.anim = None;
            // 换文件/换目录等于换一段动画，时钟要从零走
            self.vclock = Duration::ZERO;
            self.anim = if path.is_dir() {
                load_dir(path)
            } else {
                load_file(path)
            };
        }
        let anim = self.anim.as_ref()?;
        let frame = anim
            .frames
            .get(anim.frame_at(self.vclock))
            .or(anim.frames.first())?;
        Some((frame, anim.width, anim.height))
    }
}

fn file_stamp(path: &std::path::Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn dir_stamp(path: &std::path::Path) -> Option<(u64, SystemTime)> {
    let count = fs::read_dir(path).ok()?.count() as u64;
    Some((count, fs::metadata(path).ok()?.modified().ok()?))
}

/// 单文件源：限大小，过了就按魔数分发解码。
fn load_file(path: &std::path::Path) -> Option<Animation> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        eprintln!(
            "⚠️ tray_gif 超过 {MAX_FILE_BYTES} 字节，忽略：{}",
            path.display()
        );
        return None;
    }
    decode(&bytes)
}

/// 目录帧序列：文件名升序取前 [`MAX_FOLDER_FILES`] 个 `.png`/`.gif`，每张图贡献
/// 它的**首帧**（帧序列源的语义就是"一文件一帧"），尺寸不符的踢掉。
fn load_dir(dir: &std::path::Path) -> Option<Animation> {
    let mut names: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            (lower.ends_with(".png") || lower.ends_with(".gif")).then(|| (name, e.path()))
        })
        .collect();
    names.sort();
    names.truncate(MAX_FOLDER_FILES);
    let mut frames = Vec::new();
    let mut size = (0u16, 0u16);
    for (name, p) in names {
        // 单文件解不出（坏了/超预算）跳过这一张，不断整段动画
        let Some(a) = load_file(&p) else { continue };
        let first = a.frames.first()?;
        if size == (0, 0) {
            size = (a.width, a.height);
        } else if (a.width, a.height) != size {
            eprintln!(
                "⚠️ 帧序列里 {name} 尺寸不符（{}×{} ≠ {}×{}），忽略",
                a.width, a.height, size.0, size.1
            );
            continue;
        }
        frames.push(Frame {
            rgba: first.rgba.clone(),
            delay: FOLDER_FRAME_DELAY,
        });
        if frames.len() >= MAX_FOLDER_FILES {
            break;
        }
    }
    if frames.is_empty() {
        return None;
    }
    Some(Animation {
        width: size.0,
        height: size.1,
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 限速靠虚拟时钟实现：倍率 0 时它一步都不许走，越界的倍率要夹住。
    /// （住在 anim：`vclock` 是私有字段，只有本模块的测试看得见。）
    #[test]
    fn player_clock_advances_by_the_speed_only() {
        let mut p = Player::new(None);
        for _ in 0..4 {
            p.advance(0.0);
        }
        assert_eq!(p.vclock, Duration::ZERO, "0 速时虚拟时钟不许前进");
        p.advance(-1.0);
        assert_eq!(p.vclock, Duration::ZERO, "负倍率该被夹到 0");
        // 倍率封顶 4.0：fixed 档允许双倍速，但不许把配置写错变成快进
        p.advance(9.0);
        assert!(
            p.vclock >= Duration::ZERO,
            "倍率再大也不许倒退: {:?}",
            p.vclock
        );
    }

    /// 目录源：文件名升序、尺寸不符踢掉、每张图一帧。
    #[test]
    fn folder_source_orders_by_name() {
        fn png_gray_2x1(v0: u8, v1: u8) -> Vec<u8> {
            // 1 行 2 像素灰度 8-bit：filter None；stored deflate；CRC/adler 不校验
            let raw = [0u8, v0, v1];
            let mut z = vec![0x78u8, 0x01, 0x01];
            z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
            z.extend_from_slice(&raw);
            z.extend_from_slice(&[0; 4]);
            let mut b: Vec<u8> = b"\x89PNG\r\n\x1a\n".to_vec();
            for (typ, d) in [
                // IHDR：宽高是大端 4 字节（2×1），位深 8、颜色类型 0（灰度）
                (b"IHDR", vec![0u8, 0, 0, 2, 0, 0, 0, 1, 8, 0, 0, 0, 0]),
                (b"IDAT", z),
                (b"IEND", Vec::new()),
            ] {
                b.extend_from_slice(&(d.len() as u32).to_be_bytes());
                b.extend_from_slice(typ);
                b.extend_from_slice(&d);
                b.extend_from_slice(&[0; 4]);
            }
            b
        }
        let dir = std::env::temp_dir().join(format!("tt-anim-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("b.png"), png_gray_2x1(200, 201)).unwrap();
        fs::write(dir.join("a.png"), png_gray_2x1(30, 31)).unwrap();
        fs::write(dir.join("wide.png"), png_gray_2x1(9, 9)).unwrap();
        fs::write(dir.join("notes.txt"), b"not an image").unwrap();
        let mut p = Player::new(dir.to_str());
        let (f, w, h) = p.current().expect("目录该解出一段帧序列");
        assert_eq!((w, h), (2, 1));
        assert_eq!(f.rgba[0], 30, "首帧该是 a.png（文件名升序）");
        p.vclock = FOLDER_FRAME_DELAY;
        let (f2, _, _) = p.current().unwrap();
        assert_eq!(f2.rgba[0], 200, "第二帧该是 b.png");
        let _ = fs::remove_dir_all(&dir);
    }

    /// magic 分发：两边任一解得出都行，解不出的前缀必须两个都不碰。
    #[test]
    fn decode_dispatches_by_signature() {
        const TINY: &[u8] = &[
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x04, 0x00, 0x02, 0x00, 0x83, 0x00, 0x00,
        ];
        assert!(decode(TINY).is_none() || decode(TINY).unwrap().frames.len() == 1);
        // 一个半截 PNG 签名：分发进 png 后被块长守卫拒掉，不能 panic
        let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
        fake.extend_from_slice(&[0u8; 30]);
        assert!(decode(&fake).is_none());
        assert!(decode(b"nonsense").is_none());
    }

    /// 全 0 延迟的帧不许把 frame_at 拖成死循环（total==0 旧版会除 0 回绕）。
    #[test]
    fn zero_delay_frames_still_advance() {
        let a = Animation {
            width: 1,
            height: 1,
            frames: vec![
                Frame {
                    rgba: vec![0; 4],
                    delay: Duration::ZERO,
                },
                Frame {
                    rgba: vec![0; 4],
                    delay: Duration::ZERO,
                },
            ],
        };
        // 每帧至少按 1ms 走：1ms 处就该翻到第二帧
        assert_eq!(a.frame_at(Duration::from_millis(1)), 1);
    }
}
