# TinyTicker

极简悬浮计时器：倒计时 / 秒表 / 番茄钟 / 时钟挂件。Rust 编写，纯 CPU 软渲染，内置 8x8 位图字体，单二进制 0.5 MB，零第三方 crate，无任何运行时字体或 UI 依赖。

![平台](https://img.shields.io/badge/platform-Linux%20(Wayland%20%2F%20X11)-blue) ![许可](https://img.shields.io/badge/license-MIT-green) ![版本](https://img.shields.io/badge/version-0.5.0-brightgreen) ![CI](https://github.com/panzhifu/tinyticker/actions/workflows/ci.yml/badge.svg)

未压缩体积：**Wayland 极小版 0.47 MB**、通用版（Wayland + X11）0.48 MB——X11 后端只多 9 KB。UPX 压缩后的发布产物体积待下次发版按 CI 实测更新（v0.4.1 为 0.38 / 0.50 MB）。

## 功能

- **四种模式**
  - **倒计时**：归零自动停止、变绿显示 `DONE`
  - **秒表**：从 0 正计时
  - **番茄钟**：专注 / 短休息自动轮转，每跑满 4 轮专注插一次长休息；状态行显示 `WORK n` / `BREAK n` / `LONG n`，可限定只跑几组后收工，每阶段结束提醒
  - **时钟挂件**：实时显示本地时间，`HH:MM:SS` 或 12 小时制 `hh:mm:ss AM`
- **无边框透明悬浮窗**：逐像素预乘 ARGB，`bg_alpha` 从 0（全透明，只剩文字）到 255（不透明）自由调节
- **浮在全屏窗口之上**：Wayland 上走 layer-shell 的 overlay 层，网页视频全屏时依然可见（协议限制下这是唯一办法）；X11 走 `_NET_WM_STATE_ABOVE`
- **交互**：左键按住拖动、右键关闭、**滚轮缩放**（0.5–3.0，自动持久化）；**在托盘图标上滚动同样缩放**（SNI `Scroll`，挂件太小或开着点击穿透时更顺手）
- **外观即时可调**：托盘「外观」子菜单——5 档背景透明度 + 6 套配色预设 + 4 种图标内容，点击立刻生效并写回配置，不必重启；菜单带勾选，看得出当前用的是哪一项
- **外部文本源**：`text_source` 指向一个由别的进程写的文件，它的第一行顶替状态行（编译进度、下载百分比之类）。靠 `(大小, mtime)` 判断要不要重读，超过 64 KB 直接不采信，文件缺失或清空就交还状态行。**只读不执行**——不像 Catime 那样替你启动插件脚本，生产者得自己跑
- **时长输入**：相对时长 `25m` / `1h30m` / `90`，或**绝对时刻** `14:30`（已过则算明天）
- **托盘控制**：**左键开始/暂停、中键重置**，右键打开完整菜单——时长预设（档位由 `presets` 配置，出厂 1 / 5 / 15 / 25 / 45 / 60 分钟，点击即开始）、四模式切换、外观、退出
- **托盘图标会走**：`tray_icon` 选真实时表盘（指针按本地时间摆）、CPU / 内存 / 电量的水位占用表，或**你自己的 GIF 动图**（`tray_gif`）。占用数据读 `/proc/stat`、`/proc/meminfo`、`/sys/class/power_supply`；图标按派发节拍重绘，像素变了才发 `NewIcon`
- **结束动作**：桌面通知（带「再来一次」按钮）+ 可选 `on_finish` shell 命令（锁屏、关机、放音乐等）
- **配置持久化**：时长、时长预设、模式、配色、透明度、缩放、番茄钟节奏、结束命令、窗口位置，退出时自动写回
- **走时不漂移**：以整数秒为基准推进，休眠恢复或高负载后自动补齐整秒

## 安装

只发布 Linux x86_64。产物名带版本号（自 v0.5.0 起；更早的版本沿用不带版本的旧名），
URL 不可变，可直接校验：

```sh
TAG=v0.5.0                                   # 换成 Releases 页面里的版本
BASE=https://github.com/panzhifu/tinyticker/releases/download/$TAG
curl -fLO "$BASE/tinyticker-${TAG#v}-x86_64-unknown-linux-gnu"
curl -fLO "$BASE/SHA256SUMS.txt"
sha256sum -c --ignore-missing SHA256SUMS.txt
install -Dm755 "tinyticker-${TAG#v}-x86_64-unknown-linux-gnu" ~/.local/bin/tinyticker
```

上面是 **Wayland + X11 通用版**（拿不准就用它）。Wayland 会话还可以用更小的极小版，
把文件名换成 `tinyticker-${TAG#v}-wayland-x86_64-unknown-linux-gnu`——它没有 X11 后端，
X11 会话下无法运行。

桌面集成（可选，让应用菜单能搜到、可开机自启）：

```sh
APP_ID=io.github.panzhifu.tinyticker
for f in "$APP_ID.desktop" "$APP_ID.svg" "$APP_ID.metainfo.xml"; do curl -fLO "$BASE/$f"; done
install -Dm644 "$APP_ID.desktop"      ~/.local/share/applications/"$APP_ID".desktop
install -Dm644 "$APP_ID.svg"          ~/.local/share/icons/hicolor/scalable/apps/"$APP_ID".svg
install -Dm644 "$APP_ID.metainfo.xml" ~/.local/share/metainfo/"$APP_ID".metainfo.xml
mkdir -p ~/.config/autostart && cp "$APP_ID.desktop" ~/.config/autostart/   # 开机自启
```

系统级安装把 `~/.local` 换成 `/usr/local`（需 root）即可。校验和只保证「下载没被篡改」，
产物尚未做发布签名，需要强信任链的话请自行 `cargo build --release`。

## 构建

依赖：Rust 1.88+（edition 2024 + let-chains），Linux。不需要任何 X11 / Wayland / D-Bus 的 `-dev` 包——三边都是运行时 `dlopen` 发行版自带的共享库。

```sh
cargo build --release                          # Wayland + X11 通用版（约 0.48 MB）
cargo build --release --no-default-features    # 仅 Wayland 极小版（约 0.47 MB）
```

也可直接从 [Releases](https://github.com/panzhifu/tinyticker/releases) 下载预编译二进制（见上一节，UPX 压缩）。打 tag 会自动触发构建、校验和生成与上传。

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

悬浮窗交互：左键按住拖动，右键关闭，滚轮缩放。

托盘交互：左键开始/暂停，中键重置，右键打开完整菜单，在图标上滚动即可缩放。为此 `ItemIsMenu`
报的是 `false`——规范要求「只有菜单、没有自己的激活行为」的项才报 `true`，而宿主据此会把左键
直接吞成弹菜单，`Activate` 就永远到不了我们这里。挂件上没有做「点击输入时长」：那需要一个文本
输入框，而本项目只有一个 8x8 位图渲染器；设时长走托盘预设或命令行参数。
透明度与配色走托盘「外观」子菜单，点一下即时生效；改 `bg_alpha` / `color_*` 也行，但要重启。

## 配置

配置文件退出时自动写回，也可手动编辑后重启生效：

```
$XDG_CONFIG_HOME/tinyticker/config.conf      # 该变量缺失或不是绝对路径时
~/.config/tinyticker/config.conf             # 退回 $HOME/.config
```

实际解析出的路径可由 `tinyticker -h` 查看。本项目只做 Linux，不再为其它系统的目录惯例保留代码。

```ini
duration = 1500        # 倒计时总时长（秒），支持 "25m" 写法
mode = countdown       # countdown | stopwatch | pomodoro | clock
bg_alpha = 0           # 背景不透明度 0-255：0 全透明（只剩文字），255 不透明
zoom = 1.0             # 窗口缩放倍数 0.5-3.0（滚轮调节）
click_through = false  # 鼠标穿透：true 则只有文字处可点（不挡下方窗口），
                       # 但拖动也要点中文字；false（默认）整窗可拖动
clock_12h = false      # 时钟挂件用 12 小时制（带 AM/PM）；false 为 24 小时制
tray_icon = clock      # 托盘图标：clock 真实时表盘 | cpu | memory | battery 水位占用表
                       # | gif 动图。这一项也能从托盘「外观 → 图标内容」直接点选，改完即时生效
                       # 占用表每秒重绘，颜色按 <60% 绿 / <85% 黄 / 其余红分级；
                       # 充电中一律绿，没有电池选 battery 则显示空心环
tray_gif = ~/pics/spin.gif   # tray_icon = gif 时的动图路径，支持开头的 ~/
                       # 只支持 GIF87a/89a；文件换了会自动重新解码，最大 2 MB
pomo_work = 1500       # 番茄钟专注时长（秒）
pomo_break = 300       # 番茄钟短休息时长（秒）
pomo_long_break = 900  # 番茄钟长休息时长（秒）：每跑满 pomo_rounds 轮专注后插一次
                       # 0 = 不安排长休息，每轮后面都跟短休息
pomo_rounds = 4        # 一「组」有几轮专注（1-100），即每几轮插一次长休息
pomo_cycles = 0        # 跑满几组后自动收工显示 DONE（0-100）
                       # 0（默认）= 不限，一直轮转——和旧版行为一致
presets = 60, 300, 900, 1500, 2700, 3600
                       # 托盘「时长预设」子菜单的档位，点击即重置并开始
                       # 逗号或空格分隔，每段支持 "25m" / "1h30m" 写法；最多 24 段、
                       # 单段不超过 24 小时。任一段非法则整条作废、回到上面这 6 档
                       # 菜单标签按时长自动生成（90 → "1 分 30 秒"，5400 → "1 小时 30 分"）
on_finish = loginctl lock-session   # 计时结束执行的命令（可选，省略则只通知）
                       # 经 sh -c 解释，例如锁屏 loginctl lock-session、关机 systemctl poweroff
text_source = ~/tmp/build.out       # 外部文本源（可选）：第一行顶替状态行，支持开头的 ~/
                       # 只能显示 ASCII（内置 8x8 字体），非 ASCII 字符会留空位
color_bg = 0f0f14      # 背景色（#RRGGBB / 0xRRGGBB / RRGGBB 均可）
color_running = ffffff # 运行中
color_paused = ffc850  # 暂停
color_done = 50dc78    # 结束
window_x = 100         # 窗口位置（逻辑像素，成对出现才生效；layer-shell 与 X11 生效）
window_y = 200
```

透明实现：两后端统一预乘 alpha 的 0xAARRGGBB 像素。Wayland 走自研 ARGB8888 shm 呈现（softbuffer 的 Wayland 后端硬编码 XRGB，无透明能力，所以不用它）；X11 走 depth-32 TrueColor visual + `XPutImage`，alpha 字节直通合成器（需合成器，如 picom / KCompositor / Xwayland）。

> Wayland 上挂件走 layer-shell 的 overlay 层，自行置顶与定位，**不需要任何合成器规则**。

## 项目结构

```
src/
├── main.rs    # 入口：CLI 解析、通道装配、按会话挑选窗口路径
├── wl.rs      # Wayland 客户端：直调 libwayland-client，layer-shell overlay 层
│              #   自研 ARGB shm 双缓冲 + 拖动/滚轮/右键 + 事件循环
├── sys/       # 系统 API 声明层（dlopen + FFI，无第三方 crate）
│   ├── mod.rs     # dlopen / dlsym 封装
│   ├── wayland.rs # libwayland-client 声明 + 手写扩展协议接口描述符
│   ├── dbus.rs    # libdbus-1 声明（消息迭代器 / vtable / 总线与连接）
│   └── x11.rs     # libX11 / libXext 声明（事件 union 与结构体逐字段照 Xlib.h）
├── x11.rs     # X11 客户端：直调 libX11，override-redirect ARGB 窗口
│              #   XPutImage 软渲染 + 拖动/滚轮/右键 + 点击穿透（libXext SHAPE），仅 `x11` feature
├── widget.rs  # 与后端无关的挂件核心（计时推进、帧内容与布局）
├── clock.rs   # 本地时间读取（自研 TZif + POSIX 规则解析，不依赖 C 库时区 API）
├── sysinfo.rs # 系统状态采样（CPU 差分 / MemAvailable / 电量），只读 /proc 与 /sys
├── gif.rs     # 最小 GIF89a 解码器（LZW + 交错 + 子矩形 + disposal），托盘动图图标用
├── textsrc.rs # 外部文本源：读一个文件的第一行顶替状态行，带大小上限与变化检测
├── timer.rs   # 计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟），含走时补齐
├── render.rs  # 像素画布、8x8 字形渲染、时间格式化
├── config.rs  # 配置持久化（纯 std 文件 IO）
├── parse.rs   # 相对时长与绝对时刻解析
├── tray.rs    # 自研托盘：StatusNotifierItem + dbusmenu 菜单 + 通知 + 每秒重绘的图标
└── font8x8.rs # 内置 ASCII 位图字体

packaging/   # 随 release 发布的 freedesktop 资产
├── io.github.panzhifu.tinyticker.desktop      # 应用菜单入口
├── io.github.panzhifu.tinyticker.svg          # 图标（与托盘图标同款）
└── io.github.panzhifu.tinyticker.metainfo.xml # AppStream 元数据
```

技术栈：Wayland 侧不用任何 Rust GUI/协议库——`src/sys/wayland.rs` 声明 libwayland-client 的 C API 并在运行时 dlopen，扩展协议（layer-shell / viewporter / fractional-scale / relative-pointer / cursor-shape）的接口描述符按协议 XML 手写。托盘与通知同理：`src/sys/dbus.rs` 声明 libdbus-1 的 C API，`src/tray.rs` 自己实现 StatusNotifierItem + com.canonical.dbusmenu + 通知发送。因此 **Wayland 极小版零第三方 crate，`ldd` 只有 libc 与 libgcc_s**。

X11 侧同理：`src/sys/x11.rs` 逐字段照 `Xlib.h` 镜像 ABI，`src/x11.rs` 自己建 override-redirect 的 depth-32 ARGB 窗口、`XPutImage` 软渲染、`XShapeCombineRectangles` 做点击穿透，拖动靠 `XGrabPointer` + 根坐标。因此**两种构建都零第三方 crate**（`Cargo.lock` 里只有 tinyticker 自己），实测只硬链 `libc` 与 `libgcc_s`，X11 / Wayland / D-Bus 全部运行时 dlopen。

## 已知限制

- 内置字体仅 ASCII，不支持中文等非 ASCII 字符显示
- 托盘动图**只支持 GIF**：WebP 要整个 VP8 codec、PNG 要 inflate + 滤波、JPG 要 DCT，为零第三方依赖的 0.5 MB 二进制都不划算（Catime 那几个格式是靠 Windows 自带的 WIC 解的，Linux 没有等价物）。自写解码器还设了界：画布 ≤128×128、帧数 ≤64、文件 ≤2 MB，超出一律拒收
- `tray_icon = gif` 但 `tray_gif` 没配（或文件不可用）时，图标退回真实时表盘；托盘菜单里那一档会**置灰**并把原因写进标签（「GIF 动图（未配置 tray_gif）」），点了不会有动作
- 鼠标穿透为可选项：默认整窗接收输入（好拖动），开启 `click_through` 后只有文字可点——二者不可兼得
- 无全局快捷键（Wayland 无统一协议）
- 挂件上没有 Ctrl/Shift+滚轮：Wayland 的 pointer 事件不带 modifier，而 overlay 层挂件默认拿不到键盘焦点，读不到修饰键状态。连续调节一律走托盘（图标上滚动 = 缩放，菜单 = 透明度/配色），X11 侧同样如此，两端行为一致
- 依赖 layer-shell：合成器不提供该协议时（如 GNOME 裸机）直接报错退出，不再退回普通窗口——退回也没意义，全屏会被盖住
- X11 侧的透明需要合成器（picom / KWin / Xwayland）；没有合成器时背景会显示成不透明黑底。缩放系数取 `Xft.dpi / 96`，X11 没有分数缩放协议
- layer-shell 路径的挂件位置受合成器 reserved 区域影响：顶部面板（exclusive zone）会把它往下推

## 致谢与许可

- [font8x8](https://github.com/dhepper/font8x8)（`src/font8x8.rs`），MIT License
- 本项目以 MIT 许可发布，见 [LICENSE](LICENSE)
