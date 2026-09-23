//! 系统状态采样：读 `/proc` 与 `/sys` 下的纯文本，不引入任何第三方 crate。
//!
//! 解析函数与 IO 分开：`parse_*` 只吃 `&str`，因此单测不需要真机状态。

use std::fs;
use std::time::{Duration, Instant};

const STAT: &str = "/proc/stat";
const MEMINFO: &str = "/proc/meminfo";
const POWER_SUPPLY: &str = "/sys/class/power_supply";

/// 一次采样的结果，百分比都是 0-100。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sources {
    pub cpu: u8,
    pub mem: u8,
    /// `None` = 这台机器没有电池（台式机 / 容器）。
    pub battery: Option<Battery>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// CPU 占用只能靠两次采样的差分算出，所以采样器自己留着上一次的计数。
pub struct Sampler {
    prev: Option<(u64, u64)>,
    cpu: u8,
    battery_at: Instant,
    /// 电池变化以分钟计，读勤了只是浪费 syscall。
    battery_every: Duration,
    battery: Option<Battery>,
}

impl Sampler {
    /// 立刻取一次 CPU 计数做基准，第一次 `sample` 就能给出真实占用。
    pub fn new() -> Self {
        let mut s = Self {
            prev: None,
            cpu: 0,
            battery_at: Instant::now(),
            battery_every: Duration::from_secs(30),
            battery: read_battery(),
        };
        s.prev = read_cpu_times();
        s
    }

    pub fn sample(&mut self) -> Sources {
        if let Some(cur) = read_cpu_times() {
            self.cpu = match self.prev {
                Some((prev_busy, prev_total)) => delta_percent(cur, prev_busy, prev_total),
                None => 0,
            };
            self.prev = Some(cur);
        }
        if self.battery_at.elapsed() >= self.battery_every {
            self.battery_at = Instant::now();
            self.battery = read_battery();
        }
        Sources {
            cpu: self.cpu,
            mem: read_to_string(MEMINFO).as_deref().and_then(parse_meminfo).unwrap_or(0),
            battery: self.battery,
        }
    }
}

fn read_to_string(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn read_cpu_times() -> Option<(u64, u64)> {
    let text = read_to_string(STAT)?;
    parse_cpu_line(&text)
}

/// `/proc/stat` 首行的 cpu 聚合字段 → `(busy, total)`。
///
/// 字段序：user nice system idle iowait irq softirq steal guest guest_nice。
/// `total` 只加前 8 项：内核已把 guest 时间计入 user/nice，再加一次会重复。
fn parse_cpu_line(text: &str) -> Option<(u64, u64)> {
    let fields = text
        .lines()
        .find_map(|l| l.strip_prefix("cpu "))?
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    // 2.6.11 之前没有 iowait 及之后的字段；缺的按 0 算，user/nice/system/idle 总是存在
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().take(8).sum();
    Some((total.saturating_sub(idle), total))
}

/// 两次采样的差分；计数器回绕或机器重启时总时间不增，此时按 0 处理。
fn delta_percent(cur: (u64, u64), prev_busy: u64, prev_total: u64) -> u8 {
    let d_total = cur.1.saturating_sub(prev_total);
    if d_total == 0 {
        return 0;
    }
    let d_busy = cur.0.saturating_sub(prev_busy).min(d_total);
    ((d_busy * 100) / d_total) as u8
}

/// `/proc/meminfo` → 占用百分比，按 `MemTotal - MemAvailable` 算。
/// 老内核没有 `MemAvailable` 时不猜，直接 0。
fn parse_meminfo(text: &str) -> Option<u8> {
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))?
            .trim_start_matches(':')
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    };
    let total = value("MemTotal")?;
    let available = value("MemAvailable")?;
    if total == 0 || available > total {
        return None;
    }
    Some((((total - available) * 100) / total) as u8)
}

/// 汇总 `/sys/class/power_supply` 下的电池；多块取平均，任一块在充电就算充电。
fn read_battery() -> Option<Battery> {
    let mut sum = 0u64;
    let mut n = 0u64;
    let mut charging = false;
    for entry in fs::read_dir(POWER_SUPPLY).ok()?.flatten() {
        let dir = entry.path();
        let kind = fs::read_to_string(dir.join("type")).unwrap_or_default();
        if !kind.trim().eq_ignore_ascii_case("battery") {
            continue;
        }
        let Some(percent) = fs::read_to_string(dir.join("capacity"))
            .ok()
            .and_then(|t| parse_capacity(&t))
        else {
            continue;
        };
        sum += u64::from(percent);
        n += 1;
        let status = fs::read_to_string(dir.join("status")).unwrap_or_default();
        charging |= status.trim().eq_ignore_ascii_case("charging");
    }
    (n > 0).then_some(Battery {
        percent: (sum / n) as u8,
        charging,
    })
}

fn parse_capacity(text: &str) -> Option<u8> {
    let v: u8 = text.trim().parse().ok()?;
    (v <= 100).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_line_sums_only_the_first_eight_fields() {
        // total = 1000+0+200+7000+300+50+50+0 = 8600（末尾两个 guest 不计，已含在 user/nice 里）
        // busy  = total - idle - iowait = 8600 - 7000 - 300 = 1300
        let (busy, total) = parse_cpu_line("cpu  1000 0 200 7000 300 50 50 0 9 9\ncpu0 1 2 3\n").unwrap();
        assert_eq!(total, 8600);
        assert_eq!(busy, 1300);
    }

    #[test]
    fn cpu_line_tolerates_pre_2_6_11_field_counts() {
        // 只有 4 个字段的老内核：没有 iowait
        let (busy, total) = parse_cpu_line("cpu 100 10 20 200").unwrap();
        assert_eq!((busy, total), (130, 330));
    }

    #[test]
    fn cpu_line_rejects_garbage() {
        assert!(parse_cpu_line("").is_none());
        assert!(parse_cpu_line("intr 123").is_none());
        assert!(parse_cpu_line("cpu 1 2 x 4").is_none());
        assert!(parse_cpu_line("cpu 1 2 3").is_none());
    }

    #[test]
    fn cpu_delta_clamps_and_handles_restart() {
        assert_eq!(delta_percent((500, 1000), 100, 500), 80);
        // 计数器回绕：total 没增长 → 报 0 而不是负数
        assert_eq!(delta_percent((10, 20), 100, 500), 0);
        // busy 增量超过 total 增量（不可能，但要夹紧）
        assert_eq!(delta_percent((900, 1000), 0, 990), 100);
    }

    #[test]
    fn meminfo_uses_mem_available() {
        let text = "MemTotal:       16384000 kB\nMemFree:   1024000 kB\nMemAvailable:   4096000 kB\n";
        // (16384000 - 4096000) / 16384000 = 75%
        assert_eq!(parse_meminfo(text), Some(75));
    }

    #[test]
    fn meminfo_needs_both_fields() {
        assert!(parse_meminfo("MemTotal: 100 kB\n").is_none());
        // available 大于 total 说明读到了撕裂的数据，不猜
        assert!(parse_meminfo("MemTotal: 100 kB\nMemAvailable: 200 kB\n").is_none());
        assert!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_none());
    }

    #[test]
    fn capacity_is_bounded() {
        assert_eq!(parse_capacity("87\n"), Some(87));
        assert_eq!(parse_capacity("101"), None);
        assert_eq!(parse_capacity("abc"), None);
    }
}
