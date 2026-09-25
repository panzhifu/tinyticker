//! 系统状态采样：读 `/proc` 与 `/sys` 下的纯文本，不引入任何第三方 crate。
//!
//! 解析函数与 IO 分开：`parse_*` 只吃 `&str`，因此单测不需要真机状态。

use std::fs;
use std::time::{Duration, Instant};

const STAT: &str = "/proc/stat";
const MEMINFO: &str = "/proc/meminfo";
const POWER_SUPPLY: &str = "/sys/class/power_supply";
const NET_DEV: &str = "/proc/net/dev";
const UPTIME: &str = "/proc/uptime";

/// 一次采样的结果，百分比都是 0-100。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sources {
    pub cpu: u8,
    pub mem: u8,
    /// `None` = 这台机器没有电池（台式机 / 容器）。
    pub battery: Option<Battery>,
    /// 下行 / 上行字节每秒，所有非 loopback 网卡之和。
    pub net_down: u64,
    pub net_up: u64,
    /// 开机至今的秒数；读不到（奇异的容器）给 0，悬停提示据它省略那行。
    pub uptime_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// CPU 与网络速率都只能靠两次采样的差分算出，所以采样器自己留着上一次的计数。
pub struct Sampler {
    prev: Option<(u64, u64)>,
    cpu: u8,
    /// 上一次读到的网卡累计 `(接收, 发送)` 字节，以及读到它的时刻。
    prev_net: Option<(u64, u64)>,
    net_at: Instant,
    net_down: u64,
    net_up: u64,
    battery_at: Instant,
    /// 电池变化以分钟计，读勤了只是浪费 syscall。
    battery_every: Duration,
    battery: Option<Battery>,
}

impl Sampler {
    /// 立刻取一次 CPU 与网卡计数做基准，第一次 `sample` 就能给出真实占用。
    pub fn new() -> Self {
        let mut s = Self {
            prev: None,
            cpu: 0,
            prev_net: read_netdev(),
            net_at: Instant::now(),
            net_down: 0,
            net_up: 0,
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
        if let Some(cur) = read_netdev() {
            // 速率按**真实间隔**算而不是按节拍假定：采样挂在派发循环上，两拍之间
            // 会有零点几秒的抖动，写死 1 秒会让读数跟着抖
            let ms = self.net_at.elapsed().as_millis().max(1) as u64;
            if let Some((prev_rx, prev_tx)) = self.prev_net {
                self.net_down = bytes_per_sec(cur.0, prev_rx, ms);
                self.net_up = bytes_per_sec(cur.1, prev_tx, ms);
            }
            self.prev_net = Some(cur);
            self.net_at = Instant::now();
        }
        if self.battery_at.elapsed() >= self.battery_every {
            self.battery_at = Instant::now();
            self.battery = read_battery();
        }
        Sources {
            cpu: self.cpu,
            mem: read_to_string(MEMINFO)
                .as_deref()
                .and_then(parse_meminfo)
                .unwrap_or(0),
            battery: self.battery,
            net_down: self.net_down,
            net_up: self.net_up,
            uptime_secs: read_uptime().unwrap_or(0),
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

/// `/proc/uptime` 首 token：`123.45` 这种浮点秒，只取整数部分。
fn parse_uptime(text: &str) -> Option<u64> {
    text.split_whitespace()
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn read_uptime() -> Option<u64> {
    parse_uptime(&read_to_string(UPTIME)?)
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

/// 差分速率（字节/秒）。计数器回绕（重启 / 网卡重置）时差分为负，按 0 处理，与 CPU 同口径。
fn bytes_per_sec(cur: u64, prev: u64, ms: u64) -> u64 {
    cur.saturating_sub(prev) * 1000 / ms.max(1)
}

/// `/proc/net/dev` → 非 loopback 网卡的累计 `(接收, 发送)` 字节。
fn read_netdev() -> Option<(u64, u64)> {
    parse_netdev(&read_to_string(NET_DEV)?)
}

/// `/proc/net/dev` 文本 → 累计字节。
///
/// 每行形如 `wlan0: 12345 67 0 0 ... | 9999 12 ...`，冒号后先是 Receive 的 8 列
/// （bytes 打头）再是 Transmit 的 8 列，所以 rx 取第 0 列、tx 取第 8 列。表头那两行
/// 不含冒号，天然被跳过；只排 loopback，与 Catime 的口径一致。
fn parse_netdev(text: &str) -> Option<(u64, u64)> {
    let mut rx = 0u64;
    let mut tx = 0u64;
    let mut seen = false;
    for line in text.lines() {
        let Some((name, cols)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let cols: Vec<&str> = cols.split_whitespace().collect();
        if cols.len() < 9 {
            continue;
        }
        let (Ok(r), Ok(t)) = (cols[0].parse::<u64>(), cols[8].parse::<u64>()) else {
            continue;
        };
        rx += r;
        tx += t;
        seen = true;
    }
    // 一块网卡都没读到（容器里没有 /proc/net/dev 的权限）时报 None，让调用方留着上次的值
    seen.then_some((rx, tx))
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
        let (busy, total) =
            parse_cpu_line("cpu  1000 0 200 7000 300 50 50 0 9 9\ncpu0 1 2 3\n").unwrap();
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

    /// 网卡累计字节：排掉 loopback，取 rx 第 0 列 / tx 第 8 列，junk 行整行不采信。
    #[test]
    fn netdev_sums_interfaces_and_skips_loopback() {
        let text = "Inter-|   Receive                                                |  Transmit\n \
face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n \
   lo:  111111    890    0    0    0     0          0         0   222222    890    0    0    0     0       0          0\n \
 wlan0: 1000000    890    0    0    0     0          0         0    50000    120    0    0    0     0       0          0\n \
  veth1:  250000    100    0    0    0     0          0         0    10000    100    0    0    0     0       0          0\n";
        assert_eq!(parse_netdev(text), Some((1_250_000, 60_000)));
        // 只有 loopback / 空文件 / 列数不足 / 数字栏是 junk：都没有可信读数
        assert_eq!(
            parse_netdev("    lo: 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1 1\n"),
            None
        );
        assert_eq!(parse_netdev(""), None);
        assert_eq!(parse_netdev("  eth0: 1 2 3\n"), None);
        assert_eq!(
            parse_netdev(" eth0: bytes packets errs drop fifo frame compressed multicast bytes\n"),
            None
        );
    }

    /// 速率按真实间隔折算；计数器回绕（重启 / 网卡重置）报 0 而不是负数。
    #[test]
    fn net_rate_uses_the_real_interval() {
        assert_eq!(bytes_per_sec(1000, 0, 1000), 1000);
        assert_eq!(
            bytes_per_sec(500, 0, 500),
            1000,
            "半秒读到 500B 该报 1000B/s"
        );
        assert_eq!(bytes_per_sec(0, 5000, 1000), 0);
        assert_eq!(
            bytes_per_sec(100, 0, 0),
            100_000,
            "间隔下限取 1ms，不许除零"
        );
    }

    #[test]
    fn meminfo_uses_mem_available() {
        let text =
            "MemTotal:       16384000 kB\nMemFree:   1024000 kB\nMemAvailable:   4096000 kB\n";
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
