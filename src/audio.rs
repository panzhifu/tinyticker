//! 到点提示音：对应 Catime 的 `NOTIFICATION_SOUND_FILE` + `NOTIFICATION_SOUND_VOLUME`
//! （`config_defaults.c:78-79`）与 audio_player 那六个文件的行为。
//!
//! 与它的语义逐条对照：
//! - **空 = 不出声**（Catime：empty for silent success）。
//! - **`beep` 哨兵值**：Catime 写 `"SYSTEM_BEEP"` 走 `MessageBeep`；Linux 没有等价的
//!   "系统提示音"服务（pcspkr 早就不在了），所以这里自己合成两声 880 Hz 短音——
//!   效果同类，且不再需要任何后端。两种写法（`beep` / `SYSTEM_BEEP`）都认。
//! - **文件 = 解码后播**。Catime 用 miniaudio 吃 MP3/WAV；我们只解 WAV（PCM 8/16/24/32
//!   bit 与 IEEE float，任意采样率与 1-8 声道）——MP3 需要整个解码器，与体积卖点冲突。
//! - **后台播放不挡倒计时**（audio_player.h:40 "background worker"）：每次播放 spawn
//!   一条线程，主循环不等它；两次播放交叠时交给 ALSA 的 dmix 自然混音。
//! - **音量 0-100 即时生效**（`SetAudioVolume`）：整数域缩放，取配置当下值。
//! - 失败只警告不重试不排队：放不出声不该让计时器的行为发生变化。
//!
//! ALSA 走运行时 dlopen（[`crate::sys::alsa`]），每个播放线程自己 `load()`：对同一个
//! .so 重复 dlopen 只是引用计数 +1，开销可忽略，省掉把裸函数指针标记成 `Send` 的担保。

use std::fs;
use std::path::Path;

use crate::config::expand_tilde;
use crate::sys::alsa::Alsa;

/// 音频文件的大小上限。提示音按 48 kHz 立体声 16 bit 算 4 MB ≈ 21 秒，足够长；
/// 再大的文件更可能是选错了路径。
const MAX_AUDIO_BYTES: u64 = 4 * 1024 * 1024;

/// `spec` 是不是内置 beep。
pub fn is_beep(spec: &str) -> bool {
    let s = spec.trim();
    s.eq_ignore_ascii_case("beep") || s.eq_ignore_ascii_case("SYSTEM_BEEP")
}

/// 播一次提示音。`spec` = `beep` / `SYSTEM_BEEP` / WAV 路径（支持 `~/`）。
/// 调用方（主循环）不该为它等待——内部一律走后台线程。
pub fn play(spec: &str, volume: u8) {
    if spec.trim().is_empty() || volume == 0 {
        return;
    }
    let spec = spec.trim().to_string();
    let vol = volume.min(100);
    std::thread::spawn(move || {
        let audio = if is_beep(&spec) {
            beep()
        } else {
            match load_wav(&expand_tilde(&spec)) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("⚠️ 提示音 {spec} 放不出来：{e}");
                    return;
                }
            }
        };
        let mut samples = audio.samples;
        scale_volume(&mut samples, vol);
        if let Err(e) = write_to_device(&samples, audio.rate, audio.channels) {
            eprintln!("⚠️ 提示音播放失败：{e}");
        }
    });
}

/// 一段已解码的音频：S16 交错样本 + 采样率 + 声道数（原样保留，交给 ALSA 重采样）。
struct Audio {
    samples: Vec<i16>,
    rate: u32,
    channels: u32,
}

/// 合成两声 880 Hz 短音（各 120 ms、间隔 60 ms），44.1 kHz 立体声。
/// 每声带 8 ms 淡入淡出，免得起停的硬边在好耳机上是一串咔哒。
fn beep() -> Audio {
    const RATE: u32 = 44_100;
    let ms = |n: u32| n as usize * RATE as usize / 1000;
    let mut out: Vec<i16> = Vec::with_capacity((ms(300)) * 2);
    for (i, on) in [(ms(120), true), (ms(60), false), (ms(120), true)] {
        let edge = ms(8).min(i / 2).max(1);
        for s in 0..i {
            // 间隙（on=false）恒 0；两端的样本做淡入淡出
            let env = if !on {
                0.0
            } else if s < edge {
                s as f32 / edge as f32
            } else if s >= i - edge {
                (i - 1 - s) as f32 / edge as f32
            } else {
                1.0
            };
            let v = if on {
                (0.6f32
                    * env
                    * ((s as f32) * 880.0 * 2.0 * std::f32::consts::PI / RATE as f32).sin())
                    * i16::MAX as f32
            } else {
                0.0
            };
            let s16 = v.round() as i16;
            out.extend_from_slice(&[s16, s16]); // 双声道同值
        }
    }
    Audio {
        samples: out,
        rate: RATE,
        channels: 2,
    }
}

/// 读并解码一个 WAV（RIFF/WAVE，fmt + data 两个块就够）。错误信息给人话。
fn load_wav(path: &Path) -> Result<Audio, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读不到文件：{e}"))?;
    if meta.len() > MAX_AUDIO_BYTES {
        return Err(format!("文件超过 {} MB", MAX_AUDIO_BYTES / 1024 / 1024));
    }
    let b = fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let (channels, rate, bits, is_float) = parse_wav_header(&b)?;
    let data = find_chunk(&b, 12, b"data").ok_or("WAV 里没有 data 块")?;
    let samples = decode_pcm(data, bits, is_float, channels).ok_or_else(|| {
        format!(
            "不支持的位深/编码：{bits} bit {}",
            if is_float { "float" } else { "PCM" }
        )
    })?;
    Ok(Audio {
        samples,
        rate,
        channels,
    })
}

/// 校验 RIFF/WAVE 并沿链表找 `fmt `，返回 (channels, rate, bits, is_float)。
fn parse_wav_header(b: &[u8]) -> Result<(u32, u32, u32, bool), String> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("不是 RIFF/WAVE 文件".into());
    }
    let fmt = find_chunk(b, 12, b"fmt ").ok_or("WAV 里没有 fmt 块")?;
    if fmt.len() < 16 {
        return Err("fmt 块太短".into());
    }
    let u16le = |o: usize| u32::from(u16::from_le_bytes([fmt[o], fmt[o + 1]]));
    let codec = u16le(0);
    // 1 = PCM, 3 = IEEE float；其余（μ-law/A-law/WMA/extensible 头）不做
    let is_float = match codec {
        1 => false,
        3 => true,
        other => {
            return Err(format!(
                "不支持的编码 {other}（只解 PCM/float；MP3 等不支持）"
            ));
        }
    };
    let channels = u16le(2);
    let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
    let bits = u16le(14);
    if channels == 0 || channels > 8 || !(8_000..=192_000).contains(&rate) {
        return Err(format!("越界的声道数/采样率：{channels}ch @{rate}Hz"));
    }
    Ok((channels, rate, bits, is_float))
}

/// 从 `from` 起沿 RIFF 链表找名叫 `tag` 的块，返回内容切片。
/// 块按 2 字节对齐（奇数长度后面有 1 字节的填充，不在内容里）。
fn find_chunk<'a>(b: &'a [u8], mut from: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    while from + 8 <= b.len() {
        let len = u32::from_le_bytes([b[from + 4], b[from + 5], b[from + 6], b[from + 7]]) as usize;
        if &b[from..from + 4] == tag {
            let body = from + 8;
            return b.get(body..body.checked_add(len)?);
        }
        from = from + 8 + len + (len & 1);
    }
    None
}

/// 把交错样本流转成 S16（保留原声道数：帧 = channels 个样本，逐样本归一位深）。
fn decode_pcm(data: &[u8], bits: u32, is_float: bool, channels: u32) -> Option<Vec<i16>> {
    let bytes_per = (bits / 8) as usize;
    let stride = bytes_per.checked_mul(channels as usize)?;
    if stride == 0 || data.is_empty() || !data.len().is_multiple_of(stride) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / bytes_per);
    for sample in data.chunks_exact(bytes_per) {
        let s = match (bits, is_float) {
            (8, false) => (sample[0] as i16 - 128) * 256, // 8-bit WAV 无符号，128 = 静音
            (16, false) => i16::from_le_bytes([sample[0], sample[1]]),
            (24, false) => {
                let v = (sample[0] as i32) | (sample[1] as i32) << 8 | (sample[2] as i32) << 16;
                // 左移进 i32 高位再用**算术**右移符号扩展：直接 `as i16` 截低 16 位会丢符号
                ((v << 8) >> 16) as i16
            }
            (32, false) => {
                (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16
            }
            (32, true) => f_to_s16(f32::from_le_bytes([
                sample[0], sample[1], sample[2], sample[3],
            ])),
            (64, true) => f_to_s16(f64::from_le_bytes(sample.try_into().ok()?) as f32),
            _ => return None,
        };
        out.push(s);
    }
    Some(out)
}

fn f_to_s16(f: f32) -> i16 {
    (f.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// 0-100 的音量：整数域乘 256 再移回来，避免每个样本走一遍浮点。
fn scale_volume(samples: &mut [i16], volume: u8) {
    if volume >= 100 {
        return;
    }
    let g = volume.min(100) as i32 * 256 / 100;
    for s in samples.iter_mut() {
        *s = (*s as i32 * g / 256) as i16;
    }
}

/// 打开 default 设备把样本写干净。每个播放线程自己 dlopen 一次（见模块注释）。
fn write_to_device(samples: &[i16], rate: u32, channels: u32) -> Result<(), String> {
    let alsa = Alsa::load().ok_or("打不开 libasound.so.2（没装 ALSA 或兼容层？）")?;
    let name = std::ffi::CString::new("default").expect("常量串");
    let pcm = alsa
        .pcm_open(&name)
        .map_err(|e| format!("打不开播放设备：{e}"))?;
    // 500 ms 缓冲：提示音就一两秒长，latency 大一点更不怕调度抖动
    if let Err(e) = alsa.pcm_setup(pcm, channels, rate, 500_000) {
        alsa.pcm_close(pcm);
        return Err(format!("设备不接受 {channels}ch@{rate}Hz：{e}"));
    }
    let frame = channels as usize;
    let block = frame * 2048; // 每次喂 2048 帧
    let mut done = 0;
    let mut err = None;
    while done < samples.len() {
        let end = (done + block).min(samples.len());
        match alsa.pcm_writei(pcm, &samples[done..end], channels) {
            // 返回的是接下的帧数：换算回样本推进。0 帧是软失败，别再喂。
            Ok(n) if n > 0 => done += n * frame,
            Ok(_) => {
                err = Some("设备拒收数据".to_string());
                break;
            }
            Err(e) => {
                err = Some(format!("写入中断：{e}"));
                break;
            }
        }
    }
    if err.is_none() {
        alsa.pcm_drain(pcm);
    }
    alsa.pcm_close(pcm);
    err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工拼一个 WAV：RIFF 头 + fmt + 一个奇数长度的杂块（测 2 字节对齐跳节）+ data。
    fn wav(codec: u16, channels: u16, bits: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(0u32).to_le_bytes()); // 总长字段解的人不需要
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&codec.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * channels as u32 * bits as u32 / 8).to_le_bytes());
        b.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"LIST"); // 奇数长度杂块
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"adx");
        b.extend_from_slice(&[0u8; 1]); // 填充回偶数
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn header_parses_and_routes_to_data() {
        let b = wav(1, 2, 16, 48000, &[1, 2, 3, 4]);
        let (ch, rate, bits, flt) = parse_wav_header(&b).expect("头该解出来");
        assert_eq!((ch, rate, bits, flt), (2, 48000, 16, false));
        // 奇数 LIST 块之后必须仍能对上 data 的偏移
        assert_eq!(find_chunk(&b, 12, b"data"), Some(&[1u8, 2, 3, 4][..]));
    }

    #[test]
    fn rejects_garbage_unknown_codec_and_bad_ranges() {
        assert!(parse_wav_header(b"not a wav at all!!").is_err());
        assert!(
            parse_wav_header(&wav(6, 1, 16, 44100, b""))
                .unwrap_err()
                .contains("编码")
        );
        assert!(
            parse_wav_header(&wav(1, 9, 16, 44100, b"")).is_err(),
            "9 声道越界"
        );
        assert!(
            parse_wav_header(&wav(1, 1, 16, 8000, b"")).is_ok(),
            "8000Hz 是下界，含"
        );
        assert!(
            parse_wav_header(&wav(1, 1, 16, 7999, b"")).is_err(),
            "低于下界该拒"
        );
    }

    #[test]
    fn bit_depths_map_to_s16() {
        // 8-bit 无符号：0 → -32768，255 → +32512
        let s = decode_pcm(&[0, 255], 8, false, 2).unwrap();
        assert_eq!(s, vec![-32768i16, 32512]);
        // 16-bit 原样（立体声两帧四样本）
        let raw = [0x00u8, 0x40, 0x00, -0x40i8 as u8, 0x01, 0x00, 0xFF, 0x7F];
        let s = decode_pcm(&raw, 16, false, 2).unwrap();
        assert_eq!(s, vec![0x4000i16, -0x4000, 1, i16::MAX]);
        // 24-bit：0x7FFFFF → 高 16 位 0x7FFF；负值符号扩展
        let s = decode_pcm(&[0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80], 24, false, 1).unwrap();
        assert_eq!(s, vec![0x7FFF, -0x8000]);
        // 32-bit int：右移 16
        let mut raw = Vec::new();
        raw.extend_from_slice(&(0x0001_8000i32).to_le_bytes());
        let s = decode_pcm(&raw, 32, false, 1).unwrap();
        assert_eq!(s, vec![1i16]);
        // float32：1.5 夹到 MAX，-0.5 ≈ -16384
        let mut raw = Vec::new();
        raw.extend_from_slice(&1.5f32.to_le_bytes());
        raw.extend_from_slice(&(-0.5f32).to_le_bytes());
        let s = decode_pcm(&raw, 32, true, 2).unwrap();
        assert_eq!(s[0], i16::MAX);
        assert!((s[1] + 16383).abs() <= 1, "{s:?}");
        // 长度不落在帧边界上的数据整块拒收
        assert!(decode_pcm(&[0; 3], 16, false, 2).is_none());
        // 不支持的组合（12 bit）
        assert!(decode_pcm(&[0; 24], 12, false, 1).is_none());
    }

    #[test]
    fn stereo_stays_interleaved() {
        // 立体声 L=+1.0 R=-1.0 一帧：交错必须保持 L,R 顺序与各自的值。
        // 注意非对称量化：正满幅是 32767，负满幅也是 -32767（i16::MIN 不可达）
        let mut raw = Vec::new();
        raw.extend_from_slice(&1.0f32.to_le_bytes());
        raw.extend_from_slice(&(-1.0f32).to_le_bytes());
        let s = decode_pcm(&raw, 32, true, 2).unwrap();
        assert_eq!(s, vec![i16::MAX, -i16::MAX]);
    }

    #[test]
    fn volume_scales_and_clips() {
        let mut s = vec![1000i16, -2000, 0];
        scale_volume(&mut s, 50);
        assert_eq!(s, vec![500, -1000, 0]);
        let mut t = vec![10i16];
        scale_volume(&mut t, 100);
        assert_eq!(t, vec![10], "满音量不许动样本");
    }

    #[test]
    fn beep_is_two_tones_with_a_silent_gap() {
        let a = beep();
        assert_eq!((a.rate, a.channels), (44100, 2));
        assert!(!a.samples.is_empty());
        // 每一帧左右声道同值（beep 没有立体声内容）
        assert!(a.samples.chunks(2).all(|f| f[0] == f[1]));
        // 两声之间确实有一段全 0 的间隙：扫到的最长静音段要有 60ms 的量级
        let longest_silence = a
            .samples
            .chunks(2)
            .map(|f| i32::from(f[0]).abs())
            .scan(0u32, |run, v| {
                *run = if v == 0 { *run + 1 } else { 0 };
                Some(*run)
            })
            .max()
            .unwrap_or(0);
        assert!(longest_silence >= 2000, "间隙太短（{longest_silence} 帧）");
        // 也必须有声音：全零的 beep 是 bug
        assert!(a.samples.iter().any(|&v| v != 0));
    }

    #[test]
    fn beep_sentinels_are_recognized() {
        assert!(is_beep("beep") && is_beep("BEEP") && is_beep(" SYSTEM_BEEP "));
        assert!(!is_beep("~/x.wav"));
    }
}
