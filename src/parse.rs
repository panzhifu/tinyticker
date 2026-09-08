//! 时间解析：相对时长（`"90"`、`"25m"`、`"1h30m"`）与绝对时刻（`"14:30"`）。

/// 把时长字符串解析为秒数。
///
/// - 纯数字按秒处理；
/// - 数字 + 单位（`h`/`m`/`s`，大小写不敏感）的序列，可含空格；
///   末尾无单位的数字段按秒（`"1h30"` = 1 小时 30 秒）。
///
/// 无法解析或数值溢出时返回 `None`。
pub fn parse_duration(input: &str) -> Option<u32> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    // 快速路径：纯数字按秒
    if let Ok(secs) = s.parse::<u32>() {
        return Some(secs);
    }

    let mut total: u32 = 0;
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_whitespace() {
            chars.next();
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        let mut num: u32 = 0;
        while let Some(&d) = chars.peek() {
            if !d.is_ascii_digit() {
                break;
            }
            num = num.checked_mul(10)?.checked_add(d.to_digit(10)?)?;
            chars.next();
        }
        let mult = match chars.next().map(|u| u.to_ascii_lowercase()) {
            None => 1, // 末尾无单位
            Some('s') => 1,
            Some('m') => 60,
            Some('h') => 3600,
            Some(_) => return None,
        };
        total = total.checked_add(num.checked_mul(mult)?)?;
    }
    Some(total)
}

/// 解析绝对时刻 `"14:30"` / `"14:30:45"` → (时, 分, 秒)，24 小时制。
pub fn parse_absolute(input: &str) -> Option<(u32, u32, u32)> {
    let input = input.trim();
    let mut parts = input.split(':');
    let h: u32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let s: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some((h, m, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_number_is_seconds() {
        assert_eq!(parse_duration("90"), Some(90));
        assert_eq!(parse_duration(" 5 "), Some(5));
        assert_eq!(parse_duration("0"), Some(0));
    }

    #[test]
    fn single_unit() {
        assert_eq!(parse_duration("25m"), Some(1500));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("45S"), Some(45));
        assert_eq!(parse_duration("90M"), Some(5400));
    }

    #[test]
    fn mixed_units_with_spaces() {
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("1h 30m"), Some(5400));
        assert_eq!(parse_duration("2h15m10s"), Some(8110));
        assert_eq!(parse_duration("1h30"), Some(3630)); // 末尾无单位按秒
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("   "), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("m25"), None);
        assert_eq!(parse_duration("1.5h"), None);
        assert_eq!(parse_duration("1x"), None);
    }

    #[test]
    fn rejects_overflow() {
        assert_eq!(parse_duration("4294967296"), None); // > u32::MAX
        assert_eq!(parse_duration("999999999999h"), None);
    }

    #[test]
    fn absolute_time_parsing() {
        assert_eq!(parse_absolute("14:30"), Some((14, 30, 0)));
        assert_eq!(parse_absolute("9:05"), Some((9, 5, 0)));
        assert_eq!(parse_absolute("23:59:59"), Some((23, 59, 59)));
        assert_eq!(parse_absolute(" 14:30 "), Some((14, 30, 0)));
        // 越界 / 格式错误
        assert_eq!(parse_absolute("24:00"), None);
        assert_eq!(parse_absolute("14:60"), None);
        assert_eq!(parse_absolute("14:30:60"), None);
        assert_eq!(parse_absolute("14"), None);
        assert_eq!(parse_absolute("14:30:45:00"), None);
        assert_eq!(parse_absolute("14:3o"), None);
        assert_eq!(parse_absolute(""), None);
    }
}
