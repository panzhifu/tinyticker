//! 本地时间读取与格式化（时钟挂件模式用）。
//!
//! std 没有本地时区 API。这里不依赖 C 库的 `localtime_r`（Windows 上根本没有），
//! 而是自己解析 IANA TZif 数据库：`$TZ` → `/etc/localtime` → `/usr/share/zoneinfo/UTC`。
//!
//! 覆盖 TZif v2/v3（64 位 transition 表，跨过 2038 不出错）与表尾的 POSIX 规则串——
//! 后者是必需的：发行版的 transition 表只排到 2037 年，不实现规则串就会在此后
//! 静默错一小时。`TZ=EST5EDT` 这类纯规则值也直接支持。
//!
//! 挂件只显示时分秒，因此不做「历日 → 年月日」的输出换算，只在判定 DST 边界时
//! 用到底层的民用日历算法。

use std::fs;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const SECS_PER_DAY: i64 = 86_400;
const TZINFO_DIR: &str = "/usr/share/zoneinfo";

/// 当前本地时间 (时, 分, 秒)，24 小时制。
pub fn now_hms() -> (u32, u32, u32) {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    default_zone().hms(epoch)
}

/// 进程内解析一次：transition 表与规则串都是静态数据，跨 DST 边界无需重读文件。
fn default_zone() -> &'static Zone {
    static ZONE: OnceLock<Zone> = OnceLock::new();
    ZONE.get_or_init(|| match zone_from_env() {
        Some(zone) => zone,
        None => {
            eprintln!("⚠️ 读不到时区数据（$TZ / /etc/localtime），时钟按 UTC 显示");
            Zone::utc()
        }
    })
}

/// 按 `$TZ` 的语义定位时区；失败返回 `None`。
fn zone_from_env() -> Option<Zone> {
    let tz = std::env::var("TZ").unwrap_or_default();
    let tz = tz.strip_prefix(':').unwrap_or(&tz);
    if tz.is_empty() {
        return Zone::load("/etc/localtime").or_else(|| Zone::load(format!("{TZINFO_DIR}/UTC")));
    }
    // 绝对路径直接当文件；含 '/' 的相对名按 zoneinfo 下的文件；否则是 POSIX 规则串
    if tz.starts_with('/') {
        return Zone::load(tz);
    }
    if tz.contains('/')
        && let Some(zone) = Zone::load(format!("{TZINFO_DIR}/{tz}"))
    {
        return Some(zone);
    }
    Zone::from_posix(tz)
}

/// 一个时区：transition 表 + 表尾之后的 POSIX 规则。
pub struct Zone {
    /// transition 时刻（UTC epoch 秒，升序）
    times: Vec<i64>,
    /// 每个 transition 指向 `types` 的下标
    kinds: Vec<usize>,
    /// 本地时间类型：(相对 UTC 的偏移秒, 是否夏令时)
    types: Vec<(i32, bool)>,
    /// 最后一个 transition 之后生效的 POSIX 规则（无 transition 时全程生效）
    rule: Option<PosixRule>,
}

impl Zone {
    fn utc() -> Self {
        Self {
            times: Vec::new(),
            kinds: Vec::new(),
            types: vec![(0, false)],
            rule: None,
        }
    }

    /// 从 TZif 文件加载。
    pub fn load(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let data = fs::read(path).ok()?;
        parse_tzif(&data)
    }

    /// 从 POSIX 规则串构造（无历史 transition，全程按规则）。
    pub fn from_posix(spec: &str) -> Option<Self> {
        let rule = parse_posix(spec)?;
        Some(Self {
            times: Vec::new(),
            kinds: Vec::new(),
            types: vec![(rule.std_off, false)],
            rule: Some(rule),
        })
    }

    /// 该时刻相对 UTC 的偏移秒。
    pub fn offset_at(&self, epoch: i64) -> i32 {
        if let Some(rule) = &self.rule {
            // 无表，或已越过表尾：由 POSIX 规则决定
            if self.times.is_empty() || epoch >= self.times[self.times.len() - 1] {
                return rule.offset_at(epoch);
            }
        }
        match self.times.binary_search(&epoch) {
            Ok(i) => self.types[self.kinds[i]].0,
            // 早于第一个 transition：按 RFC 8536 取第一个非夏令时类型（而非 types[0]）
            Err(0) => self
                .types
                .iter()
                .find(|(_, dst)| !*dst)
                .or(self.types.first())
                .map(|(off, _)| *off)
                .unwrap_or(0),
            Err(i) => self.types[self.kinds[i - 1]].0,
        }
    }

    /// 该时刻的本地时分秒。
    pub fn hms(&self, epoch: i64) -> (u32, u32, u32) {
        let local = epoch + self.offset_at(epoch) as i64;
        let sod = local.rem_euclid(SECS_PER_DAY) as u32;
        (sod / 3600, sod % 3600 / 60, sod % 60)
    }
}

// —— TZif 解析 ——

/// 解析 TZif；优先用 v2+ 的 64 位数据块。
fn parse_tzif(data: &[u8]) -> Option<Zone> {
    if data.len() < 44 || &data[0..4] != b"TZif" {
        return None;
    }
    let version = data[4];
    let (isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt) = header(&data[20..44])?;
    let v1_len = 44
        + timecnt * 4
        + timecnt
        + typecnt * 6
        + charcnt
        + leapcnt * 8
        + isstdcnt
        + isutcnt;
    // v1 只有 32 位时刻，2038 年会溢出；有 v2+ 就读它
    let (start, time_width) = if version == 0 || version == b'1' {
        (0usize, 4usize)
    } else {
        (v1_len, 8)
    };
    let (isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt) = header(&data[start + 20..])?;
    let body = start + 44;
    let times_off = body;
    let kinds_off = times_off + timecnt * time_width;
    let types_off = kinds_off + timecnt;
    let char_off = types_off + typecnt * 6;
    let end = char_off + charcnt + leapcnt * (time_width + 4) + isstdcnt + isutcnt;
    if data.len() < end {
        return None;
    }

    let times = (0..timecnt)
        .map(|i| match time_width {
            4 => i64::from(be_i32(&data[times_off + i * 4..])),
            _ => be_i64(&data[times_off + i * 8..]),
        })
        .collect();
    let kinds = (0..timecnt).map(|i| data[kinds_off + i] as usize).collect();
    let types = (0..typecnt)
        .map(|i| {
            let at = &data[types_off + i * 6..];
            (be_i32(at), at[4] != 0)
        })
        .collect();

    // 表尾：`\n` POSIX 规则串 `\n`
    let rule = data.get(end..)?
        .iter()
        .position(|&b| b == b'\n')
        .and_then(|nl| {
            let tail = &data[end + nl + 1..];
            let len = tail.iter().position(|&b| b == b'\n').unwrap_or(tail.len());
            std::str::from_utf8(&tail[..len]).ok()
        })
        .and_then(|s| if s.is_empty() { None } else { parse_posix(s) });

    Some(Zone {
        times,
        kinds,
        types,
        rule,
    })
}

fn header(b: &[u8]) -> Option<(usize, usize, usize, usize, usize, usize)> {
    if b.len() < 24 {
        return None;
    }
    let mut n = [0usize; 6];
    for (i, slot) in n.iter_mut().enumerate() {
        *slot = be_u32(&b[i * 4..]) as usize;
    }
    let [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt] = n;
    // 明显不合理的长度直接拒绝，避免下面算偏移时溢出
    if timecnt > 100_000 || typecnt > 1000 || charcnt > 10_000 || leapcnt > 1000 {
        return None;
    }
    Some((isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt))
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn be_i32(b: &[u8]) -> i32 {
    be_u32(b) as i32
}

fn be_i64(b: &[u8]) -> i64 {
    i64::from_be_bytes(b[..8].try_into().unwrap())
}

// —— POSIX 时区规则串 ——
//
// 形如 `STDoffset[DST[offset][,start[/time],end[/time]]]`，例如
// `CET-1CEST,M3.5.0,M10.5.0/3`、`EST5EDT`、`CST-8`。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PosixRule {
    /// 标准时相对 UTC 的偏移秒
    std_off: i32,
    /// 夏令时：(偏移秒, 起始, 结束)
    dst: Option<(i32, RuleTime, RuleTime)>,
}

/// 规则里的切换点：日序 + 当日时刻（秒）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RuleTime {
    day: DayOfYear,
    /// 当日秒数，默认 02:00:00
    at: i32,
}

/// `Jn` / `n` / `Mm.w.d` 三种日序写法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DayOfYear {
    /// 1-365，闰年也不计 2 月 29 日
    Julian(u32),
    /// 0-365，闰年计 2 月 29 日
    Ordinal(u32),
    /// 月 1-5，周 0-5（5 = 最后一个），星期 0=周日
    MonthDay(u32, u32, u32),
}

impl PosixRule {
    /// 该 UTC 时刻的偏移秒。
    fn offset_at(&self, epoch: i64) -> i32 {
        let Some((dst_off, start, end)) = self.dst else {
            return self.std_off;
        };
        // 全部换算到「标准时本地秒」这一时基上比较，避免夏令时期间的循环依赖
        let std_local = epoch + self.std_off as i64;
        let year = civil_from_days(std_local.div_euclid(SECS_PER_DAY)).0;
        let jan1 = days_from_civil(year, 1, 1) * SECS_PER_DAY;
        // start 按标准时计，end 按夏令时计（故要减回一个 DST 增量）
        let start_at = jan1 + start.day.second_of_year(year) + start.at as i64;
        let end_at = jan1 + end.day.second_of_year(year) + end.at as i64 - (dst_off - self.std_off) as i64;

        let in_dst = if start_at <= end_at {
            std_local >= start_at && std_local < end_at
        } else {
            // 南半球：夏令时跨年
            std_local >= start_at || std_local < end_at
        };
        if in_dst { dst_off } else { self.std_off }
    }
}

impl DayOfYear {
    /// 该日序在指定年份里是第几天（0 基，相对 1 月 1 日），乘 86400 即一年中的第几秒。
    fn second_of_year(&self, year: i64) -> i64 {
        let leap = is_leap(year);
        let day0 = match *self {
            // Jn：不计闰日，故闰年里 3 月 1 日之后要往后挪一天
            DayOfYear::Julian(n) => {
                let mut d = n.saturating_sub(1) as i64;
                if leap && d >= 59 {
                    d += 1;
                }
                d
            }
            DayOfYear::Ordinal(n) => n.min(365) as i64,
            // 月内日序要再折算成一年中的第几天
            DayOfYear::MonthDay(m, w, d) => {
                let within_month = nth_weekday_of_month(year, m, w, d);
                let first = days_from_civil(year, m, 1);
                let jan1 = days_from_civil(year, 1, 1);
                (first - jan1) + within_month
            }
        };
        day0.max(0) * SECS_PER_DAY
    }
}

/// 规则串解析游标。
struct Spec<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Spec<'a> {
    fn new(s: &'a str) -> Self {
        Self { b: s.as_bytes(), i: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    /// 消费一个指定字符。
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// 时区名：`<...>` 包裹，或字母/下划线串。
    fn read_name(&mut self) -> bool {
        if self.peek() == Some(b'<') {
            self.i += 1;
            let start = self.i;
            while self.peek().is_some_and(|c| c != b'>') {
                self.i += 1;
            }
            return self.i > start && self.eat(b'>');
        }
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') {
            self.i += 1;
        }
        self.i > start
    }

    fn read_num(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok()
    }

    /// `[+-]h[:mm[:ss]]`，缺省符号为正；POSIX 里符号表示「UTC + offset」。
    fn read_offset(&mut self) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'-') => {
                self.i += 1;
                -1
            }
            Some(b'+') => {
                self.i += 1;
                1
            }
            _ => 1,
        };
        Some(sign * self.read_clock_secs()?)
    }

    /// `h[:mm[:ss]]`（无符号），用于 offset 的数值部分与 `/time`。
    fn read_clock_secs(&mut self) -> Option<i32> {
        let mut secs = self.read_num()? as i32 * 3600;
        if self.eat(b':') {
            secs += self.read_num()? as i32 * 60;
            if self.eat(b':') {
                secs += self.read_num()? as i32;
            }
        }
        Some(secs)
    }

    /// 切换时刻，缺省 02:00:00。
    fn read_rule_time(&mut self) -> Option<RuleTime> {
        let day = match self.peek() {
            Some(b'J') => {
                self.i += 1;
                let n = self.read_num()?;
                if !(1..=365).contains(&n) {
                    return None;
                }
                DayOfYear::Julian(n)
            }
            Some(b'M') => {
                self.i += 1;
                let m = self.read_num()?;
                if !self.eat(b'.') {
                    return None;
                }
                let w = self.read_num()?;
                if !self.eat(b'.') {
                    return None;
                }
                let d = self.read_num()?;
                if !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 {
                    return None;
                }
                DayOfYear::MonthDay(m, w, d)
            }
            Some(c) if c.is_ascii_digit() => {
                let n = self.read_num()?;
                if n > 365 {
                    return None;
                }
                DayOfYear::Ordinal(n)
            }
            _ => return None,
        };
        let at = if self.eat(b'/') {
            self.read_signed_secs()?
        } else {
            2 * 3600
        };
        Some(RuleTime { day, at })
    }

    /// `/time` 允许带符号（如 `+167`）。
    fn read_signed_secs(&mut self) -> Option<i32> {
        let sign = match self.peek() {
            Some(b'-') => {
                self.i += 1;
                -1
            }
            Some(b'+') => {
                self.i += 1;
                1
            }
            _ => 1,
        };
        Some(sign * self.read_clock_secs()?)
    }
}

/// 解析 POSIX 规则串；不合法时返回 `None`。
fn parse_posix(spec: &str) -> Option<PosixRule> {
    let mut s = Spec::new(spec);
    if !s.read_name() {
        return None;
    }
    // POSIX 的 offset 是「本地 + offset = UTC」，内部用的是「UTC + off = 本地」，故取负
    let std_off = -s.read_offset()?;
    let rule = PosixRule {
        std_off,
        dst: None,
    };
    // 名字之后没有内容，或压根不是名字 → 只有标准时
    if !s.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == b'<') {
        return Some(rule);
    }
    if !s.read_name() {
        return Some(rule);
    }
    // DST 偏移缺省为标准时 +1 小时
    let dst_off = if s.peek().is_some_and(|c| c.is_ascii_digit() || c == b'-' || c == b'+') {
        -s.read_offset()?
    } else {
        std_off + 3600
    };
    // 省略切换规则时按美国本地规则（RFC 8536）
    let (start, end) = if s.eat(b',') {
        let start = s.read_rule_time()?;
        if !s.eat(b',') {
            return None;
        }
        (start, s.read_rule_time()?)
    } else {
        (
            RuleTime {
                day: DayOfYear::MonthDay(3, 5, 0),
                at: 2 * 3600,
            },
            RuleTime {
                day: DayOfYear::MonthDay(10, 5, 0),
                at: 2 * 3600,
            },
        )
    };
    Some(PosixRule {
        std_off,
        dst: Some((dst_off, start, end)),
    })
}

// —— 民用日历（Howard Hinnant 的 days/civil 算法，1970-01-01 为第 0 天）——

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 由天数还原 (年, 月, 日)；这里只需要年份。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// 某年第 m 月第 w 个星期 d 的月内日序（0 基，相对当月 1 日）。
fn nth_weekday_of_month(year: i64, month: u32, week: u32, dow: u32) -> i64 {
    let first_dow = weekday_from_days(days_from_civil(year, month, 1));
    let mut day = 1i64 + (dow as i64 - first_dow as i64).rem_euclid(7) + (week.saturating_sub(1) as i64) * 7;
    let last = days_in_month(year, month);
    if day > last {
        day -= 7; // 第 5 周不足时取该月最后一个
    }
    day - 1
}

/// 1970-01-01 是星期四；返回 0=周日 … 6=周六。
fn weekday_from_days(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
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

    /// 对拍基准：同一条 (时区, UTC epoch) → (本地时分秒, 偏移秒) 取自 Python 标准库
    /// `zoneinfo`（独立实现）。刻意覆盖 2038 前后、表尾之后（靠 POSIX 规则）、
    /// 南北半球 DST、45 分钟偏移、以及早于首个 transition 的 1970 年。
    /// (时区, UTC epoch, 期望本地时分秒, 期望偏移秒)
    type Vector = (&'static str, i64, (u32, u32, u32), i32);
    const VECTORS: &[Vector] = &[
        ("Asia/Shanghai", 0, (8, 0, 0), 28800),
        ("Asia/Shanghai", 300000000, (13, 20, 0), 28800),
        ("Asia/Shanghai", 951789600, (10, 0, 0), 28800),
        ("Asia/Shanghai", 1048323600, (17, 0, 0), 28800),
        ("Asia/Shanghai", 1141606800, (9, 0, 0), 28800),
        ("Asia/Shanghai", 1585267200, (8, 0, 0), 28800),
        ("Asia/Shanghai", 1709254800, (9, 0, 0), 28800),
        ("Asia/Shanghai", 2147483647, (11, 14, 7), 28800),
        ("Asia/Shanghai", 2147483648, (11, 14, 8), 28800),
        ("Asia/Shanghai", 3000000000, (13, 20, 0), 28800),
        ("Asia/Shanghai", 4102444800, (8, 0, 0), 28800),
        ("Asia/Shanghai", 1520298000, (9, 0, 0), 28800),
        ("Europe/Berlin", 0, (1, 0, 0), 3600),
        ("Europe/Berlin", 300000000, (6, 20, 0), 3600),
        ("Europe/Berlin", 951789600, (3, 0, 0), 3600),
        ("Europe/Berlin", 1048323600, (10, 0, 0), 3600),
        ("Europe/Berlin", 1141606800, (2, 0, 0), 3600),
        ("Europe/Berlin", 1585267200, (1, 0, 0), 3600),
        ("Europe/Berlin", 1709254800, (2, 0, 0), 3600),
        ("Europe/Berlin", 2147483647, (4, 14, 7), 3600),
        ("Europe/Berlin", 2147483648, (4, 14, 8), 3600),
        ("Europe/Berlin", 3000000000, (6, 20, 0), 3600),
        ("Europe/Berlin", 4102444800, (1, 0, 0), 3600),
        ("Europe/Berlin", 1520298000, (2, 0, 0), 3600),
        ("America/New_York", 0, (19, 0, 0), -18000),
        ("America/New_York", 300000000, (1, 20, 0), -14400),
        ("America/New_York", 951789600, (21, 0, 0), -18000),
        ("America/New_York", 1048323600, (4, 0, 0), -18000),
        ("America/New_York", 1141606800, (20, 0, 0), -18000),
        ("America/New_York", 1585267200, (20, 0, 0), -14400),
        ("America/New_York", 1709254800, (20, 0, 0), -18000),
        ("America/New_York", 2147483647, (22, 14, 7), -18000),
        ("America/New_York", 2147483648, (22, 14, 8), -18000),
        ("America/New_York", 3000000000, (0, 20, 0), -18000),
        ("America/New_York", 4102444800, (19, 0, 0), -18000),
        ("America/New_York", 1520298000, (20, 0, 0), -18000),
        ("Australia/Sydney", 0, (10, 0, 0), 36000),
        ("Australia/Sydney", 300000000, (15, 20, 0), 36000),
        ("Australia/Sydney", 951789600, (13, 0, 0), 39600),
        ("Australia/Sydney", 1048323600, (20, 0, 0), 39600),
        ("Australia/Sydney", 1141606800, (12, 0, 0), 39600),
        ("Australia/Sydney", 1585267200, (11, 0, 0), 39600),
        ("Australia/Sydney", 1709254800, (12, 0, 0), 39600),
        ("Australia/Sydney", 2147483647, (14, 14, 7), 39600),
        ("Australia/Sydney", 2147483648, (14, 14, 8), 39600),
        ("Australia/Sydney", 3000000000, (16, 20, 0), 39600),
        ("Australia/Sydney", 4102444800, (11, 0, 0), 39600),
        ("Australia/Sydney", 1520298000, (12, 0, 0), 39600),
        ("Asia/Kathmandu", 0, (5, 30, 0), 19800),
        ("Asia/Kathmandu", 300000000, (10, 50, 0), 19800),
        ("Asia/Kathmandu", 951789600, (7, 45, 0), 20700),
        ("Asia/Kathmandu", 1048323600, (14, 45, 0), 20700),
        ("Asia/Kathmandu", 1141606800, (6, 45, 0), 20700),
        ("Asia/Kathmandu", 1585267200, (5, 45, 0), 20700),
        ("Asia/Kathmandu", 1709254800, (6, 45, 0), 20700),
        ("Asia/Kathmandu", 2147483647, (8, 59, 7), 20700),
        ("Asia/Kathmandu", 2147483648, (8, 59, 8), 20700),
        ("Asia/Kathmandu", 3000000000, (11, 5, 0), 20700),
        ("Asia/Kathmandu", 4102444800, (5, 45, 0), 20700),
        ("Asia/Kathmandu", 1520298000, (6, 45, 0), 20700),
        ("Pacific/Chatham", 0, (12, 45, 0), 45900),
        ("Pacific/Chatham", 300000000, (18, 5, 0), 45900),
        ("Pacific/Chatham", 951789600, (15, 45, 0), 49500),
        ("Pacific/Chatham", 1048323600, (21, 45, 0), 45900),
        ("Pacific/Chatham", 1141606800, (14, 45, 0), 49500),
        ("Pacific/Chatham", 1585267200, (13, 45, 0), 49500),
        ("Pacific/Chatham", 1709254800, (14, 45, 0), 49500),
        ("Pacific/Chatham", 2147483647, (16, 59, 7), 49500),
        ("Pacific/Chatham", 2147483648, (16, 59, 8), 49500),
        ("Pacific/Chatham", 3000000000, (19, 5, 0), 49500),
        ("Pacific/Chatham", 4102444800, (13, 45, 0), 49500),
        ("Pacific/Chatham", 1520298000, (14, 45, 0), 49500),
        ("UTC", 0, (0, 0, 0), 0),
        ("UTC", 300000000, (5, 20, 0), 0),
        ("UTC", 951789600, (2, 0, 0), 0),
        ("UTC", 1048323600, (9, 0, 0), 0),
        ("UTC", 1141606800, (1, 0, 0), 0),
        ("UTC", 1585267200, (0, 0, 0), 0),
        ("UTC", 1709254800, (1, 0, 0), 0),
        ("UTC", 2147483647, (3, 14, 7), 0),
        ("UTC", 2147483648, (3, 14, 8), 0),
        ("UTC", 3000000000, (5, 20, 0), 0),
        ("UTC", 4102444800, (0, 0, 0), 0),
        ("UTC", 1520298000, (1, 0, 0), 0),
    ];

    #[test]
    fn tzif_matches_reference_implementation() {
        if !std::path::Path::new("/usr/share/zoneinfo/UTC").exists() {
            eprintln!("跳过：本机无 tzdata");
            return;
        }
        for (zone_name, epoch, hms, off) in VECTORS {
            let zone = Zone::load(format!("/usr/share/zoneinfo/{zone_name}"))
                .unwrap_or_else(|| panic!("{zone_name} 解析失败"));
            assert_eq!(zone.offset_at(*epoch), *off, "{zone_name} @{epoch} 偏移");
            assert_eq!(zone.hms(*epoch), *hms, "{zone_name} @{epoch} 时分秒");
        }
    }

    #[test]
    fn posix_rule_without_files() {
        // TZ=EST5EDT 这类纯规则值：不读任何文件也要算对
        let est = parse_posix("EST5EDT,M3.2.0,M11.1.0").expect("EST5EDT");
        assert_eq!(est.std_off, -18000);
        // 2024-07-04 12:00 UTC → EDT
        assert_eq!(est.offset_at(1_720_080_000), -14400);
        // 2024-01-04 12:00 UTC → EST
        assert_eq!(est.offset_at(1_704_374_400), -18000);

        let cst = parse_posix("CST-8").expect("CST-8");
        assert_eq!(cst.offset_at(0), 28800);
        // 无 DST 段时不该有夏令时
        assert_eq!(cst.dst, None);

        // 名字里带尖括号、偏移带分钟
        let odd = parse_posix("<+05>-5:30").expect("+05:30");
        assert_eq!(odd.offset_at(0), 19800);
    }

    #[test]
    fn default_posix_rule_is_us_style() {
        // 省略切换点时按 RFC 8536 用美国规则：3 月最后一个周日、11 月第一个周日
        let r = parse_posix("EST5EDT").expect("EST5EDT 无显式规则");
        assert_eq!(r.offset_at(1_720_080_000), -14400); // 7 月
        assert_eq!(r.offset_at(1_704_374_400), -18000); // 1 月
    }

    #[test]
    fn julian_and_ordinal_day_numbering() {
        // Jn 不计闰日：J60 在平年和闰年都落在 3 月 1 日
        let date_of = |n: u32, year: i64| {
            let doy = DayOfYear::Julian(n).second_of_year(year) / SECS_PER_DAY;
            civil_from_days(days_from_civil(year, 1, 1) + doy)
        };
        assert_eq!(date_of(60, 2023), (2023, 3, 1));
        assert_eq!(date_of(60, 2024), (2024, 3, 1));
        assert_eq!(date_of(59, 2024), (2024, 2, 28));
        // n 计闰日：同一个 n 在闰年比平年早一天
        let ordinal = |n: u32, year: i64| {
            let doy = DayOfYear::Ordinal(n).second_of_year(year) / SECS_PER_DAY;
            civil_from_days(days_from_civil(year, 1, 1) + doy)
        };
        assert_eq!(ordinal(60, 2024), (2024, 3, 1));
        assert_eq!(ordinal(60, 2023), (2023, 3, 2));
    }

    #[test]
    fn last_weekday_of_month() {
        // 2024-03 的最后一个周日是 31 日 → M3.5.0 的月内日序 30（0 基）
        assert_eq!(nth_weekday_of_month(2024, 3, 5, 0), 30);
        // 2024-11 的第一个周日是 3 日 → 日序 2
        assert_eq!(nth_weekday_of_month(2024, 11, 1, 0), 2);
        // 1970-01-01 是星期四
        assert_eq!(weekday_from_days(0), 4);
    }

    #[test]
    fn civil_day_roundtrip() {
        let mut d = -40_000i64;
        while d < 40_000 {
            let (y, m, day) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, day), d, "{y}-{m}-{day}");
            d += 137;
        }
        assert!(is_leap(2024) && !is_leap(2023) && is_leap(2000) && !is_leap(1900));
    }

    #[test]
    fn hms_stays_in_range_across_epoch_edges() {
        let zone = Zone::load("/etc/localtime").unwrap_or_else(Zone::utc);
        for epoch in [-1_000_000i64, 0, 1, 1_000_000_000, 4_000_000_000] {
            let (h, m, s) = zone.hms(epoch);
            assert!(h < 24 && m < 60 && s < 60, "{epoch} → {h}:{m}:{s}");
        }
    }

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
