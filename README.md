# TinyTicker

> 极简悬浮计时器：倒计时 / 秒表 / 番茄钟 / 时钟挂件。
> Rust 编写，纯 CPU 软渲染，**零第三方 crate**——Wayland / X11 / D-Bus / FreeType / ALSA 全部运行时 `dlopen` 发行版自带的共享库，单二进制 ~0.65 MB，二进制里不含任何字体数据。

![平台](https://img.shields.io/badge/platform-Linux%20(Wayland%20%2F%20X11)-blue) ![许可](https://img.shields.io/badge/license-MIT-green) ![版本](https://img.shields.io/badge/version-0.6.0-brightgreen) ![CI](https://github.com/panzhifu/tinyticker/actions/workflows/ci.yml/badge.svg)

| 构建 | 未压缩体积 | 说明 |
|---|---|---|
| Wayland 极小版 | 675,600 B（0.64 MB） | 无 X11 后端，X11 会话下无法运行 |
| Wayland + X11 通用版 | 684,888 B（0.65 MB） | 拿不准就装这个；X11 后端整份只多 9.3 KB |

UPX 压缩后的发布产物见 [Releases](https://github.com/panzhifu/tinyticker/releases)。与 Windows 侧 [Catime](https://github.com/vladelaina/Catime) 的逐条差距分析见 [GAP.md](GAP.md)，功能对照表见 [COMPARISON.md](COMPARISON.md)。

## 快速上手

**Arch Linux**（AUR，装完直接可用，含桌面集成）：

```sh
yay -S tinyticker          # 或 paru -S tinyticker
```

**其它发行版**——一条循环脚本装用户目录（校验和可验证，URL 不可变；自 v0.5.0 起产物名带版本号）：

```sh
TAG=v0.6.0
BASE=https://github.com/panzhifu/tinyticker/releases/download/$TAG
curl -fLO "$BASE/tinyticker-${TAG#v}-x86_64-unknown-linux-gnu"
curl -fLO "$BASE/SHA256SUMS.txt"
sha256sum -c --ignore-missing SHA256SUMS.txt
install -Dm755 "tinyticker-${TAG#v}-x86_64-unknown-linux-gnu" ~/.local/bin/tinyticker
```

跑起来：

```sh
tinyticker 25m             # 25 分钟倒计时，浮在所有窗口之上
tinyticker -k              # 桌面时钟挂件
tinyticker --help          # 全部用法与配置文件实际路径
```

桌面集成（可选，让应用菜单能搜到、可开机自启）：

```sh
APP_ID=io.github.panzhifu.tinyticker
for f in "$APP_ID.desktop" "$APP_ID.svg" "$APP_ID.metainfo.xml"; do curl -fLO "$BASE/$f"; done
install -Dm644 "$APP_ID.desktop"      ~/.local/share/applications/"$APP_ID".desktop
install -Dm644 "$APP_ID.svg"          ~/.local/share/icons/hicolor/scalable/apps/"$APP_ID".svg
install -Dm644 "$APP_ID.metainfo.xml" ~/.local/share/metainfo/"$APP_ID".metainfo.xml
```

Wayland 会话还可以用更小的极小版（把文件名换成 `tinyticker-${TAG#v}-wayland-x86_64-unknown-linux-gnu`）；系统级安装把 `~/.local` 换成 `/usr/local`（需 root）。校验和只保证「下载没被篡改」，产物尚未做发布签名，需要强信任链请自行 `cargo build --release` 或走 AUR 构建。

## 目录

- [功能](#功能)：[计时模式](#四种计时模式) · [悬浮窗与交互](#悬浮窗与交互) · [外观字体与显示](#外观字体与显示) · [托盘](#托盘) · [声音与结束动作](#声音与结束动作) · [配置与系统集成](#配置与系统集成) · [工程实现](#工程实现)
- [用法](#用法) · [配置](#配置) · [项目结构](#项目结构) · [已知限制](#已知限制) · [开发与发布](#开发与发布) · [致谢与许可](#致谢与许可)

## 功能

### 四种计时模式

| 模式 | 行为 |
|---|---|
| **倒计时** | 归零自动停止、变绿显示 `DONE`；也可以倒计时到**绝对时刻**（`14:30`，已过则算明天） |
| **秒表** | 从 0 正计时；`centiseconds` 档可见百分之一秒的零头 |
| **番茄钟** | 专注 / 短休息自动轮转，每跑满 4 轮专注插一次长休息；状态行 `WORK n` / `BREAK n` / `LONG n`，可限定只跑几组后收工，每阶段结束提醒。想完全自己排节奏就写 `pomo_seq = 25m,5m,15m`——任意一段序列（≤16 段），跑完整条算一遍，状态行换成 `POMO n` |
| **时钟挂件** | 实时本地时间，`HH:MM:SS` 或 12 小时制 `hh:mm:ss AM`；开了百分秒档也覆盖这一模式（`HH:MM:SS.cc`，关显示秒时百分秒一并压掉） |

- **时长输入**：相对时长 `25m` / `1h30m` / `90` / `2d`，或绝对时刻 `14:30`（已过则算明天）；也认 Catime 那套 `t` 后缀写法 `14 30t`
- **走时不漂移**：每次推进取的是自重绘心跳以来的**真实时长差**并一次算清，休眠恢复、高负载都不会少走；不足一秒的余量留在状态里参与下一次进位，不因心跳间隔累积误差

### 悬浮窗与交互

- **无边框透明悬浮窗**：逐像素预乘 ARGB，`bg_alpha` 从 0（全透明，只剩文字）到 255（不透明）自由调节
- **浮在全屏窗口之上**：Wayland 走 layer-shell 的 overlay 层，网页视频全屏时依然可见（协议限制下这是唯一办法）；X11 走 `_NET_WM_STATE_ABOVE`
- **手势**：左键按住拖动、右键关闭、**滚轮缩放**（0.5–6.0，自动持久化）、**中键进编辑态**；**在托盘图标上滚动同样缩放**（SNI `Scroll`，挂件太小或开着点击穿透时更顺手）
- **编辑态是一档临时状态**：挂件中键、托盘「🛠 编辑态」、或 `tinyticker --edit`（`--no-edit` 退回）都能进。进去之后三件事变了——**整窗接收输入**（点击穿透临时让路，拖和滚轮不必去瞄那两行字）、**到点静默**（只把读数换成 `DONE`，不发通知也不执行 `on_finish`，一次性的 `--and` 也不会被悄悄消费）、**右键改成退出编辑态**（否则想退出去结果把程序关了）。状态行写成 `EDIT` 提示你在哪儿。它**不进配置**：下次启动总该是普通态。Catime 的编辑模式还要吃键盘与对话框，我们这一档只拿它"摆弄挂件"的那半边
- **挂件可以藏起来**：托盘「👻 隐藏挂件」勾一下，或 `tinyticker --hide`（配合单实例转发，这就是你的 DE 快捷键）。藏起来时计时**照常跑**、心跳回落到最慢一档（实测 10 秒 CPU 从 100ms 掉到 < 10ms），再点一下就回来

### 外观、字体与显示

- **外观即时可调**：托盘「外观」子菜单——5 档背景透明度 + 6 套整体配色 + **30 条数字颜色**（逐条取自 Catime 的预设，含两段与五段渐变）+ 8 种文字特效 + 6 档图标内容 + 「时间格式」（补零三档 / 时钟显示秒 / 百分之一秒），点击立刻生效并写回配置，不必重启；菜单带勾选，看得出当前用的是哪一项
- **文字特效与渐变**：`text_effect` 七种特效（辉光 / 玻璃 / 霓虹 / 全息 / 液态 / 水波 / 复古投影）全部在自研软渲染器里实现——文字先栅格化成单通道覆盖度图，在其上做可分离盒模糊、腐蚀求轮廓、梯度求边、正弦与 value-noise 位移，再按预乘 alpha 合成回画布；三色可写成 `_` 分隔的横向渐变，超过两个停靠点自动流动，也认 Catime 的具名渐变（`candy` / `breeze` / `frost` / `sunset` / `streamer`）。不引入任何图像库，也不依赖 GPU
- **状态行能写中文**：数字行保持内置 8x8 点阵按整数倍放大的像素风（所有特效的模糊半径与位移量都是照它标定的），而点阵覆盖不到的码位——中文、Latin-1、带圈数字这类符号——由运行时 `dlopen` 出来的 libfreetype 光栅宿主字体补齐，两种字形按同一条基线混排。字体自动发现（先试 Noto Sans CJK / Source Han / 文泉驿 / 霞鹜文楷，再按名字偏好扫字体根），也可用 `text_font` 指定文件；`text_font = off` 退回"非 ASCII 一律留空位"。整行都是 ASCII 时 freetype 一次都不会被调用，画面与以前逐字节相同。状态行字号另有 `status_font_px` 可调（数字行的点阵质感不受影响）
- **百分之一秒（可选）**：`centiseconds = true` 后数字行走 `45.32s` / `12:34.56` / `1:01:01.01`；托盘勾选或 `tinyticker --centis`（设定值，可绑快捷键）即时生效并写回。这一档把心跳从 200ms 降到 20ms，实测烧单核的 1%——不贵，但默认关着，且**只在真在走时**才提频：暂停时百分位原样冻住，恢复后接着数而不是回 `.00`。显示百分秒时倒计时读向下取整的剩余量（`9.30`），隐藏时读回向上那一格（`10`），开局不会先跳一秒
- **补零三档**：`time_pad` = `none`（`45s` / `12:34`）· `zero`（`00:45`）· `full`（`00:00:45`），值串与 Catime 的 `TimeFormatType` 对齐。补零的真正用处是**宽度固定**：`none` 档读数从 `45s` 跨到 `1:00` 时整行会左右抖，另两档不会

### 托盘

- **托盘控制**：**左键开始/暂停、中键重置**，右键打开完整菜单——时长预设（档位由 `presets` 配置，出厂 1 / 5 / 15 / 25 / 45 / 60 分钟，最多 50 档、超 20 档折进「更多 ▸」）、四模式切换、番茄分段（配了 `pomo_seq` 才出现：每段一项、当前段打勾，点了跳去那段）、外观、语言（自动/中文/English 三档单选，菜单标签即时重建）、编辑态、退出。手改配置文件把配色/特效改了，勾也不漂（主循环回填一份快照，套接字进来的命令同样跟）
- **托盘图标会走**：`tray_icon` 选真实时表盘（指针按本地时间摆）、CPU / 内存 / 电量 / **网络速率**的水位占用表，或**你自己的动图**（`tray_gif`）——GIF87a/89a（自研 LZW）与 PNG/APNG（自研 DEFLATE + 五种行滤波，dispose/blend 全语义）都行，路径指一个**目录**时按 Catime 的帧序列源走：文件名升序、每张图一帧。占用数据读 `/proc/stat`、`/proc/meminfo`、`/sys/class/power_supply`、`/proc/net/dev`；网络那一档是**对数水位**（1 KB/s 空盘、10 MB/s 满盘），线性刻度会让"挂着不动"和"满速下载"挤在同一格。`tray_numbers = true` 把那四档改成**直接写数字**（`42%`，网络是上下两行速率），走内置 8x8 点阵。图标按派发节拍重绘，像素变了才发 `NewIcon`
- **动图限速**：`tray_throttle` 五档（off / cpu / memory / **timer** / **fixed**，对齐 Catime 的 `ANIMATION_SPEED_METRIC`）。半载以下完全原速，半载到满载线性掉到 1/4 速——是"变慢"不是跳帧：动图走自己的虚拟时钟。timer 档拿倒计时进度当负载，fixed 档按 `tray_gif_speed` 直接给倍率（默认 200% = 双倍速，五档里唯一会变快的）
- **悬停提示带实时指标**：正文是 `CPU 12% · 内存 44% · ↓ 1.0 MB/s ↑ 2048 B/s · 电池 87%⚡`，下面接**开机时长**（天/时/分三档）和限速生效时的**动画倍率**；每秒随采样刷新、变了才发 `NewToolTip`（SNI 里宿主是拉取属性的，不催就永远显示启动时那份）

### 声音与结束动作

- **桌面通知**：带「再来一次」按钮；可 `notify = false` 整个关掉、`notify_text` 把正文写死成一句话
- **提示音**：`alarm_sound` = `beep`（合成两声 880 Hz 短音——Linux 没有系统提示音服务，自己造一个反而不挑硬件）或 WAV 文件路径（PCM 8/16/24/32 bit 与 IEEE float，任意采样率/声道），`alarm_volume` 0-100。ALSA 运行时 dlopen + 后台线程，不挡倒计时也不拖通知；托盘「🔊 试听音效」不用等计时结束就能确认响不响
- **结束命令**：可选 `on_finish` shell 命令（锁屏、关机、放音乐等）。**要"这次而已"就用 `--and <命令>`**：只武装一次、跑完自动回落、永不落盘——常驻的 `on_finish` 误留一次就每次结束都触发，这条不会
- **到点文案**：`timeout_text` 把数字行整行换成一句话（可中文），`"0"` = 留空，状态行仍写 `DONE`

### 配置与系统集成

- **配置持久化**：时长、预设、模式、配色、透明度、缩放、番茄节奏、显示精度、结束命令、窗口位置，退出时自动写回；写入走"同目录临时文件 + `rename`"的**原子替换**，掉电最多退回上一版
- **配置热加载**：手改 `config.conf` 不用重启——每拍一次 `stat`，靠 `(大小, mtime)` 认变更，真变了才重读并当场应用（颜色 / 特效 / 补零 / 百分秒 / 缩放 / 番茄节奏 / 结束命令 / 文本源 / 状态行字号 / 托盘图标档 / 语言）。**跑动中的计时器不会被配置文件掐掉**：模式与总时长只在它空着时跟着文件走。只有四样要重启：时长预设与番茄分段（菜单启动时建好）、GIF 动图路径、窗口位置——改到这些时会打一行 `↻ 配置已热加载（…要重启才生效）` 说清楚
- **单实例，二次启动 = 下命令**：已经在跑的时候再敲 `tinyticker 25m` 不会多开一个窗口，而是把同样的参数交给那个实例（Unix 套接字，`$XDG_RUNTIME_DIR/tinyticker.sock`，权限 0600）。`--config-dir <目录>`（或 `TINYTICKER_CONFIG_DIR`）把配置与套接字一起搬进去，于是**可以同时跑两套互不相干的挂件**。**全局快捷键因此不必我们自己实现**——Wayland 没有统一的快捷键协议，你在 KDE / GNOME / niri 的快捷键设置里绑一条 `tinyticker 25m` 就完了
- **无时长语义的命令行动作**：`--pause` / `--toggle` / `--reset` 与 `--centis` / `--no-centis`——同那条套接字一样都是"给在跑的实例下命令"，快捷键除"开始某时长"外也能绑暂停/继续与重置
- **开机自启**：托盘「🚀 开机自启」勾一下就在 `~/.config/autostart/` 写好那份与打包同名的 `.desktop`（`Exec` 用可执行文件的真实路径），再点一下删掉。判据就是文件在不在，所以在系统设置里关掉我们也能看见
- **外部文本源**：`text_source` 指向一个由别的进程写的文件，第一行顶替状态行（编译进度、下载百分比之类）。靠 `(大小, mtime)` 判断重读，超 64 KB 不采信，缺失或清空就交还状态行。**只读不执行**——Catime 那套插件也要用户逐个手动启动并显式信任，我们连启动这一步都不做，生产者自己跑
- **托盘文案双语**：菜单 / 悬停提示 / 通知走 `language = auto|zh|en`（auto 按 `$LANGUAGE` / `$LANG` 判），托盘「语言」子菜单即时切换。挂件那两行不受它管：数字行恒 ASCII，状态行写什么由内容决定

### 工程实现

- **零第三方 crate**：`Cargo.lock` 里只有本项目自己；`ldd` 只有 libc、libm 与 libgcc_s。构建不需要任何 X11 / Wayland / D-Bus / FreeType 的 `-dev` 包——四边加 ALSA 都是运行时 `dlopen` 发行版自带的共享库（见[项目结构](#项目结构)的逐模块注释）
- **心跳阶梯五档，空闲睡到 1 秒**：20（百分秒在走）/ 50（动画特效）/ 200（计时在跑）/ 250（走针时钟）/ 1000ms（什么都没动）。下探那两档纯省电，**不拿响应换**：托盘点击与套接字命令发完会摸一下 self-pipe（`src/wake.rs`），事件循环把它与 Wayland/X11 的 fd 一起 `poll`，命令到了立刻醒
- **自研时区**：TZif v2/v3 的 64 位块 + POSIX 规则文法（`Jn`/`n`/`M m.w.d`）全部自己解析，不依赖 C 库时区 API，跨 2038 不出错
- **230 项内联测试**：畸形输入零 panic（GIF/PNG/WAV 解析器全部按不可信输入对待）、时间解析与格式化的边界逐一钉住、`cargo clippy -- -D warnings` 零告警是 CI 门槛

## 用法

```text
用法: tinyticker [选项] [时长]

时长:
  纯数字按秒（"90"），或数字+单位序列（"25m"、"1h30m"、"1h 30m 10s"、"2d"）
  也支持绝对时刻（"14:30" / "14:30:45"，或 Catime 写法 "14 30t" / "14t"，已过则视为明天）
  不带时长时使用配置文件中的值（默认 60 秒）

选项:
  -s, --stopwatch  以秒表模式启动
  -c, --countdown  以倒计时模式启动（默认）
  -p, --pomodoro   以番茄钟模式启动
  -k, --clock      以时钟挂件模式启动
  -r, --running    启动后立即开始计时
  --hide, --show   隐藏 / 显示挂件（只对已经在跑的那个实例有效）
  --edit, --no-edit
                   开 / 关编辑态（同上，只对已经在跑的那个有效）
  -C, --config-dir 指定配置目录（或设 TINYTICKER_CONFIG_DIR）：配置与套接字都在里面，
                   于是可以同时跑两套互不相干的挂件
  --and <命令>     武装一条一次性结束命令（别名 --then）：下次结束执行一次就失效，不落盘
  --pause          暂停在跑的计时器
  --toggle         运行中则暂停、否则开始（快捷键最爱绑这条）
  --reset          把读数拨回起点（不开始）
  --centis / --no-centis
                   开 / 关百分之一秒：设定值且进配置，重复绑同一条快捷键结果不变
  -h, --help       显示帮助
  -V, --version    显示版本
```

示例：

```sh
tinyticker 25m          # 25 分钟倒计时
tinyticker 14:30        # 倒计时到今天 14:30
tinyticker -p -r        # 立即开始番茄钟
tinyticker -k           # 桌面时钟挂件
tinyticker --hide       # 把在跑的挂件藏起来（计时照跑），再 --show 放回来

# 已经开着挂件时，上面这些命令是「给在跑的那个下命令」而不是再开一个窗口：
tinyticker 25m          # → 当前那个立刻开始 25 分钟倒计时
tinyticker -k           # → 当前那个切到时钟模式
tinyticker --toggle     # → 当前那个暂停/继续（还有 --pause / --reset / --centis）
```

把 `tinyticker 25m` 绑进 KDE / GNOME / niri 的自定义快捷键，就等于有了全局快捷键——
我们不需要实现任何快捷键协议，只需要那个套接字。转发成功时会在 stderr 留一行
`已把这条命令交给正在运行的实例 …（本进程不另开窗口）`，免得看不出窗口为什么没变多；
想知道是谁在听，看 `/run/user/1000/tinyticker.sock` 与 `pgrep -x tinyticker` 即可。

**悬浮窗交互**：左键按住拖动，右键关闭，滚轮缩放，中键进编辑态（编辑态下右键只退出编辑态）。

**托盘交互**：左键开始/暂停，中键重置，右键打开完整菜单，在图标上滚动即可缩放。为此 `ItemIsMenu`
报的是 `false`——规范要求「只有菜单、没有自己的激活行为」的项才报 `true`，而宿主据此会把左键
直接吞成弹菜单，`Activate` 就永远到不了我们这里。挂件上没有做「点击输入时长」：那需要一个文本
输入框，而本项目拿不到键盘（layer-shell 的键盘交互恒为 0）；设时长走托盘预设或命令行参数。
透明度与配色走托盘「外观」子菜单，点一下即时生效；直接改 `bg_alpha` / `color_*` 也一样——
配置文件是**热加载**的，手改之后一个心跳以内就应用，不必重启。

## 配置

配置文件退出时自动写回（写盘走"临时文件 + `rename`"的原子替换），手动编辑则即时生效：

```
$XDG_CONFIG_HOME/tinyticker/config.conf      # 该变量缺失或不是绝对路径时
~/.config/tinyticker/config.conf             # 退回 $HOME/.config
```

实际解析出的路径可由 `tinyticker -h` 查看。本项目只做 Linux，不再为其它系统的目录惯例保留代码。

```ini
duration = 1500        # 倒计时总时长（秒），支持 "25m" 写法
mode = countdown       # countdown | stopwatch | pomodoro | clock
bg_alpha = 0           # 背景不透明度 0-255：0 全透明（只剩文字），255 不透明
zoom = 1.0             # 窗口缩放倍数 0.5-6.0（滚轮调节；200×100 的逻辑画布放到 1200×600）
click_through = false  # 鼠标穿透：true 则只有文字处可点（不挡下方窗口），
                       # 但拖动也要点中文字；false（默认）整窗可拖动
clock_12h = false      # 时钟挂件用 12 小时制（带 AM/PM）；false 为 24 小时制
clock_seconds = true   # 时钟挂件显示到秒（false 则只到分：17:31 / 05:31 PM）
time_pad = none        # 计时数字的补零档：none 45s/12:34 | zero 00:45/12:34
                       # | full 00:00:45。zero 与 full 同时去掉 `s` 后缀，宽度固定不抖。
                       # 也能从托盘「外观 → 时间格式」直接点选，改完即时生效
centiseconds = false   # 数字行显示百分之一秒：45.32s / 12:34.56 / 1:01:01.01。
                       # 计时模式只在跑动时把心跳从 200ms 提到 20ms（实测单核 0.9%，整秒档 < 0.1%）；
                       # 时钟挂件也吃这一档（HH:MM:SS.cc，关了 clock_seconds 则百分秒一并压掉）。
                       # 也能从托盘「外观 → 时间格式 → 百分之一秒」或 `--centis` 直接设定，即时生效
tray_icon = clock      # 托盘图标：clock 真实时表盘 | cpu | memory | battery 水位占用表
                       # | network 网络速率水位（对数刻度，1 KB/s 空盘 ~ 10 MB/s 满盘）
                       # | gif 动图。也能从托盘「外观 → 图标内容」直接点选，改完即时生效。
                       # 占用表每秒重绘，颜色按 <60% 绿 / <85% 黄 / 其余红分级；
                       # 充电中一律绿，没有电池选 battery 则显示空心环
tray_numbers = false   # 那四档占用指标改用**数字**画，而不是水位（表盘与动图不受影响）。
                       # 百分比写 `42%`；网络是上下两行，上面绿的是下行流量、下面琥珀的是
                       # 上行（`1.1M` / `332K`），与悬停提示里 ↓↑ 的先后一致。数字档的底是
                       # 圆角方块而不是圆盘——内盘半径 12.5，在 y=7 那一行只剩 18 px，
                       # 一行速率要 32 px，字会骑到表圈上。也能从托盘「外观 → 图标内容」
                       # 末尾那一项直接勾选
tray_throttle = off    # 动图按哪个指标限速：off | cpu | memory | timer | fixed（五档对齐
                       # Catime 的 ANIMATION_SPEED_METRIC）。半载以下完全原速，半载到满载
                       # 线性掉到 1/4 速（是"变慢"而不是跳帧：动图走自己的虚拟时钟，每拍按
                       # 倍率累加）。timer 档拿倒计时进度当负载——越到后段动图越慢；fixed 档
                       # 不跟指标，直接按 tray_gif_speed 的倍率走（五档里唯一能变快的）。
                       # 曲线写死不可改——改它需要键盘与一个曲线编辑器。也能从
                       # 托盘「外观 → 图标内容 → 动图限速」单选五档
tray_gif_speed = 200   # fixed 档的倍率百分数（10-200；默认 200 = 双倍速，与 Catime 的
                       # ANIMATION_FIXED_SPEED_PERCENT 默认值一致）
tray_gif = ~/pics/spin.gif   # tray_icon = gif 时的动图路径，支持开头的 ~/。
                       # GIF / PNG / APNG 都行，指一个目录 = 帧序列源（文件名升序、
                       # 每张图一帧）。文件换了会自动重新解码；单文件最大 2 MB
pomo_work = 1500       # 番茄钟专注时长（秒）
pomo_break = 300       # 番茄钟短休息时长（秒）
pomo_long_break = 900  # 番茄钟长休息时长（秒）：每跑满 pomo_rounds 轮专注后插一次
                       # 0 = 不安排长休息，每轮后面都跟短休息
pomo_rounds = 4        # 一「组」有几轮专注（1-100），即每几轮插一次长休息
pomo_cycles = 0        # 跑满几组后自动收工显示 DONE（0-100）
                       # 0（默认）= 不限，一直轮转——和旧版行为一致
pomo_seq =             # 自己排节奏：一串任意时长，逗号或空格分隔（`25m,5m,15m`）。
                       # 写了它，上面四个键就整条让位——一段接一段跑，跑完整条算一遍，
                       # `pomo_cycles` 限定跑几遍（0 = 一直轮转）。状态行显示 `POMO n`，
                       # 中间段只提醒"这一段结束"，不触发 `on_finish` 也不消费 `--and`；
                       # 收工那一刻才算计时结束。上限 16 段、单段 ≤24 小时，
                       # 任一段非法整条不采信（与 `presets` 同规矩）；留空回到经典配方
presets = 60, 300, 900, 1500, 2700, 3600
                       # 托盘「时长预设」子菜单的档位，点击即重置并开始
                       # 逗号或空格分隔，每段支持 "25m" / "1h30m" 写法；最多 50 段（与 Catime 同档）、
                       # 单段不超过 24 小时。任一段非法则整条作废、回到上面这 6 档
                       # 菜单标签按时长自动生成（90 → "1 分 30 秒"，5400 → "1 小时 30 分"；
                       # 英文档走紧凑单位 90 → "1m 30s"）；超 20 段余下的折进「更多 ▸」
notify = true          # 计时结束发不发桌面通知；false 则一声不响（on_finish 与提示音照旧）
notify_text = 该起来了 # 通知正文写死成这句话（可选）；不写则五种事件各用默认文案
timeout_text = 时间到  # 到点把数字行整行换成这句话（可选，可中文）
                       # "0" = 数字行留空（状态行仍写 DONE）；不写则照旧显示 0s / 0.00s
                       # 走字形层所以支持中文；热加载即时生效
on_finish = loginctl lock-session   # 计时结束执行的命令（可选，省略则只通知）
                       # 经 sh -c 解释，例如锁屏 loginctl lock-session、关机 systemctl poweroff
alarm_sound = beep     # 计时结束的提示音（可选）：beep / SYSTEM_BEEP = 合成两声短音，
                       # 其它值 = WAV 路径（支持 ~/，PCM 8/16/24/32 bit 与 IEEE float；
                       # MP3 不支持）；不写 = 静默。与 notify 各走各的开关，
                       # 编辑态到点静默时一并闭嘴
alarm_volume = 100     # 提示音音量 0-100（0 与没配同义；对齐 Catime 的 NOTIFICATION_SOUND_VOLUME）
text_source = ~/tmp/build.out       # 外部文本源（可选）：第一行顶替状态行，支持开头的 ~/
                       # 截断按**实测像素宽**算，不是按字数——中文一格放不下就整字退让
text_font = auto       # 点阵补不到的码位（中文、Latin-1、符号）用什么字形
                       # auto = dlopen libfreetype 并自动找一块中文字体（默认）
                       # off  = 完全不用外部字体，非 ASCII 一律留空位
                       # 其它值 = 字体文件路径，支持开头的 ~/；打不开会警告并退回自动发现
                       # 数字行恒用 8x8 点阵，这个开关只影响状态行里点阵没有的那些字
status_font_px = 12    # 状态行里 TTF 字形的像素高 = 这一项 × 字号格（8-24，默认 12）。
                       # 只动点阵补不到的那些字，数字行的点阵质感不受影响；热加载即时生效
language = auto        # 托盘文案（菜单/悬停提示/通知）的语言：auto 按 $LANGUAGE/$LC_ALL/
                       # $LC_MESSAGES/$LANG 判（zh 前缀中文，否则英文），或直接写 zh / en。
                       # 也能从托盘「语言」子菜单三档单选，切换即时重建菜单标签；
                       # 挂件上那两行字不受它管（数字行恒 ASCII，状态行写什么由内容决定）
color_bg = 0f0f14      # 背景色。四种写法都收：`#RRGGBB` / `0xRRGGBB` / 裸 6 位、`#RGB` 三位
                       # 简写（每位翻倍，与 CSS 同规则）、CSS 颜色名（那 30 条，大小写无关，
                       # 取值逐条照 Catime 的名表抄，所以它的配置能直接搬）、以及
                       # `rgb(80, 220, 120)` 或省掉前缀的裸三元组（分隔符收 `, ; | 空格`
                       # 与全角逗号分号，同 Catime 口径）
color_running = ffffff # 运行中；可写渐变，如 #FF5E96_#56C6FF（`_` 分隔 2-20 个停靠点，
                       # 横向铺满文字采样；超过 2 个会自动流动，与 Catime 同规则）。
                       # 也可直接写 Catime 的具名渐变：candy / breeze / frost / sunset /
                       # streamer（大小写无关，GRADIENT_ 前缀也认），写回配置时展开成停靠点串
color_paused = ffc850  # 暂停（同上，可写渐变）
color_done = 50dc78    # 结束（同上，可写渐变）
text_effect = none     # 文字特效：none | glow | glass | neon | holographic | liquid | aqua | retro
                       # 辉光 / 玻璃 / 霓虹 / 全息 / 液态 / 水波 / 复古投影，值串与 Catime 的
                       # TEXT_EFFECT 逐字一致。液态与水波会动，此时心跳自动从 200ms 提到 50ms
                       # 也能从托盘「外观 → 文字特效」直接点选，改完即时生效
                       # 注意：特效的模糊半径与位移量都按 8x8 位图字形的尺寸重定过，
                       # 不是照抄 Catime 那套针对几十像素 TTF 字形的常数
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
│   ├── freetype.rs # libfreetype 声明：只按探测出的字节偏移读需要的字段，不镜像整份结构体
│   ├── alsa.rs    # libasound 声明：snd_pcm_set_params 一个高层入口代掉整台配置状态机
│   └── x11.rs     # libX11 / libXext 声明（事件 union 与结构体逐字段照 Xlib.h）
├── x11.rs     # X11 客户端：直调 libX11，override-redirect ARGB 窗口
│              #   XPutImage 软渲染 + 拖动/滚轮/右键 + 点击穿透（libXext SHAPE），仅 `x11` feature
├── widget.rs  # 与后端无关的挂件核心（计时推进、帧内容与布局）
├── wake.rs    # self-pipe 唤醒器：空闲档 1s 心跳下，托盘/套接字的命令也能即时结算
├── clock.rs   # 本地时间读取（自研 TZif + POSIX 规则解析，不依赖 C 库时区 API）
├── sysinfo.rs # 系统状态采样（CPU 差分 / MemAvailable / 电量 / 开机时长），只读 /proc 与 /sys
├── anim.rs    # 动图容器的共同形状：帧序列 + 虚拟时钟播放器，按文件魔数分发解码；
│              #   路径是目录时按"帧序列源"逐张成帧
├── gif.rs     # 最小 GIF89a 解码器（LZW + 交错 + 子矩形 + disposal）
├── png.rs     # 最小 PNG / APNG 解码器（自研 DEFLATE + 五种行滤波 + dispose/blend）
├── audio.rs   # 到点提示音：WAV 解码与合成 beep，ALSA 后台播放（不挡主循环）
├── textsrc.rs # 外部文本源：读一个文件的第一行顶替状态行，带大小上限与变化检测
├── ipc.rs     # 命令行意图解析 + 单实例转发（Unix 套接字）；二次启动 = 下命令
├── lang.rs    # 托盘文案的双语层（菜单/悬停提示/通知；挂件那两行不受它管）
├── timer.rs   # 计时状态机（倒计时 / 秒表 / 番茄钟 / 时钟），含走时补齐
├── text.rs    # 字形层：8x8 点阵优先，点阵没有的码位交给 libfreetype；含字体自动发现
├── render.rs  # 像素画布、字形绘制（点阵整数放大 / 灰度覆盖度贴图）、时间格式化
├── config.rs  # 配置持久化（纯 std 文件 IO）
├── parse.rs   # 相对时长与绝对时刻解析
├── tray/      # 自研托盘（见下）
│   ├── mod.rs      # 公共出口 + 托盘线程主体 + 总线消息路由
│   ├── sni.rs      # StatusNotifierItem 属性、图标像素送出、注册、通知与 ActionInvoked
│   ├── menu.rs     # 菜单节点表 + 托盘自持的那份勾选态
│   ├── dbusmenu.rs # com.canonical.dbusmenu 的布局读写与点击/滚轮参数解析
│   ├── icon.rs     # 32x32 图标的像素绘制 + 悬停提示文本
│   └── wire.rs     # libdbus 迭代器的小工具（开闭容器、放变体、打包回复）
└── font8x8.rs # 内置 ASCII 位图字体

packaging/     # 打包资产
├── io.github.panzhifu.tinyticker.desktop      # 应用菜单入口
├── io.github.panzhifu.tinyticker.svg          # 图标（与托盘图标同款）
├── io.github.panzhifu.tinyticker.metainfo.xml # AppStream 元数据
└── PKGBUILD                                   # Arch 包（AUR 提交形态，tag 源码构建）
```

技术栈：Wayland 侧不用任何 Rust GUI/协议库——`src/sys/wayland.rs` 声明 libwayland-client 的 C API 并在运行时 dlopen，扩展协议（layer-shell / viewporter / fractional-scale / relative-pointer / cursor-shape）的接口描述符按协议 XML 手写。托盘与通知同理：`src/sys/dbus.rs` 声明 libdbus-1 的 C API，`src/tray/` 自己实现 StatusNotifierItem + com.canonical.dbusmenu + 通知发送。字形也是同一手法：`src/sys/freetype.rs` 运行时 dlopen `libfreetype.so.6` 取六个函数，按 `offsetof` 探测出的偏移读槽位字段，取不到库或字体就退回内置点阵。声音同理：`src/sys/alsa.rs` dlopen `libasound.so.2` 取六个符号。因此 **Wayland 极小版零第三方 crate，`ldd` 只有 libc、libm 与 libgcc_s**——构建不需要任何 `-dev` 包，freetype 与 fontconfig 一样都是发行版自带的运行时库。

X11 侧同理：`src/sys/x11.rs` 逐字段照 `Xlib.h` 镜像 ABI，`src/x11.rs` 自己建 override-redirect 的 depth-32 ARGB 窗口、`XPutImage` 软渲染、`XShapeCombineRectangles` 做点击穿透，拖动靠 `XGrabPointer` + 根坐标。因此**两种构建都零第三方 crate**（`Cargo.lock` 里只有 tinyticker 自己），实测只硬链 `libc`、`libm` 与 `libgcc_s`，X11 / Wayland / D-Bus / FreeType / ALSA 全部运行时 dlopen。

## 与 Catime 的对照

本项目以 [Catime](https://github.com/vladelaina/Catime)（Windows 计时工具箱）为功能基准重写：

| 文档 | 回答什么 |
|---|---|
| [COMPARISON.md](COMPARISON.md) | 谁有什么——逐行功能对照表 |
| [GAP.md](GAP.md) | 差在哪、为什么差、补齐要付多少——逐文件读码的代码级差距分析，带 `文件:行` 取证 |

截至 v0.6.0：功能面已对齐或反超；剩余差距全部记录在 GAP.md 并附"为什么不做"（三条根因：没有键盘通路、数字行是刻意保留的 8x8 点阵、不引入进程外能力/攻击面）。

## 已知限制

- **计时数字只有 ASCII**：那是刻意的——8x8 点阵按整数倍放大才有这套像素风，且所有文字特效的模糊半径与位移量都是照 `8 × scale` 的字形尺寸标定的。唯一会进数字行的非 ASCII 是 `timeout_text`：那句到点顶替的话由字形层补 TTF 字形（实测中文正常，只是那一行不再是点阵质感）
- 状态行的非 ASCII 依赖机器上有 `libfreetype.so.6` 和一块中文字体；两者任一缺失就退回原行为（非 ASCII 留空位），不会报错也不影响其余功能。`text_font = off` 可以显式要这个原行为
- **不做文本 shaping**：逐码位查字形、水平推进，所以连字、从右到左书写、以及"字母 + 组合附加符"这类需要排字引擎的写法都不处理。状态行只显示它拿到的那一行，不换行
- 时钟挂件的百分秒在 12 小时制下是 14 字符，比默认窗口宽：超出的部分按实测像素宽整字退让（不画半个数字），想要完整读数就把窗口缩放调大一档
- 托盘动图**只支持 GIF / PNG / APNG**：WebP 要整个 VP8 codec、JPG 要 DCT，为零第三方依赖的二进制都不划算（Catime 那几个格式是靠 Windows 自带的 WIC 解的，Linux 没有等价物）。PNG 也不支持隔行（Adam7）——图标没理由用 7 遍重排的编码，遇到退回静态表盘。自写解码器还设了界：画布 ≤128×128、帧数/目录张数 ≤64、单文件 ≤2 MB，超出一律拒收
- 提示音**只解 WAV**（PCM 8/16/24/32 bit 与 IEEE float）：MP3 需要整个解码器（Catime 那边是 vendored miniaudio 替它解的），与体积卖点冲突；`beep` 哨兵是合成音，不依赖任何系统提示音服务。真机上有 PipeWire-only 且不装 ALSA 兼容层的机器会放不出声——那只会差一行 stderr 警告
- `tray_icon = gif` 但 `tray_gif` 没配（或文件/目录不可用）时，图标退回真实时表盘；托盘菜单里那一档会**置灰**并把原因写进标签（「动图（未配置 tray_gif）」），点了不会有动作
- 鼠标穿透为可选项：默认整窗接收输入（好拖动），开启 `click_through` 后只有文字可点——二者不可兼得
- **Wayland 上"隐藏"不是真的卸载窗口**：layer-shell 规定未锚定的表面尺寸为 0 就是协议错误（`attach(NULL)` 会被 niri 直接掐断客户端），所以隐藏 = 提交一帧全透明像素 + 空输入区域。看不见也点不到，但合成器的窗口列表里它还在（X11 侧是正经的 `XUnmapWindow`，没这个痕迹）
- 我们自己不注册全局快捷键（Wayland 无统一协议，X11 走 `XGrabKey` 又会与合成器抢键）。替代路径是单实例套接字：把 `tinyticker 25m` 绑进 DE 的自定义快捷键即可，见「用法」一节
- 挂件上没有 Ctrl/Shift+滚轮：Wayland 的 pointer 事件不带 modifier，而 overlay 层挂件默认拿不到键盘焦点，读不到修饰键状态。连续调节一律走托盘（图标上滚动 = 缩放，菜单 = 透明度/配色），X11 侧同样如此，两端行为一致
- 依赖 layer-shell：合成器不提供该协议时（如 GNOME 裸机）直接报错退出，不再退回普通窗口——退回也没意义，全屏会被盖住
- X11 侧的透明需要合成器（picom / KWin / Xwayland）；没有合成器时背景会显示成不透明黑底。缩放系数取 `Xft.dpi / 96`，X11 没有分数缩放协议
- layer-shell 路径的挂件位置受合成器 reserved 区域影响：顶部面板（exclusive zone）会把它往下推

## 开发与发布

```sh
cargo build --release                          # Wayland + X11 通用版
cargo build --release --no-default-features    # 仅 Wayland 极小版
cargo test                                     # 230 项内联测试
cargo clippy --all-targets -- -D warnings      # CI 门槛：零告警
```

依赖只有 Rust 1.88+（edition 2024 + let-chains），任何 `-dev` 包都不需要。Arch 上也可以直接 `makepkg`（[packaging/PKGBUILD](packaging/PKGBUILD)）。

发布流程：bump `Cargo.toml` 版本 → 附注 tag（`v*`）→ push tag 自动触发 release 工作流（双产物构建 + UPX + `SHA256SUMS.txt` + freedesktop 资产上传）。AUR 侧把同一份 PKGBUILD 推到 `aur@aur.archlinux.org:tinyticker.git`（改版本时记得 `makepkg -g` 换新的 sha256sums）。

## 致谢与许可

- [Catime](https://github.com/vladelaina/Catime)——本项目的功能基准与取值来源（30 条颜色名表、5 条具名渐变、限速指标枚举、托盘行为都逐条取证自它的源码，MIT License）
- [font8x8](https://github.com/dhepper/font8x8)（`src/font8x8.rs`），MIT License
- 本项目以 MIT 许可发布，见 [LICENSE](LICENSE)
