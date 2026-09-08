//! 本地时间读取与格式化（时钟挂件模式用）。
//!
//! std 没有本地时区 API，这里经 libc 的 `localtime_r` 转换
//!（libc 已在依赖树中，几乎零体积成本），避免为此引入 chrono。

/// 当前本地时间 (时, 分, 秒)，24 小时制。
pub fn now_hms() -> (u32, u32, u32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}

/// 格式化时间；`use_12h` 为 true 时输出 12 小时制并带 AM/PM。
///
/// - 24 小时制：`"HH:MM:SS"`（8 字符）
/// - 12 小时制：`"hh:mm:ss AM"`（11 字符，仍容纳于 200px 宽的默认窗口）
pub fn format_hms(h: u32, m: u32, s: u32, use_12h: bool) -> String {
    if !use_12h {
        return format!("{h:02}:{m:02}:{s:02}");
    }
    let (h12, suffix) = match h {
        0 => (12, "AM"),  // 午夜
        1..=11 => (h, "AM"),
        12 => (12, "PM"), // 正午
        _ => (h - 12, "PM"),
    };
    format!("{h12:02}:{m:02}:{s:02} {suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_formatting_24h() {
        assert_eq!(format_hms(0, 0, 0, false), "00:00:00");
        assert_eq!(format_hms(9, 5, 3, false), "09:05:03");
        assert_eq!(format_hms(23, 59, 59, false), "23:59:59");
    }

    #[test]
    fn hms_formatting_12h() {
        // 午夜与正午是 12 小时制最容易错的两个点
        assert_eq!(format_hms(0, 0, 0, true), "12:00:00 AM");
        assert_eq!(format_hms(12, 30, 0, true), "12:30:00 PM");
        assert_eq!(format_hms(9, 5, 3, true), "09:05:03 AM");
        assert_eq!(format_hms(13, 5, 3, true), "01:05:03 PM");
        assert_eq!(format_hms(23, 59, 59, true), "11:59:59 PM");
    }

    #[test]
    fn now_is_in_range() {
        let (h, m, s) = now_hms();
        assert!(h < 24 && m < 60 && s < 60);
    }
}
