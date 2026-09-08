//! 计时状态机：倒计时 / 秒表 / 番茄钟 / 时钟挂件，秒级精度。

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
    Break,
}

/// 一次"计时结束"事件的来源（决定通知文案与是否执行 on_finish 命令）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Finished {
    /// 倒计时归零。
    Countdown,
    /// 番茄钟专注阶段完成。
    PomodoroWork,
    /// 番茄钟休息结束。
    PomodoroBreak,
}

/// 计时器核心状态。
pub struct Timer {
    pub mode: Mode,
    /// 倒计时总时长（秒）；秒表模式下保留最近一次的倒计时值。
    pub total: u32,
    /// 倒计时为剩余秒数，秒表为已进行秒数；时钟模式不使用。
    pub secs: u32,
    pub running: bool,
    finished: Option<Finished>, // 每次结束置位一次，由 take_finished 消费
    last_tick: Cell<Option<Instant>>,
    // 番茄钟状态
    pub phase: Phase,
    /// 已完成的专注轮数。
    pub round: u32,
    work_secs: u32,
    break_secs: u32,
}

impl Timer {
    pub fn new(mode: Mode, total: u32) -> Self {
        let mut t = Self {
            mode,
            total,
            secs: 0,
            running: false,
            finished: None,
            last_tick: Cell::new(None),
            phase: Phase::Work,
            round: 0,
            work_secs: 1500,
            break_secs: 300,
        };
        t.reset();
        t
    }

    /// 当前显示的秒数（倒计时为剩余，秒表为已进行）。
    pub fn display_secs(&self) -> u32 {
        self.secs
    }

    /// 倒计时是否已归零。
    pub fn is_done(&self) -> bool {
        self.mode == Mode::Countdown && self.secs == 0
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
                self.secs = self.work_secs;
                self.round = 0;
            }
        }
        self.running = false;
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

    /// 设置番茄钟专注/休息时长（秒，至少 1）；番茄钟模式下重置。
    pub fn set_pomo_durations(&mut self, work: u32, break_: u32) {
        self.work_secs = work.max(1);
        self.break_secs = break_.max(1);
        if self.mode == Mode::Pomodoro {
            self.reset();
        }
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
            Mode::Pomodoro => {
                if self.secs > 0 {
                    self.secs -= 1;
                }
                if self.secs == 0 {
                    // 阶段结束：通知一次并自动进入下一阶段（保持运行）
                    if self.phase == Phase::Work {
                        self.round += 1;
                        self.finished = Some(Finished::PomodoroWork);
                        self.phase = Phase::Break;
                        self.secs = self.break_secs;
                    } else {
                        self.finished = Some(Finished::PomodoroBreak);
                        self.phase = Phase::Work;
                        self.secs = self.work_secs;
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

    fn tick_at(&mut self, now: Instant) {
        let Some(last) = self.last_tick.get() else {
            self.last_tick.set(Some(now)); // 首次调用仅建立基准
            return;
        };
        let elapsed = now.duration_since(last).as_secs();
        if elapsed == 0 {
            return;
        }
        // 基准只前进整数秒，保留亚秒余量，保证长期走时不漂移；
        // 停顿（休眠恢复、高负载）期间的整秒也要补齐
        self.last_tick.set(Some(last + Duration::from_secs(elapsed)));
        for _ in 0..elapsed {
            self.tick();
            if !self.running {
                break; // 倒计时已归零，无需继续补齐
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn pomodoro_cycles_phases_and_counts_rounds() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo_durations(2, 1);
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

    #[test]
    fn pomodoro_reset_restarts_work_phase() {
        let mut t = Timer::new(Mode::Pomodoro, 0);
        t.set_pomo_durations(600, 300);
        t.start();
        t.secs = 100; // 模拟专注进行到一半
        t.reset();
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.secs, 600);
        assert_eq!(t.round, 0);
        assert!(!t.running);
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
        for mode in [Mode::Countdown, Mode::Stopwatch, Mode::Pomodoro, Mode::Clock] {
            assert_eq!(Mode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(Mode::from_name("bogus"), None);
    }
}
