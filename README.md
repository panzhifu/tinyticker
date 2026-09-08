# TinyTicker

极简悬浮计时器：倒计时 / 秒表 / 番茄钟 / 时钟挂件。Rust 编写，纯 CPU 软渲染，内置 8x8 位图字体，单二进制约 1 MB，无任何运行时字体或 UI 依赖。

![平台](https://img.shields.io/badge/platform-Linux%20(Wayland%20%2F%20X11)-blue) ![许可](https://img.shields.io/badge/license-MIT-green) ![版本](https://img.shields.io/badge/version-0.4.0-brightgreen)

## 功能

- **四种模式**
  - **倒计时**：归零自动停止、变绿显示 `DONE`
  - **秒表**：从 0 正计时
  - **番茄钟**：专注 / 休息自动轮转并累计轮数，每阶段结束提醒
  - **时钟挂件**：实时显示本地时间 `HH:MM:SS`
- **无边框透明悬浮窗**：逐像素预乘 ARGB，`bg_alpha` 从 0（全透明，只剩文字）到 255（不透明）自由调节
- **交互**：左键按住拖动、右键关闭、**滚轮缩放**（0.5–3.0，自动持久化）
- **时长输入**：相对时长 `25m` / `1h30m` / `90`，或**绝对时刻** `14:30`（已过则算明天）
- **托盘控制**：开始 / 暂停 / 重置；「时长预设」二级菜单（1 / 5 / 15 / 25 / 45 / 60 分钟，点击即开始）；四模式切换；退出
- **结束动作**：桌面通知（带「再来一次」按钮）+ 可选 `on_finish` shell 命令（锁屏、关机、放音乐等）
- **配置持久化**：时长、模式、配色、透明度、缩放、番茄钟时长、结束命令、窗口位置，退出时自动写回
- **走时不漂移**：以整数秒为基准推进，休眠恢复或高负载后自动补齐整秒

## 构建

依赖：Rust 1.88+（edition 2024 + let-chains），Linux。

```sh
cargo build --release                          # Wayland + X11 通用版（约 1.6 MB）
cargo build --release --no-default-features    # 仅 Wayland 极小版（约 1.1 MB）
```

## 用法

```text
用法: tinyticker [选项] [时长]

时长:
  纯数字按秒（"90"），或数字+单位序列（"25m"、"1h30m"、"1h 30m 10s"）
  也支持绝对时刻（"14:30" / "14:30:45"，已过则视为明天）
  不带时长时使用配置文件中的值（默认 60 秒）

选项:
  -s, --stopwatch  以秒表模式启动
  -c, --countdown  以倒计时模式启动（默认）
  -p, --pomodoro   以番茄钟模式启动
  -k, --clock      以时钟挂件模式启动
  -r, --running    启动后立即开始计时
  -h, --help       显示帮助
  -V, --version    显示版本
```

示例：

```sh
tinyticker 25m          # 25 分钟倒计时
tinyticker 14:30        # 倒计时到今天 14:30
tinyticker -p -r        # 立即开始番茄钟
tinyticker -k           # 桌面时钟挂件
```

悬浮窗交互：左键按住拖动，右键关闭，滚轮缩放。托盘图标右键打开完整菜单。

## 配置

文件路径：`$XDG_CONFIG_HOME/tinyticker/config.conf`（默认 `~/.config/tinyticker/config.conf`），退出时自动写回，也可手动编辑后重启生效：

```ini
duration = 1500        # 倒计时总时长（秒），支持 "25m" 写法
mode = countdown       # countdown | stopwatch | pomodoro | clock
bg_alpha = 0           # 背景不透明度 0-255：0 全透明（只剩文字），255 不透明
zoom = 1.0             # 窗口缩放倍数 0.5-3.0（滚轮调节）
pomo_work = 1500       # 番茄钟专注时长（秒）
pomo_break = 300       # 番茄钟休息时长（秒）
on_finish = loginctl lock-session   # 计时结束执行的命令（可选，省略则只通知）
color_bg = 0f0f14      # 背景色（#RRGGBB / 0xRRGGBB / RRGGBB 均可）
color_running = ffffff # 运行中
color_paused = ffc850  # 暂停
color_done = 50dc78    # 结束
window_x = 100         # 窗口位置（成对出现才生效；Wayland 忽略）
window_y = 200
```

透明实现：Wayland 走自研 ARGB8888 shm 呈现（softbuffer 的 Wayland 后端硬编码 XRGB，无透明能力）；X11 走 softbuffer + depth-32 visual 的 alpha 字节直通（需合成器，如 picom）。两后端统一预乘 alpha 像素格式。

> Wayland 下窗口无法自行置顶或定位（协议限制），需合成器规则配合。niri 示例：
> ```kdl
> window-rule {
>     match title="TinyTicker"
>     open-floating true
> }
> ```

## 项目结构

```
src/
├── main.rs    # 入口：CLI 解析、通道装配
├── app.rs     # 悬浮窗口（winit 事件循环 + 绘制调度）
├── surface.rs # 呈现后端抽象 + X11 softbuffer 实现
├── wayland.rs # Wayland ARGB shm 呈现（透明能力）
├── clock.rs   # 本地时间读取（libc localtime_r，避免引入 chrono）
├── timer.rs   # 计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟），含走时补齐
├── render.rs  # 像素画布、8x8 字形渲染、时间格式化
├── config.rs  # 配置持久化（纯 std 文件 IO）
├── parse.rs   # 相对时长与绝对时刻解析
├── tray.rs    # 托盘菜单 / 通知 / 程序化时钟图标
└── font8x8.rs # 内置 ASCII 位图字体
```

技术栈：[winit](https://crates.io/crates/winit)（窗口）、[wayland-client](https://crates.io/crates/wayland-client)（Wayland 呈现）、[softbuffer](https://crates.io/crates/softbuffer)（X11 呈现）、[ldtray](https://crates.io/crates/ldtray)（托盘与通知）。

## 已知限制

- 内置字体仅 ASCII，不支持中文等非 ASCII 字符显示
- 全透明模式下窗口输入区域仍是整个矩形，会拦截下方窗口的鼠标事件
- 无全局快捷键（Wayland 无统一协议）
- Wayland 下置顶与窗口自定位依赖合成器规则

## 致谢与许可

- [font8x8](https://github.com/dhepper/font8x8)（`src/font8x8.rs`），MIT License
- 本项目以 MIT 许可发布，见 [LICENSE](LICENSE)
