//! 计时状态机：倒计时 / 秒表 / 番茄钟 / 时钟挂件。
//! 整秒是状态（[`Timer::secs`]），不足一秒的余量也是状态（[`Timer::cs`]），
//! 两者由同一次 [`Timer::tick_at`] 采样推进，所以显示到百分之一秒不会与秒位错帧。

use std::cell::Cell;
use std::time::{Duration, Instant};

/// 计时模式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 倒计时：从 `total` 递减，归零自动停止并标记结束。
    Countdown,
    /// 秒表：从 0 递增，不自动停止。
    Stopwatch,
    /// 番茄钟：专注/休息自动轮转，每阶段结束时通知。
    Pomodoro,
    /// 时钟挂件：实时显示本地时间（绘制时读取，不计时）。
    Clock,
}

impl Mode {
    /// 配置文件 / 命令行中的模式名。
    pub fn from_name(name: &str) -> Option<Mode> {
        match name {
            "countdown" => Some(Mode::Countdown),
            "stopwatch" => Some(Mode::Stopwatch),
            "pomodoro" => Some(Mode::Pomodoro),
            "clock" => Some(Mode::Clock),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Countdown => "countdown",
            Mode::Stopwatch => "stopwatch",
            Mode::Pomodoro => "pomodoro",
            Mode::Clock => "clock",
        }
    }
}

/// 番茄钟阶段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Work,
    /// 短休息：常规轮次之间。
    Break,
    /// 长休息：跑满 `Pomo::rounds` 轮专注后插入。
    LongBreak,
}

/// 番茄钟的节奏参数（配置项 `pomo_*` 喂进来）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pomo {
    /// 专注时长（秒）。
    pub work: u32,
    /// 短休息时长（秒）。
    pub short_break: u32,
    /// 长休息时长（秒）；0 = 不安排长休息，退回每轮短休息。
    pub long_break: u32,
    /// 每几轮专注后安排一次长休息，即一「组」的轮数。
    pub rounds: u32,
    /// 跑满几组后自动结束；0 = 不限，一直轮转。
    pub cycles: u32,
    /// 任意段序列（配置项 `pomo_seq`）。非空就**整条取代**上面那四个键的节奏：
    /// 一段接一段跑下去，段与段之间只发"这一段结束"，跑完 `cycles` 遍才算收工。
    pub seq: Vec<u32>,
}

impl Pomo {
    /// 走的是 `pomo_seq` 那条路吗（序列非空就整条接管节奏）。
    pub fn is_seq(&self) -> bool {
        !self.seq.is_empty()
    }

    /// 第 `i` 段的时长。序列空时（经典配方）退回专注时长，让 `reset` 一处代码管两条路。
    pub fn step_secs(&self, i: usize) -> u32 {
        self.seq.get(i).copied().unwrap_or(self.work.max(1))
    }
}

/// `pomo_seq` 的段数上限。Catime 给的是 10（`MAX_POMO_TIMES`），我们放到 16——
/// 再多状态行那个 `POMO n/N` 就念不清了，而且这已经是"自己排节奏"而不是番茄钟。
pub const MAX_POMO_STEPS: usize = 16;

impl Default for Pomo {
    fn default() -> Self {
        Self {
            work: 1500,
            short_break: 300,
            long_break: 900,
            rounds: 4,
            cycles: 0,
            seq: Vec::new(),
        }
    }
}

/// 一次"计时结束"事件的来源（决定通知文案与是否执行 on_finish 命令）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Finished {
    /// 倒计时归零。
    Countdown,
    /// 番茄钟专注阶段完成。
    PomodoroWork,
    /// `pomo_seq` 那一档：中间某一段跑完（不是收工）。
    PomodoroStep,
    /// 短休息结束。
    PomodoroBreak,
    /// 长休息结束，进入下一组。
    PomodoroLongBreak,
    /// 跑满设定的组数，整个番茄钟收工。
    PomodoroAllDone,
}

/// 计时器核心状态。
pub struct Timer {
    pub mode: Mode,
    /// 倒计时总时长（秒）；秒表模式下保留最近一次的倒计时值。
    pub total: u32,
    /// 倒计时为剩余秒数，秒表为已进行秒数；时钟模式不使用。
    pub secs: u32,
    /// 距 `secs` 那一格又走过去了几百分之一秒（0-99）。
    ///
    /// 只在运行时累计，所以这一格会被原样冻住：暂停前显示 `9.30`，暂停期间一直是
    /// `9.30`，恢复后从 `9.29` 接着走。
    cs: u32,
    pub running: bool,
    finished: Option<Finished>, // 每次结束置位一次，由 take_finished 消费
    last_tick: Cell<Option<Instant>>,
    // 番茄钟状态
    phase: Phase,
    /// 已完成的专注轮数（跨组累加，组内位置由 `rounds` 取模得出）。
    round: u32,
    /// `pomo_seq` 那条路：当前是第几段（0 起）。
    step: usize,
    /// `pomo_seq` 那条路：已经完整跑完几遍。
    sets: u32,
    pomo: Pomo,
    /// 跑满 `pomo.cycles` 组后置位，直到 reset / start 才清。
    cycles_done: bool,
}

impl Timer {
    pub fn new(mode: Mode, total: u32) -> Self {
        let mut t = Self {
            mode,
            total,
            secs: 0,
            cs: 0,
            running: false,
            finished: None,
            last_tick: Cell::new(None),
            phase: Phase::Work,
            round: 0,
            step: 0,
            sets: 0,
            pomo: Pomo::default(),
            cycles_done: false,
        };
        t.reset();
        t
    }

    /// 当前显示的秒数（倒计时为剩余，秒表为已进行）。
    pub fn display_secs(&self) -> u32 {
        self.secs
    }

    /// 当前显示的百分之一秒数（含整秒部分），供带百分秒的格式化用。
    ///
    /// 与 [`Timer::display_secs`] 取自同一份状态，所以秒位与百分位不可能一个已经进位、
    /// 另一个还没跟上。显示百分秒时倒计时读的是**向下取整**的剩余量（`9.30` 而不是
    /// `10`）——隐藏百分秒时才回到 `secs` 那一格，两种读法本来就不该相等。
    pub fn display_centis(&self) -> u32 {
        let whole = self.secs.saturating_mul(100);
        match self.mode {
            Mode::Stopwatch => whole.saturating_add(self.cs),
            Mode::Countdown | Mode::Pomodoro => whole.saturating_sub(self.cs),
            // 时钟模式显示的是实时读到的系统时间，与计时状态无关
            Mode::Clock => whole,
        }
    }

    /// 计时是否已收工：倒计时归零，或番茄钟跑满设定的组数。
    pub fn is_done(&self) -> bool {
        match self.mode {
            Mode::Countdown => self.secs == 0,
            Mode::Pomodoro => self.cycles_done,
            _ => false,
        }
    }

    pub fn start(&mut self) {
        // 倒计时已归零：先重置再开始
        if self.is_done() {
            self.reset();
        }
        self.running = true;
    }

    pub fn pause(&mut self) {
        self.running = false;
    }

    pub fn reset(&mut self) {
        match self.mode {
            Mode::Countdown => self.secs = self.total,
            Mode::Stopwatch | Mode::Clock => self.secs = 0,
            Mode::Pomodoro => {
                self.phase = Phase::Work;
                self.secs = self.pomo.step_secs(0);
                self.round = 0;
                self.step = 0;
                self.sets = 0;
                self.cycles_done = false;
            }
        }
        self.running = false;
        self.cs = 0;
        self.finished = None;
        self.last_tick.set(None);
    }

    /// 设置倒计时总时长，重置并立即开始（快速预设的语义）。
    pub fn start_countdown(&mut self, total: u32) {
        self.mode = Mode::Countdown;
        self.total = total;
        self.reset();
        self.running = true;
    }

    /// 切换模式（重置计时）。
    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.reset();
    }

    /// 套用番茄钟节奏；番茄钟模式下重置。
    ///
    /// 时长与轮数下限取 1：为 0 会让阶段瞬间翻过去、一次心跳里发出多个结束事件。
    /// `long_break` / `cycles` 的 0 是有意的开关（不安排长休息 / 不限组数），原样保留。
    pub fn set_pomo(&mut self, pomo: &Pomo) {
        // 序列里每一段都要 ≥1 秒：0 会让一段瞬间翻过去、一次心跳里发出多个结束事件
        self.pomo = Pomo {
            work: pomo.work.max(1),
            short_break: pomo.short_break.max(1),
            long_break: pomo.long_break,
            rounds: pomo.rounds.max(1),
            cycles: pomo.cycles,
            seq: pomo.seq.iter().map(|s| (*s).max(1)).collect(),
        };
        if self.mode == Mode::Pomodoro {
            self.reset();
        }
    }

    /// 状态行要显示的番茄阶段标签与序号。
    ///
    /// 标签固定 ASCII——渲染器只有 8x8 位图字体。序号数的是组内第几轮，
    /// 长休息显示第几组，所以跑满一轮长休息后又会回到 `WORK 1`。
    pub fn pomo_status(&self) -> (&'static str, u32) {
        if self.pomo.is_seq() {
            // 序列那一档没有"专注/休息"的语义可言，段号就是它的进度
            return ("POMO", (self.step + 1) as u32);
        }
        let rounds = self.pomo.rounds;
        // 进入休息时本轮专注已计入 round，故组内位置要往前挪一格
        let done = self.round.saturating_sub(1);
        match self.phase {
            Phase::Work => ("WORK", self.round % rounds + 1),
            Phase::Break => ("BREAK", done % rounds + 1),
            Phase::LongBreak => ("LONG", done / rounds + 1),
        }
    }

    /// `pomo_seq` 那条路：当前段号（0 起）；不在序列番茄钟上就是 `None`。
    /// 托盘菜单拿它给"当前段"打勾（GAP §四没收掉的托盘那半边）。
    pub fn pomo_step(&self) -> Option<usize> {
        (self.mode == Mode::Pomodoro && self.pomo.is_seq()).then_some(self.step)
    }

    /// 跳到序列里的第 `i` 段：读数换成本段的时长，运行与否原样保持。
    /// 越界返回 `false` 且什么都不改。收工后跳回去等于再跑一遍当前这轮。
    pub fn goto_step(&mut self, i: usize) -> bool {
        if self.pomo_step().is_none() || i >= self.pomo.seq.len() {
            return false;
        }
        self.step = i;
        self.secs = self.pomo.step_secs(i);
        self.cs = 0;
        self.cycles_done = false;
        self.finished = None;
        self.last_tick.set(None);
        true
    }

    /// `pomo_seq` 那条路：一段跑完进下一段，跑完整个序列算一组。
    ///
    /// 中间段只发 [`Finished::PomodoroStep`]——它不算"计时结束"，所以既不触发
    /// `on_finish`，也不消费一次性的 `--and`（与经典配方的休息结束同一处理）。
    fn tick_seq(&mut self) {
        self.step += 1;
        if self.step >= self.pomo.seq.len() {
            self.step = 0;
            self.sets += 1;
            if self.pomo.cycles > 0 && self.sets >= self.pomo.cycles {
                self.running = false;
                self.cycles_done = true;
                self.secs = 0;
                self.finished = Some(Finished::PomodoroAllDone);
                return;
            }
        }
        self.secs = self.pomo.step_secs(self.step);
        self.finished = Some(Finished::PomodoroStep);
    }

    /// 取走"计时结束"事件；每次结束只返回一次。
    pub fn take_finished(&mut self) -> Option<Finished> {
        self.finished.take()
    }

    fn tick(&mut self) {
        if !self.running {
            return;
        }
        match self.mode {
            Mode::Countdown => {
                if self.secs > 0 {
                    self.secs -= 1;
                    if self.secs == 0 {
                        self.running = false;
                        self.finished = Some(Finished::Countdown);
                    }
                }
            }
            Mode::Pomodoro if self.pomo.is_seq() => {
                if self.secs > 0 {
                    self.secs -= 1;
                }
                if self.secs == 0 {
                    self.tick_seq();
                }
            }
            Mode::Pomodoro => {
                if self.secs > 0 {
                    self.secs -= 1;
                }
                if self.secs > 0 {
                    return;
                }
                // 阶段结束：通知一次并自动进入下一阶段（保持运行）
                let rounds = self.pomo.rounds;
                match self.phase {
                    Phase::Work => {
                        self.round += 1;
                        self.finished = Some(Finished::PomodoroWork);
                        // 组界上插长休息；关掉长休息时短休息就是组界
                        if self.round.is_multiple_of(rounds) && self.pomo.long_break > 0 {
                            self.phase = Phase::LongBreak;
                            self.secs = self.pomo.long_break;
                        } else {
                            self.phase = Phase::Break;
                            self.secs = self.pomo.short_break;
                        }
                    }
                    Phase::Break | Phase::LongBreak => {
                        let long = self.phase == Phase::LongBreak;
                        // 长休息自己就是组界；关掉了就看 round 是否凑满一轮
                        let at_boundary = long || self.round.is_multiple_of(rounds);
                        // round 只算已完成专注，整除时正好是已跑完的组数
                        let sets = self.round / rounds;
                        if at_boundary && self.pomo.cycles > 0 && sets >= self.pomo.cycles {
                            self.running = false;
                            self.cycles_done = true;
                            self.finished = Some(Finished::PomodoroAllDone);
                        } else {
                            self.phase = Phase::Work;
                            self.secs = self.pomo.work;
                            self.finished = Some(if long {
                                Finished::PomodoroLongBreak
                            } else {
                                Finished::PomodoroBreak
                            });
                        }
                    }
                }
            }
            Mode::Stopwatch => {
                self.secs = self.secs.saturating_add(1);
            }
            // 时钟在绘制时实时读取系统时间，无需推进
            Mode::Clock => {}
        }
    }

    /// 距上次调用满 1 秒时推进（挂在重绘事件上调用）。
    pub fn maybe_tick(&mut self) {
        self.tick_at(Instant::now());
    }

    /// 推进到给定时刻；`maybe_tick` 注入真实时间，测试里注入假时间。
    pub(crate) fn tick_at(&mut self, now: Instant) {
        let Some(last) = self.last_tick.get() else {
            self.last_tick.set(Some(now)); // 首次调用仅建立基准
            return;
        };
        // 停住时只重新对基准而不累计，于是暂停期间 secs 和 cs 都原样冻着，
        // 恢复后正好接上暂停前那一格
        if !self.running {
            self.last_tick.set(Some(now));
            return;
        }
        // 只把"整百分秒"那段算进状态，基准也只前进那么多——不足一百分秒的零头留在
        // 基准里。心跳周期不是 10ms 的整数倍（poll 的超时按毫秒取整，真实周期约
        // 19.5ms），要是每次都对到 now，被截掉的那点就会每格都丢，读数系统性偏慢。
        let centis = (now.duration_since(last).as_micros() / 10_000) as u64;
        if centis == 0 {
            return;
        }
        self.last_tick
            .set(Some(last + Duration::from_micros(centis * 10_000)));
        // 一次采样的差值同时喂给百分位与秒位：停顿（休眠恢复、高负载）期间走过的
        // 整秒也在这里一次补齐
        let total = self.cs as u64 + centis;
        self.cs = (total % 100) as u32;
        let mut whole = total / 100;
        while whole > 0 {
            self.tick();
            whole -= 1;
            if !self.running {
                break; // 倒计时已归零，无需继续补齐
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把计时推到 `t0` 之后 `secs` 秒（闭包做不到：它会把 `t` 借走）。
    fn advance(t: &mut Timer, t0: Instant, secs: u64) {
        t.tick_at(t0 + Duration::from_secs(secs));
    }

    #[test]
    fn countdown_decreases_and_finishes_once() {
        let mut t = Timer::new(Mode::Countdown, 3);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(1));
        assert_eq!(t.secs, 2);
        t.tick_at(t0 + Duration::from_secs(2));
        assert_eq!(t.secs, 1);
        t.tick_at(t0 + Duration::from_secs(3));
        assert_eq!(t.secs, 0);
        assert!(!t.running);
        assert_eq!(t.take_finished(), Some(Finished::Countdown));
        assert_eq!(t.take_finished(), None); // 只通知一次
    }

    #[test]
    fn countdown_restart_after_done_resets() {
        let mut t = Timer::new(Mode::Countdown, 60);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(60));
        assert_eq!(t.secs, 0);
        t.start(); // 归零后开始 → 自动重置
        assert_eq!(t.secs, 60);
        assert!(t.running);
    }

    #[test]
    fn countdown_pause_freezes() {
        let mut t = Timer::new(Mode::Countdown, 10);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(4));
        t.pause();
        t.tick_at(t0 + Duration::from_secs(9));
        assert_eq!(t.secs, 6);
    }

    #[test]
    fn stopwatch_counts_up() {
        let mut t = Timer::new(Mode::Stopwatch, 0);
        assert_eq!(t.secs, 0);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(5));
        assert_eq!(t.secs, 5);
        assert!(t.running);
        assert_eq!(t.take_finished(), None); // 秒表没有"结束"概念
    }

    /// 百分位与秒位同源：跨过整秒边界时不许跳号，也不许出现 `.100`。
    #[test]
    fn stopwatch_centiseconds_roll_into_the_second() {
        let mut t = Timer::new(Mode::Stopwatch, 0);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        assert_eq!(t.display_centis(), 0);
        t.tick_at(t0 + Duration::from_millis(500));
        assert_eq!(t.display_centis(), 50);
        t.tick_at(t0 + Duration::from_millis(1200));
        assert_eq!((t.secs, t.display_centis()), (1, 120));
        t.tick_at(t0 + Duration::from_secs(60));
        assert_eq!(t.display_centis(), 6000);
        assert_eq!(t.display_secs(), 60, "整秒读法不受百分位影响");
    }

    /// 倒计时显示向下取整的剩余量：还剩 2.3 秒读 `230`，隐藏百分秒时才回到 `3`。
    #[test]
    fn countdown_centiseconds_descend_through_the_second_boundary() {
        let mut t = Timer::new(Mode::Countdown, 3);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        assert_eq!(t.display_centis(), 300);
        t.tick_at(t0 + Duration::from_millis(700));
        assert_eq!((t.display_secs(), t.display_centis()), (3, 230));
        t.tick_at(t0 + Duration::from_millis(1000));
        assert_eq!(t.display_centis(), 200, "230 之后该是 200，不该跳号");
        t.tick_at(t0 + Duration::from_millis(2990));
        assert_eq!(t.display_centis(), 1);
        t.tick_at(t0 + Duration::from_millis(3000));
        assert_eq!(t.display_centis(), 0);
        assert_eq!(t.take_finished(), Some(Finished::Countdown));
    }

    /// 暂停要把百分位一起冻住：恢复后接着数，不回 `.00` 也不倒退。
    #[test]
    fn centiseconds_freeze_across_a_pause() {
        let mut t = Timer::new(Mode::Stopwatch, 0);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        t.tick_at(t0 + Duration::from_millis(1300));
        assert_eq!(t.display_centis(), 130);
        t.pause();
        t.tick_at(t0 + Duration::from_secs(30));
        assert_eq!(
            (t.display_secs(), t.display_centis()),
            (1, 130),
            "暂停期间读数不动"
        );
        t.start();
        t.tick_at(t0 + Duration::from_secs(31));
        assert_eq!(
            t.display_centis(),
            230,
            "该从 1.30 接着走，而不是从 2.00 重来"
        );
    }

    /// 重置要连百分位一起清掉，否则重新开始会带着上一次的余量。
    #[test]
    fn reset_clears_the_centisecond_remainder() {
        let mut t = Timer::new(Mode::Stopwatch, 0);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        t.tick_at(t0 + Duration::from_millis(750));
        assert_eq!(t.display_centis(), 75);
        t.reset();
        assert_eq!(t.display_centis(), 0);
    }

    /// 心跳周期不是 10ms 的整数倍（`poll` 的超时按毫秒取整，真实周期约 19.5ms），
    /// 每次被截掉的亚百分秒零头必须留在基准里。对到 `now` 会把零头丢掉：20 次
    /// 19.5ms 只算出 20 百分秒而不是 39，跑动中的读数会系统性慢一半。
    #[test]
    fn sub_hundredth_heartbeat_jitter_does_not_slow_the_clock() {
        let mut t = Timer::new(Mode::Stopwatch, 0);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        for i in 1..=20 {
            t.tick_at(t0 + Duration::from_micros(19_500 * i));
        }
        // 20 × 19.5ms = 390ms
        assert_eq!(t.display_centis(), 39, "零头被丢掉了 → 读数偏慢");
        for i in 21..=1000 {
            t.tick_at(t0 + Duration::from_micros(19_500 * i));
        }
        // 1000 × 19.5ms = 19.5s，一格不许漏
        assert_eq!(t.display_centis(), 1950, "长期走时漂移了");
        assert_eq!(t.display_secs(), 19);
    }

    #[test]
    fn pomodoro_cycles_phases_and_counts_rounds() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&no_long_break(2, 1));
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(1));
        assert_eq!(t.secs, 1);
        t.tick_at(t0 + Duration::from_secs(2));
        // 专注结束 → 自动进入休息
        assert_eq!(t.phase, Phase::Break);
        assert_eq!(t.secs, 1);
        assert_eq!(t.round, 1);
        assert!(t.running);
        assert_eq!(t.take_finished(), Some(Finished::PomodoroWork));
        assert_eq!(t.take_finished(), None);
        t.tick_at(t0 + Duration::from_secs(3));
        // 休息结束 → 回到专注，轮数不变
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.secs, 2);
        assert_eq!(t.round, 1);
        assert_eq!(t.take_finished(), Some(Finished::PomodoroBreak));
    }

    /// 长休息关掉时，行为与只会短休息的旧版一致。
    fn no_long_break(work: u32, short: u32) -> Pomo {
        Pomo {
            work,
            short_break: short,
            long_break: 0,
            rounds: 4,
            cycles: 0,
            seq: Vec::new(),
        }
    }

    /// 跑满 rounds 轮专注插一次长休息，并在最后一组长休息后停住。
    #[test]
    fn pomodoro_inserts_long_break_and_stops_after_last_cycle() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        // 2 轮一组、跑 1 组：W2 B1 W2 B1(长) → 停
        t.set_pomo(&Pomo {
            work: 2,
            short_break: 1,
            long_break: 3,
            rounds: 2,
            cycles: 1,
            seq: Vec::new(),
        });
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);

        advance(&mut t, t0, 2); // 第 1 轮专注结束
        assert_eq!((t.phase, t.secs), (Phase::Break, 1));
        assert_eq!(t.take_finished(), Some(Finished::PomodoroWork));
        advance(&mut t, t0, 3); // 短休息结束
        assert_eq!((t.phase, t.secs), (Phase::Work, 2));
        assert_eq!(t.take_finished(), Some(Finished::PomodoroBreak));
        advance(&mut t, t0, 5); // 第 2 轮专注结束 → 凑满一轮，长休息
        assert_eq!((t.phase, t.secs), (Phase::LongBreak, 3));
        assert_eq!(t.take_finished(), Some(Finished::PomodoroWork));
        advance(&mut t, t0, 8); // 长休息结束，且已跑满 1 组 → 收工
        assert_eq!(t.take_finished(), Some(Finished::PomodoroAllDone));
        assert!(!t.running);
        assert!(t.is_done());
        assert_eq!(t.secs, 0);
    }

    /// `cycles = 0` 是"不限"：长休息之后接着开下一组，序号回到 1。
    #[test]
    fn unlimited_pomodoro_keeps_looping_into_the_next_set() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 2,
            short_break: 1,
            long_break: 3,
            rounds: 2,
            cycles: 0,
            seq: Vec::new(),
        });
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        // 走完一整组：专注 2 + 短休 1 + 专注 2 + 长休 3 = 8 秒
        t.tick_at(t0 + Duration::from_secs(8));
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.round, 2);
        assert!(t.running);
        assert!(!t.is_done());
        assert_eq!(t.take_finished(), Some(Finished::PomodoroLongBreak));
        assert_eq!(t.pomo_status(), ("WORK", 1), "新的一组该从 1 重新数");
    }

    /// 长休息被关掉时，收工判定得挂在组界的短休息上，否则永远停不下来。
    #[test]
    fn cycles_still_bite_with_long_break_disabled() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 1,
            short_break: 1,
            long_break: 0,
            rounds: 2,
            cycles: 1,
            seq: Vec::new(),
        });
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        // W1 B1 W2 B2(组界) → 收工，共 4 秒
        t.tick_at(t0 + Duration::from_secs(3));
        assert!(t.running && !t.is_done(), "第 2 轮刚结束，短休息还没走完");
        t.tick_at(t0 + Duration::from_secs(4));
        assert_eq!(t.take_finished(), Some(Finished::PomodoroAllDone));
        assert!(t.is_done() && !t.running);
    }

    /// `pomo_seq` 那条路：一段接一段跑，跑完整个序列算一遍，跑满 `cycles` 遍收工。
    #[test]
    fn a_custom_sequence_runs_step_by_step_then_stops() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            seq: vec![3, 2, 1],
            cycles: 2,
            ..Pomo::default()
        });
        assert_eq!(
            (t.pomo_status(), t.display_secs()),
            (("POMO", 1), 3),
            "第 1 段"
        );
        t.start();
        let run = |t: &mut Timer, secs: u32| {
            for _ in 0..secs {
                t.tick();
            }
        };
        run(&mut t, 3);
        assert_eq!(t.take_finished(), Some(Finished::PomodoroStep));
        assert_eq!((t.pomo_status(), t.display_secs()), (("POMO", 2), 2));
        run(&mut t, 2);
        assert_eq!(t.take_finished(), Some(Finished::PomodoroStep));
        assert_eq!(
            (t.pomo_status(), t.display_secs()),
            (("POMO", 3), 1),
            "第 3 段"
        );
        run(&mut t, 1);
        // 序列跑完一遍：第 2 遍从第 1 段重新开始，中间段仍算"这段结束"
        assert_eq!(t.take_finished(), Some(Finished::PomodoroStep));
        assert_eq!((t.pomo_status(), t.display_secs()), (("POMO", 1), 3));
        run(&mut t, 3 + 2 + 1);
        assert_eq!(
            t.take_finished(),
            Some(Finished::PomodoroAllDone),
            "两遍跑完该收工"
        );
        assert!(t.is_done() && !t.running);
        assert_eq!(t.display_secs(), 0);
    }

    /// `cycles = 0` 是"不限遍数"，序列该一直轮转下去。
    #[test]
    fn an_endless_sequence_keeps_wrapping() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            seq: vec![1, 1],
            cycles: 0,
            ..Pomo::default()
        });
        t.start();
        for i in 0..6 {
            t.tick();
            assert_eq!(
                t.take_finished(),
                Some(Finished::PomodoroStep),
                "第 {i} 次翻段"
            );
            assert!(t.running && !t.is_done(), "不限遍数就不许自己停下");
        }
        assert!(t.pomo_status().1 <= 2, "段号不许跑出序列之外");
    }

    /// 序列里出现 0 秒段会被抬成 1 秒：瞬间翻段会让一次心跳里发出好几个结束事件。
    #[test]
    fn zero_length_steps_are_clamped_up() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            seq: vec![0, 5],
            cycles: 1,
            ..Pomo::default()
        });
        assert_eq!(t.display_secs(), 1, "0 秒段该被抬成 1 秒");
    }

    /// 托盘「番茄分段」那一串的底层：`pomo_step` 只在序列番茄钟上有值，
    /// `goto_step` 换读数但不动运行态，越界与不在序列上都得拒。
    #[test]
    fn step_jump_follows_the_tray_item() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        assert_eq!(t.pomo_step(), None, "经典配方没有\"段\"可报");
        assert!(!t.goto_step(0), "不在序列上就不许跳");
        t.set_pomo(&Pomo {
            seq: vec![30, 20, 10],
            cycles: 0,
            ..Pomo::default()
        });
        assert_eq!(t.pomo_step(), Some(0));
        assert!(t.goto_step(2));
        assert_eq!(
            (t.pomo_step(), t.display_secs()),
            (Some(2), 10),
            "读数该换成第 3 段"
        );
        assert!(!t.running, "没在跑的时候跳段不该顺手把它跑起来");
        t.start();
        assert!(t.goto_step(1));
        assert_eq!(
            (t.pomo_step(), t.display_secs(), t.running),
            (Some(1), 20, true),
            "在跑的保持跑"
        );
        assert!(!t.goto_step(9), "越界拒绝且什么都不改");
        assert_eq!((t.pomo_step(), t.display_secs()), (Some(1), 20));
        // 收工后跳回去等于再跑这一遍
        t.set_pomo(&Pomo {
            seq: vec![1],
            cycles: 1,
            ..Pomo::default()
        });
        t.start();
        t.tick();
        assert_eq!(t.take_finished(), Some(Finished::PomodoroAllDone));
        assert!(t.is_done() && !t.running);
        assert!(t.goto_step(0));
        assert_eq!(t.display_secs(), 1, "跳回去读数重新挂上本段时长");
        // 段号也能回到模式切换之外：换了模式就不是序列番茄钟
        t.set_mode(Mode::Countdown);
        assert_eq!(t.pomo_step(), None);
    }

    #[test]
    fn pomo_status_numbers_follow_the_set() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 10,
            short_break: 10,
            long_break: 10,
            rounds: 2,
            cycles: 2,
            seq: Vec::new(),
        });
        assert_eq!(t.pomo_status(), ("WORK", 1));
        t.phase = Phase::Break;
        t.round = 1;
        assert_eq!(t.pomo_status(), ("BREAK", 1));
        t.phase = Phase::LongBreak;
        t.round = 2;
        assert_eq!(t.pomo_status(), ("LONG", 1));
        t.phase = Phase::Work;
        assert_eq!(t.pomo_status(), ("WORK", 1), "第 3 轮是第 2 组的第 1 轮");
        t.phase = Phase::LongBreak;
        t.round = 4;
        assert_eq!(t.pomo_status(), ("LONG", 2));
    }

    /// 番茄钟的 `is_done` 只在跑满组数后成立，中途休息结束不算结束。
    #[test]
    fn pomodoro_is_not_done_until_cycles_run_out() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 1,
            short_break: 1,
            long_break: 0,
            rounds: 2,
            cycles: 1,
            seq: Vec::new(),
        });
        assert!(!t.is_done());
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        t.tick_at(t0 + Duration::from_secs(1));
        assert!(!t.is_done(), "刚结束一轮专注不该算收工");
        assert!(t.running);
    }

    #[test]
    fn pomodoro_reset_restarts_work_phase() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&no_long_break(600, 300));
        t.start();
        t.secs = 100; // 模拟专注进行到一半
        t.reset();
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.secs, 600);
        assert_eq!(t.round, 0);
        assert!(!t.running);
    }

    /// 收工后重置要能把「已跑满」这格也清掉，否则开始键会被 is_done 挡回来。
    #[test]
    fn finished_pomodoro_restarts_from_a_clean_set() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 1,
            short_break: 1,
            long_break: 0,
            rounds: 1,
            cycles: 1,
            seq: Vec::new(),
        });
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        // W1 → 长休息 1s（rounds=1）→ 收工
        t.tick_at(t0 + Duration::from_secs(2));
        assert!(t.is_done() && !t.running);
        t.start();
        assert!(t.running);
        assert!(!t.is_done());
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.secs, 1);
    }

    /// 时长下限取 1：为 0 会让阶段在同一个心跳里连着翻，一次发出好几个结束事件。
    #[test]
    fn pomo_zero_durations_are_clamped_to_one_second() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo(&Pomo {
            work: 0,
            short_break: 0,
            long_break: 0,
            rounds: 0,
            cycles: 0,
            seq: Vec::new(),
        });
        assert_eq!(t.secs, 1, "番茄钟总时长不该被置成 0");
        assert_eq!(t.pomo_status(), ("WORK", 1), "rounds=0 该被当作 1");
    }

    #[test]
    fn clock_mode_never_ticks_or_finishes() {
        let mut t = Timer::new(Mode::Clock, 60);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0);
        t.tick_at(t0 + Duration::from_secs(5));
        assert_eq!(t.secs, 0); // 显示实时读取系统时间，secs 不变
        assert_eq!(t.take_finished(), None);
    }

    #[test]
    fn start_countdown_sets_total_and_runs() {
        let mut t = Timer::new(Mode::Countdown, 60);
        t.start_countdown(1500);
        assert_eq!(t.total, 1500);
        assert_eq!(t.secs, 1500);
        assert!(t.running);
    }

    #[test]
    fn set_mode_resets() {
        let mut t = Timer::new(Mode::Countdown, 60);
        t.start();
        let t0 = Instant::now();
        t.tick_at(t0); // 建立基准
        t.tick_at(t0 + Duration::from_secs(10));
        assert_eq!(t.secs, 50);
        t.set_mode(Mode::Stopwatch);
        assert_eq!(t.secs, 0);
        assert!(!t.running);
        // 切回倒计时应回到总时长
        t.set_mode(Mode::Countdown);
        assert_eq!(t.secs, 60);
    }

    #[test]
    fn mode_names_roundtrip() {
        for mode in [
            Mode::Countdown,
            Mode::Stopwatch,
            Mode::Pomodoro,
            Mode::Clock,
        ] {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::from_name("bogus"), None);
    }
}
