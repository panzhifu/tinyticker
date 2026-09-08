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

/// 格式化为 `"HH:MM:SS"`。
pub fn format_hms(h: u32, m: u32, s: u32) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_formatting() {
        assert_eq!(format_hms(0, 0, 0), "00:00:00");
        assert_eq!(format_hms(9, 5, 3), "09:05:03");
        assert_eq!(format_hms(23, 59, 59), "23:59:59");
    }

    #[test]
    fn now_is_in_range() {
        let (h, m, s) = now_hms();
        assert!(h < 24 && m < 60 && s < 60);
    }
}
