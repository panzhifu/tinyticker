//! libasound（ALSA）的 C ABI 声明层。
//!
//! 与 [`crate::sys::freetype`] 同一手法：运行时 `dlopen` 发行版自带的
//! `libasound.so.2` 取函数指针，不引 crate、不链 `-lasound`、构建不需要 alsa 的
//! `-dev` 包。只镜像播放一条最短路径用得上的六个符号，不镜像 `snd_pcm_t` 的
//! 任何内部结构——句柄对我们永远是 `*mut c_void`。
//!
//! # 为什么只用 `snd_pcm_set_params` 这一个高层入口
//!
//! ALSA 的低层配置（hw_params/sw_params + 格式转换 + 软线性重采样）是几十次
//! 调用的状态机；而 `snd_pcm_set_params`（pcm.h 里的高层 helper）一次调用替你把
//! 这些全做完——喂 S16_LE 交错帧，插件层按设备能力自行重采样/换格式/走 dmix。
//! 对"到点放一声提示音"的需求，这一个入口就够了，代码面和出错面都最小。

use std::ffi::{CStr, c_char, c_int, c_void};

/// `SND_PCM_FORMAT_S16_LE`（pcm.h 的枚举序）。取错值 set_params 直接回 -EINVAL，
/// 不会画坏东西。
pub const FORMAT_S16_LE: c_int = 2;
/// `SND_PCM_ACCESS_RW_INTERLEAVED`。
pub const ACCESS_RW_INTERLEAVED: c_int = 3;
/// `SND_PCM_STREAM_PLAYBACK`。
pub const STREAM_PLAYBACK: c_int = 0;

/// `snd_pcm_t*`：对我们永远是不透明指针。
type Pcm = *mut c_void;
/// `snd_pcm_uframes_t`（unsigned long）。
type Uframes = usize;
/// `unsigned int`。
type Uint = u32;

type FnOpen = unsafe extern "C" fn(*mut Pcm, *const c_char, c_int, Uint) -> c_int;
type FnSetParams = unsafe extern "C" fn(Pcm, c_int, c_int, Uint, Uint, c_int, Uint) -> c_int;
type FnWritei = unsafe extern "C" fn(Pcm, *const c_void, Uframes) -> isize;
type FnDrain = unsafe extern "C" fn(Pcm, c_int) -> c_int;
type FnClose = unsafe extern "C" fn(Pcm) -> c_int;
type FnStrerror = unsafe extern "C" fn(c_int) -> *const c_char;

/// 已加载的 libasound + 取好的六个函数指针。任一符号缺失都不构造它。
pub struct Alsa {
    open: FnOpen,
    set_params: FnSetParams,
    writei: FnWritei,
    drain: FnDrain,
    close: FnClose,
    strerror: FnStrerror,
}

/// ALSA 返回码 + `snd_strerror` 的人话。
pub struct AlsaErr {
    pub code: c_int,
    pub msg: String,
}

impl std::fmt::Display for AlsaErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.msg, self.code)
    }
}

impl Alsa {
    /// `dlopen("libasound.so.2")`。没有 ALSA 的机器（PipeWire-only 且没装
    /// alsa 兼容层）返回 `None`，调用方只该安静地不出声并把原因解释一次。
    pub fn load() -> Option<Self> {
        let lib = crate::sys::Lib::open("libasound.so.2")?;
        Some(Self {
            open: lib.func("snd_pcm_open")?,
            set_params: lib.func("snd_pcm_set_params")?,
            writei: lib.func("snd_pcm_writei")?,
            drain: lib.func("snd_pcm_drain")?,
            close: lib.func("snd_pcm_close")?,
            strerror: lib.func("snd_strerror")?,
        })
    }

    /// 打开播放设备。`name` 一般给 `"default"`（dmix / pulse / pipewire-alsa
    /// 由用户的 ALSA 配置决定）。
    pub fn pcm_open(&self, name: &CStr) -> Result<Pcm, AlsaErr> {
        let mut pcm: Pcm = std::ptr::null_mut();
        // `mode = 0`：阻塞模式。播放跑在专用线程里，不需要非阻塞。
        let rc = unsafe { (self.open)(&raw mut pcm, name.as_ptr(), STREAM_PLAYBACK, 0) };
        if rc < 0 { Err(self.err(rc)) } else { Ok(pcm) }
    }

    /// 一次配齐：S16_LE 交错、`channels` 声道、`rate` Hz，开软重采样，
    /// 缓冲目标 `latency_us` 微秒。
    pub fn pcm_setup(
        &self,
        pcm: Pcm,
        channels: Uint,
        rate: Uint,
        latency_us: Uint,
    ) -> Result<(), AlsaErr> {
        let rc = unsafe {
            (self.set_params)(
                pcm,
                FORMAT_S16_LE,
                ACCESS_RW_INTERLEAVED,
                channels,
                rate,
                1,
                latency_us,
            )
        };
        if rc < 0 { Err(self.err(rc)) } else { Ok(()) }
    }

    /// 写一段 S16 交错样本（`frames = samples / channels`，调用方保证整除）。
    /// 返回实际接收的帧数；缓冲满时阻塞到设备吃掉为止——本函数只在播放线程出现。
    pub fn pcm_writei(&self, pcm: Pcm, samples: &[i16], channels: Uint) -> Result<usize, AlsaErr> {
        let ch = channels.max(1) as usize;
        debug_assert_eq!(samples.len() % ch, 0, "交错样本数必须整除声道数");
        let frames = samples.len() / ch;
        let n = unsafe { (self.writei)(pcm, samples.as_ptr() as *const c_void, frames) };
        // -EPIPE（xrun）由 set_params 建立的自动恢复处理；除此之外负值都是硬失败。
        if n < 0 {
            Err(self.err(n as c_int))
        } else {
            Ok(n as usize)
        }
    }

    /// 把缓冲里剩下的样本放完再返回（不打断）。
    pub fn pcm_drain(&self, pcm: Pcm) {
        unsafe { (self.drain)(pcm, 0) };
    }

    pub fn pcm_close(&self, pcm: Pcm) {
        unsafe { (self.close)(pcm) };
    }

    fn err(&self, code: c_int) -> AlsaErr {
        let msg = unsafe {
            let p = (self.strerror)(code);
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        AlsaErr { code, msg }
    }
}
