//! 时间解析：相对时长（`"90"`、`"25m"`、`"1h30m"`、`"2d"`）与绝对时刻
//! （`"14:30"`，或 Catime 那套 `t` 后缀写法 `"14 30t"`）。

/// 把时长字符串解析为秒数。
///
/// - 纯数字按秒处理；
/// - 数字 + 单位（`d`/`h`/`m`/`s`，大小写不敏感）的序列，可含空格；
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
            // 天：与 Catime 不同，它明确不收 `d`（`time_parser.c:68-92` 只放行 h m s t）
            Some('d') => 86_400,
            Some(_) => return None,
        };
        total = total.checked_add(num.checked_mul(mult)?)?;
    }
    Some(total)
}

/// 解析绝对时刻 → (时, 分, 秒)，24 小时制。
///
/// 认两种写法：`"14:30"` / `"14:30:45"`，以及 Catime 的 `t` 后缀式
/// `"14 30t"` / `"14t"`（尾部 `t` 表示"倒计时到这个时刻"，冒号与空格都可当分隔符）。
/// **没有 `t` 后缀时仍然要求至少两段**——否则裸数字 `"14"` 会被静默当成 14:00，
/// 而它在我们的语法里是 14 秒，那条歧义不该存在。
pub fn parse_absolute(input: &str) -> Option<(u32, u32, u32)> {
    let input = input.trim();
    let (explicit, body) = match input.strip_suffix(['t', 'T']) {
        Some(rest) => (true, rest),
        None => (false, input),
    };
    let parts: Vec<&str> = body.split([':', ' ']).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    if !explicit && parts.len() < 2 {
        return None;
    }
    // 每一段都必须真是数字：`"14:3o"` 不能因为"第二段读不出来"就当 0 处理
    let mut vals = [0u32; 3];
    for (i, p) in parts.iter().enumerate() {
        vals[i] = p.parse().ok()?;
    }
    let (h, m, s) = (vals[0], vals[1], vals[2]);
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some((h, m, s))
}

/// 绝对时刻距 `now` 还有多少秒，两者都是 `(h, m, s)`；已过则视为明天同一时刻。
pub fn secs_until(target: (u32, u32, u32), now: (u32, u32, u32)) -> u32 {
    let at = |t: (u32, u32, u32)| t.0 * 3600 + t.1 * 60 + t.2;
    let diff = (86_400 + at(target) - at(now)) % 86_400;
    // 正好等于此刻按一整天算：0 秒的倒计时没有意义
    if diff == 0 { 86_400 } else { diff }
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

    /// 天单位：纯加法，`d` 不与其他单位冲突（Catime 明确不收 `d`）。
    #[test]
    fn day_unit() {
        assert_eq!(parse_duration("1d"), Some(86_400));
        assert_eq!(parse_duration("2D"), Some(172_800));
        assert_eq!(parse_duration("1d12h"), Some(129_600));
        assert_eq!(parse_duration("1d 2h 3m 4s"), Some(93_784));
        assert_eq!(parse_duration("1d2d"), Some(259_200), "同类单位重复出现就是累加");
    }

    /// `t` 后缀式绝对时刻（Catime 语法），冒号与空格都能当分隔符。
    #[test]
    fn t_suffix_absolute_time() {
        assert_eq!(parse_absolute("14 30t"), Some((14, 30, 0)));
        assert_eq!(parse_absolute("14:30T"), Some((14, 30, 0)));
        assert_eq!(parse_absolute("14 30 45t"), Some((14, 30, 45)));
        assert_eq!(parse_absolute("9t"), Some((9, 0, 0)), "只给时段就是整点");
        assert_eq!(parse_absolute(" 23 59 t "), Some((23, 59, 0)));
        // 没有 t 时仍然要求至少两段：裸数字是"多少秒"，不该被读成整点
        assert_eq!(parse_absolute("14"), None);
        assert_eq!(parse_absolute("24t"), None);
        assert_eq!(parse_absolute("14 60t"), None);
        assert_eq!(parse_absolute("1 2 3 4t"), None);
        assert_eq!(parse_absolute("t"), None);
        // 既有写法一条都不能退化
        assert_eq!(parse_absolute("14:30"), Some((14, 30, 0)));
        assert_eq!(parse_absolute("23:59:59"), Some((23, 59, 59)));
    }

    /// `t` 式写法要能从 `parse_duration` 那里落下来：`t` 不是时长单位。
    #[test]
    fn t_form_is_not_a_duration() {
        assert_eq!(parse_duration("14 30t"), None);
        assert_eq!(parse_duration("9t"), None);
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

    #[test]
    fn secs_until_wraps_to_tomorrow() {
        assert_eq!(secs_until((14, 30, 0), (10, 0, 0)), 16_200);
        // 已过 → 明天同一时刻
        assert_eq!(secs_until((1, 0, 0), (23, 0, 0)), 7_200);
        // 正好此刻按一整天算
        assert_eq!(secs_until((12, 0, 0), (12, 0, 0)), 86_400);
        assert_eq!(secs_until((0, 0, 0), (0, 0, 10)), 86_390);
    }
}
