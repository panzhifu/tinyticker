//! 系统托盘：菜单控制、快速预设、计时结束通知。
//!
//! 在独立线程运行；托盘不可用（如无 StatusNotifierItem 宿主）时只打印
//! 警告，不影响悬浮窗口。

use std::sync::mpsc::Sender;
use std::thread;

use ldtray::{
    ActionId, Event, Icon, Menu, MenuItem, Notification, Tray, TrayConfig, TrayHandle,
};

use crate::timer::Mode;

/// 托盘 → 主窗口的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Pause,
    Reset,
    /// 快速预设：设置倒计时时长，重置并立即开始。
    Preset(u32),
    /// 切换计时模式（重置计时）。
    SetMode(Mode),
    Quit,
}

// 菜单项 ID（自行约定，托盘回调中原样回传）
const ID_START: u32 = 1;
const ID_PAUSE: u32 = 2;
const ID_RESET: u32 = 3;
const ID_PRESET_BASE: u32 = 10;
const ID_PRESET_LAST: u32 = ID_PRESET_BASE + PRESETS.len() as u32 - 1;
const ID_MODE_COUNTDOWN: u32 = 20;
const ID_MODE_STOPWATCH: u32 = 21;
const ID_MODE_POMODORO: u32 = 22;
const ID_MODE_CLOCK: u32 = 23;
const ID_QUIT: u32 = 99;
/// 通知里“再来一次”按钮的 ActionId。
const ACTION_RESTART: u32 = 1;

/// 快速预设（标签, 秒）。
const PRESETS: [(&str, u32); 6] = [
    ("1 分钟", 60),
    ("5 分钟", 300),
    ("15 分钟", 900),
    ("25 分钟", 1500),
    ("45 分钟", 2700),
    ("1 小时", 3600),
];

/// 启动托盘线程；就绪后把 [`TrayHandle`] 发回主线程（用于发通知）。
pub fn spawn(cmd_tx: Sender<Command>, handle_tx: Sender<TrayHandle>) {
    thread::spawn(move || {
        if let Err(e) = run(cmd_tx, handle_tx) {
            eprintln!("⚠️ 无法加载托盘图标: {e}");
        }
    });
}

fn run(cmd_tx: Sender<Command>, handle_tx: Sender<TrayHandle>) -> ldtray::Result<()> {
    let config = TrayConfig::new(clock_icon()?)
        .tooltip("TinyTicker")
        .menu(build_menu());

    let tray = Tray::new(config)?;
    let _ = handle_tx.send(tray.handle());

    tray.run(move |event| match event {
        Event::Menu(id) => {
            if let Some(cmd) = menu_command(id.0) {
                let _ = cmd_tx.send(cmd);
            }
        }
        // 通知里的“再来一次”按钮
        Event::NotificationAction(ActionId(ACTION_RESTART)) => {
            let _ = cmd_tx.send(Command::Start);
        }
        _ => {}
    })
}

fn menu_command(id: u32) -> Option<Command> {
    match id {
        ID_START => Some(Command::Start),
        ID_PAUSE => Some(Command::Pause),
        ID_RESET => Some(Command::Reset),
        ID_MODE_COUNTDOWN => Some(Command::SetMode(Mode::Countdown)),
        ID_MODE_STOPWATCH => Some(Command::SetMode(Mode::Stopwatch)),
        ID_MODE_POMODORO => Some(Command::SetMode(Mode::Pomodoro)),
        ID_MODE_CLOCK => Some(Command::SetMode(Mode::Clock)),
        ID_QUIT => Some(Command::Quit),
        ID_PRESET_BASE..=ID_PRESET_LAST => {
            Some(Command::Preset(PRESETS[(id - ID_PRESET_BASE) as usize].1))
        }
        _ => None,
    }
}

fn build_menu() -> Menu {
    let presets = PRESETS.iter().enumerate().map(|(i, (label, _))| {
        MenuItem::button(ID_PRESET_BASE + i as u32, format!("⏱ {label}"))
    });
    Menu::new()
        .item(MenuItem::button(ID_START, "▶ 开始"))
        .item(MenuItem::button(ID_PAUSE, "⏸ 暂停"))
        .item(MenuItem::button(ID_RESET, "⟳ 重置"))
        .item(MenuItem::separator())
        .item(MenuItem::submenu("时长预设", presets))
        .item(MenuItem::submenu(
            "模式",
            [
                MenuItem::button(ID_MODE_COUNTDOWN, "倒计时"),
                MenuItem::button(ID_MODE_STOPWATCH, "秒表"),
                MenuItem::button(ID_MODE_POMODORO, "🍅 番茄钟"),
                MenuItem::button(ID_MODE_CLOCK, "🕐 时钟"),
            ],
        ))
        .item(MenuItem::separator())
        .item(MenuItem::button(ID_QUIT, "✕ 退出"))
}

/// 发一条桌面通知（计时结束等事件由主线程调用）。
/// `action_label` 是通知按钮文案，点击后向主窗口发送 Start 命令。
pub fn notify(handle: &TrayHandle, body: &str, action_label: &str) {
    let note = Notification::new("TinyTicker", body).action(ACTION_RESTART, action_label);
    let _ = handle.notify(note);
}

/// 程序化绘制一枚 32x32 时钟图标（深色表盘 + 白色表圈和指针），
/// 避免引入图片资源文件。
fn clock_icon() -> ldtray::Result<Icon> {
    const SIZE: usize = 32;
    const CENTER: f32 = 15.5;
    let mut rgba = vec![0u8; SIZE * SIZE * 4]; // 圆外全透明

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - CENTER;
            let dy = y as f32 - CENTER;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > 15.0 {
                continue; // 圆外透明
            }
            let (r, g, b) = if dist > 12.5 {
                (255, 255, 255) // 表圈
            } else {
                (15, 15, 20) // 表盘
            };
            let idx = (y * SIZE + x) * 4;
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = 255;
        }
    }
    // 指针（白色，2px）：分针向上、时针向右
    for y in 7..=16 {
        put_white(&mut rgba, SIZE, 15, y);
        put_white(&mut rgba, SIZE, 16, y);
    }
    for x in 16..=23 {
        put_white(&mut rgba, SIZE, x, 15);
        put_white(&mut rgba, SIZE, x, 16);
    }

    Icon::from_rgba(SIZE as u32, SIZE as u32, rgba)
}

fn put_white(rgba: &mut [u8], size: usize, x: usize, y: usize) {
    let idx = (y * size + x) * 4;
    rgba[idx] = 255;
    rgba[idx + 1] = 255;
    rgba[idx + 2] = 255;
    rgba[idx + 3] = 255;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands() {
        assert!(matches!(menu_command(ID_START), Some(Command::Start)));
        assert!(matches!(menu_command(ID_QUIT), Some(Command::Quit)));
        assert!(matches!(
            menu_command(ID_MODE_STOPWATCH),
            Some(Command::SetMode(Mode::Stopwatch))
        ));
        assert!(matches!(
            menu_command(ID_MODE_POMODORO),
            Some(Command::SetMode(Mode::Pomodoro))
        ));
        assert!(matches!(
            menu_command(ID_MODE_CLOCK),
            Some(Command::SetMode(Mode::Clock))
        ));
        // 预设区间首尾
        assert!(matches!(
            menu_command(ID_PRESET_BASE),
            Some(Command::Preset(60))
        ));
        assert!(matches!(
            menu_command(ID_PRESET_BASE + PRESETS.len() as u32 - 1),
            Some(Command::Preset(3600))
        ));
        // 未定义的 id
        assert_eq!(menu_command(50), None);
    }

    #[test]
    fn clock_icon_has_valid_size() {
        let icon = clock_icon().expect("图标构建失败");
        assert_eq!(icon.width(), 32);
        assert_eq!(icon.height(), 32);
    }
}
