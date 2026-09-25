# TinyTicker

极简悬浮计时器：倒计时 / 秒表 / 番茄钟 / 时钟挂件。Rust 编写，纯 CPU 软渲染，数字用内置 8x8 位图字体、状态行的中文由运行时 dlopen 的 libfreetype 补，单二进制 0.57 MB，零第三方 crate。

![平台](https://img.shields.io/badge/platform-Linux%20(Wayland%20%2F%20X11)-blue) ![许可](https://img.shields.io/badge/license-MIT-green) ![版本](https://img.shields.io/badge/version-0.5.0-brightgreen) ![CI](https://github.com/panzhifu/tinyticker/actions/workflows/ci.yml/badge.svg)

未压缩体积：**Wayland 极小版 0.56 MB**、通用版（Wayland + X11）0.57 MB——X11 后端只多 8.4 KB。UPX 压缩后的发布产物体积待下次发版按 CI 实测更新（v0.4.1 为 0.38 / 0.50 MB）。

## 功能

- **四种模式**
  - **倒计时**：归零自动停止、变绿显示 `DONE`
  - **秒表**：从 0 正计时
  - **番茄钟**：专注 / 短休息自动轮转，每跑满 4 轮专注插一次长休息；状态行显示 `WORK n` / `BREAK n` / `LONG n`，可限定只跑几组后收工，每阶段结束提醒
  - **时钟挂件**：实时显示本地时间，`HH:MM:SS` 或 12 小时制 `hh:mm:ss AM`
- **无边框透明悬浮窗**：逐像素预乘 ARGB，`bg_alpha` 从 0（全透明，只剩文字）到 255（不透明）自由调节
- **浮在全屏窗口之上**：Wayland 上走 layer-shell 的 overlay 层，网页视频全屏时依然可见（协议限制下这是唯一办法）；X11 走 `_NET_WM_STATE_ABOVE`
- **交互**：左键按住拖动、右键关闭、**滚轮缩放**（0.5–6.0，自动持久化）、**中键进编辑态**；**在托盘图标上滚动同样缩放**（SNI `Scroll`，挂件太小或开着点击穿透时更顺手）
- **外观即时可调**：托盘「外观」子菜单——5 档背景透明度 + 6 套整体配色 + **30 条数字颜色**（逐条取自 Catime 的预设，含两段与五段渐变）+ 8 种文字特效 + 5 种图标内容 + 「时间格式」（补零三档 / 时钟显示秒 / 百分之一秒），点击立刻生效并写回配置，不必重启；菜单带勾选，看得出当前用的是哪一项
- **文字特效与渐变**：`text_effect` 七种特效（辉光 / 玻璃 / 霓虹 / 全息 / 液态 / 水波 / 复古投影）全部在自研软渲染器里实现——文字先栅格化成单通道覆盖度图，在其上做可分离盒模糊、腐蚀求轮廓、梯度求边、正弦与 value-noise 位移，再按预乘 alpha 合成回画布；三色可写成 `_` 分隔的横向渐变，超过两个停靠点自动流动。不引入任何图像库，也不依赖 GPU
- **状态行能写中文**：数字行保持内置 8x8 点阵按整数倍放大的像素风（所有特效的模糊半径与位移量都是照它标定的），而点阵覆盖不到的码位——中文、Latin-1、带圈数字这类符号——由运行时 `dlopen` 出来的 libfreetype 光栅宿主字体补齐，两种字形按同一条基线混排。字体自动发现（先试 Noto Sans CJK / Source Han / 文泉驿 / 霞鹜文楷 的常见路径，再按名字偏好扫 `$XDG_DATA_HOME/fonts` 等字体根），也可以用 `text_font` 指定文件；`text_font = off` 就退回"非 ASCII 一律留空位"。整行都是 ASCII 时 freetype 一次都不会被调用，画面与以前逐字节相同
- **单实例，二次启动 = 下命令**：已经在跑的时候再敲 `tinyticker 25m` 不会多开一个窗口，而是把同样的参数交给那个实例（Unix 套接字，`$XDG_RUNTIME_DIR/tinyticker.sock`，权限 0600）。`--config-dir <目录>`（或 `TINYTICKER_CONFIG_DIR`）把配置与套接字一起搬进那个目录，于是**可以同时跑两套互不相干的挂件**。**全局快捷键因此不必我们自己实现**——Wayland 没有统一的快捷键协议，你在 KDE / GNOME / niri 的快捷键设置里绑一条 `tinyticker 25m` 就完了
- **外部文本源**：`text_source` 指向一个由别的进程写的文件，它的第一行顶替状态行（编译进度、下载百分比之类）。靠 `(大小, mtime)` 判断要不要重读，超过 64 KB 直接不采信，文件缺失或清空就交还状态行。**只读不执行**——Catime 那套插件也要用户在托盘里逐个手动启动并显式信任，我们则连启动这一步都不做，生产者自己跑
- **时长输入**：相对时长 `25m` / `1h30m` / `90` / `2d`，或**绝对时刻** `14:30`（已过则算明天）；也认 Catime 那套 `t` 后缀写法 `14 30t`
- **托盘控制**：**左键开始/暂停、中键重置**，右键打开完整菜单——时长预设（档位由 `presets` 配置，出厂 1 / 5 / 15 / 25 / 45 / 60 分钟，点击即开始）、四模式切换、外观、编辑态、退出。「隐藏挂件」与「编辑态」两格的勾会跟着套接字与挂件上的手势走（主循环往托盘线程回填，不只显示"上次点菜单的结果"）
- **托盘悬停提示带实时指标**：正文是 `CPU 12% · 内存 44% · ↓ 1.0 MB/s ↑ 2048 B/s · 电池 87%⚡`，每秒随采样刷新、变了才发 `NewToolTip`（SNI 里宿主是拉取属性的，不催就等于永远显示启动时那份）
- **托盘图标会走**：`tray_icon` 选真实时表盘（指针按本地时间摆）、CPU / 内存 / 电量 / **网络速率**的水位占用表，或**你自己的 GIF 动图**（`tray_gif`）。占用数据读 `/proc/stat`、`/proc/meminfo`、`/sys/class/power_supply`、`/proc/net/dev`（差分，排 loopback，其余网卡求和）；网络那一档是**对数水位**（1 KB/s 空盘、10 MB/s 满盘、上下行取大），线性刻度会让"挂着不动"和"满速下载"挤在同一格。图标按派发节拍重绘，像素变了才发 `NewIcon`
- **挂件可以藏起来**：托盘「👻 隐藏挂件」勾一下，或 `tinyticker --hide`（配合单实例转发，这就是你的 DE 快捷键）。藏起来时计时**照常跑**、心跳回落到最慢那一档（实测 10 秒 CPU 从 100ms 掉到 < 10ms），再点一下就回来
- **编辑态是一档临时状态**：挂件中键、托盘「🛠 编辑态」、或 `tinyticker --edit`（`--no-edit` 退回）都能进。进去之后三件事变了——**整窗接收输入**（点击穿透临时让路，于是拖和滚轮不必去瞄那两行字）、**到点静默**（只把读数换成 `DONE`，不发通知也不执行 `on_finish`，那条一次性的 `--and` 也不会被悄悄消费掉）、**右键改成退出编辑态**（否则想退出去结果把程序关了）。状态行这一档会写成 `EDIT` 提示你在哪儿。它**不进配置**：下次启动总该是普通态，一上手就"右键关不掉"是灾难。Catime 的编辑模式还要吃键盘与对话框，我们这一档只拿它"摆弄挂件"的那半边
- **结束动作**：桌面通知（带「再来一次」按钮，可用 `notify = false` 整个关掉、`notify_text` 把正文写死成一句话）+ 可选 `on_finish` shell 命令（锁屏、关机、放音乐等）。**要"这次而已"就用 `--and <命令>`**：它只武装一次、跑完自动回落、永不落盘——常驻的 `on_finish` 误留一次就每次结束都触发，这条不会
- **配置持久化**：时长、时长预设、模式、配色（含 30 条数字色）、透明度、缩放、番茄钟节奏、显示精度与格式、结束命令与通知、窗口位置，退出时自动写回；写入走"同目录临时文件 + `rename`"的**原子替换**，掉电最多退回上一版，不会留下半截配置
- **开机自启**：托盘「🚀 开机自启」勾一下就在 `~/.config/autostart/` 写好那份与打包同名的 `.desktop`（`Exec` 用可执行文件的真实路径），再点一下删掉。判据就是那个文件在不在，所以在系统设置里关掉我们也能看见
- **配置热加载**：手改 `config.conf` 不用重启——每拍一次 `stat`，靠 `(大小, mtime)` 认变更，真变了才重读并当场应用（颜色 / 透明度 / 特效 / 补零 / 百分秒 / 缩放 / 番茄节奏 / 结束命令 / 文本源）。**跑动中的计时器不会被配置文件掐掉**：模式与总时长只在它空着的时候跟着文件走。只有三样要重启：时长预设档位（托盘菜单在启动时就建好了）、托盘图标内容档、窗口位置——挂件改到这些时会打一行 `↻ 配置已热加载（…要重启才生效）` 说清楚
- **走时不漂移**：每次推进取的是自重绘心跳以来的**真实时长差**并一次算清，所以休眠恢复、高负载都不会少走；不足一秒的余量留在状态里参与下一次进位，不因心跳间隔而累积误差
- **百分之一秒（可选）**：`centiseconds = true` 后数字行走 `45.32s` / `12:34.56` / `1:01:01.01`，秒表的零头终于看得见；托盘「外观 ▸ 时间格式 ▸ 百分之一秒」勾一下同样生效并写回配置。这一档把心跳从 200ms 降到 20ms，实测（release，240×120 画布，秒表跑动 10 秒）烧 100ms CPU = **单核的 1%**，同场景的整秒档测不出来（< 10ms）——不贵，但没必要默认开着，所以默认关着，且**只在计时器真在跑时**才提频：暂停时百分位原样冻住，恢复后接着数而不是回 `.00`。显示百分秒时倒计时读向下取整的剩余量（`9.30`），隐藏时读回向上那一格（`10`），这样开局不会先跳一秒
- **补零三档**：`time_pad` = `none`（`45s` / `12:34`）· `zero`（`00:45` / `12:34`）· `full`（`00:00:45`），值串与 Catime 的 `TimeFormatType` 对齐。补零的真正用处是**宽度固定**：`none` 档读数从 `45s` 跨到 `1:00` 时整行会左右抖，另两档不会。时钟挂件另有 `clock_seconds`（false 则只到分，`17:31` / `05:31 PM`）

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

依赖：Rust 1.88+（edition 2024 + let-chains），Linux。不需要任何 X11 / Wayland / D-Bus / FreeType 的 `-dev` 包——四边都是运行时 `dlopen` 发行版自带的共享库。

```sh
cargo build --release                          # Wayland + X11 通用版（约 0.55 MB）
cargo build --release --no-default-features    # 仅 Wayland 极小版（约 0.54 MB）
```

也可直接从 [Releases](https://github.com/panzhifu/tinyticker/releases) 下载预编译二进制（见上一节，UPX 压缩）。打 tag 会自动触发构建、校验和生成与上传。

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
  -C, --config-dir 指定配置目录（或设 TINYTICKER_CONFIG_DIR）：配置与套接字都在里面，
                   于是可以同时跑两套互不相干的挂件
  --and <命令>     武装一条一次性结束命令（别名 --then）：下次结束执行一次就失效，不落盘
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
```

把 `tinyticker 25m` 绑进 KDE / GNOME / niri 的自定义快捷键，就等于有了全局快捷键——
我们不需要实现任何快捷键协议，只需要那个套接字。转发成功时会在 stderr 留一行
`已把这条命令交给正在运行的实例 …（本进程不另开窗口）`，免得看不出窗口为什么没变多；
想知道是谁在听，看 `/run/user/1000/tinyticker.sock` 与 `pgrep -x tinyticker` 即可。

悬浮窗交互：左键按住拖动，右键关闭，滚轮缩放，中键进编辑态（编辑态下右键只退出编辑态）。

托盘交互：左键开始/暂停，中键重置，右键打开完整菜单，在图标上滚动即可缩放。为此 `ItemIsMenu`
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
                       # | full 00:00:45。zero 与 full 同时去掉 `s` 后缀，宽度固定不抖
                       # 也能从托盘「外观 → 时间格式」直接点选，改完即时生效
centiseconds = false   # 数字行显示百分之一秒：45.32s / 12:34.56 / 1:01:01.01
                       # 只在倒计时 / 秒表 / 番茄钟下生效（时钟挂件恒到秒），且只在跑动时
                       # 把心跳从 200ms 提到 20ms（实测单核 0.9%，整秒档 < 0.1%）
                       # 这一项也能从托盘「外观 → 时间格式 → 百分之一秒」直接勾选，改完即时生效
tray_icon = clock      # 托盘图标：clock 真实时表盘 | cpu | memory | battery 水位占用表
                       # | network 网络速率水位（对数刻度，1 KB/s 空盘 ~ 10 MB/s 满盘）
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
notify = true          # 计时结束发不发桌面通知；false 则一声不响（on_finish 照旧执行）
notify_text = 该起来了 # 通知正文写死成这句话（可选）；不写则五种事件各用默认文案
timeout_text = 时间到       # 到点把数字行整行换成这句话（可选，可中文）
                       # "0" = 数字行留空（状态行仍写 DONE）；不写则照旧显示 0s / 0.00s
                       # 也能从配置文件热加载路径即时生效
on_finish = loginctl lock-session   # 计时结束执行的命令（可选，省略则只通知）
                       # 经 sh -c 解释，例如锁屏 loginctl lock-session、关机 systemctl poweroff
text_source = ~/tmp/build.out       # 外部文本源（可选）：第一行顶替状态行，支持开头的 ~/
                       # 截断按**实测像素宽**算，不是按字数——中文一格放不下就整字退让
text_font = auto      # 点阵补不到的码位（中文、Latin-1、符号）用什么字形
                       # auto = dlopen libfreetype 并自动找一块中文字体（默认）
                       # off  = 完全不用外部字体，非 ASCII 留空位
                       # 其它值 = 字体文件路径，支持开头的 ~/；打不开会警告并退回自动发现
                       # 数字行恒用 8x8 点阵，这个开关只影响状态行里点阵没有的那些字
color_bg = 0f0f14      # 背景色。四种写法都收：`#RRGGBB` / `0xRRGGBB` / 裸 6 位、`#RGB` 三位
                       # 简写（每位翻倍，与 CSS 同规则）、CSS 颜色名（那 30 条，大小写无关，
                       # 取值逐条照 Catime 的名表抄，所以它的配置能直接搬）、以及
                       # `rgb(80, 220, 120)` 或省掉前缀的裸三元组（分隔符收 `, ; | 空格`
                       # 与全角逗号分号，同 Catime 口径）
color_running = ffffff # 运行中；可写渐变，如 #FF5E96_#56C6FF（`_` 分隔 2-20 个停靠点，
                       # 横向铺满文字采样；超过 2 个会自动流动，与 Catime 同规则）
color_paused = ffc850  # 暂停（同上，可写渐变）
color_done = 50dc78    # 结束（同上，可写渐变）
text_effect = none     # 文字特效：none | glow | glass | neon | holographic | liquid | aqua | retro
                       # 辉光 / 玻璃 / 霓虹 / 全息 / 液态 / 水波 / 复古投影，值串与 Catime 的
                       # TEXT_EFFECT 逐字一致。液态与水波会动，此时心跳自动从 200ms 提到 50ms
                       # 这一项也能从托盘「外观 → 文字特效」直接点选，改完即时生效
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
│   └── x11.rs     # libX11 / libXext 声明（事件 union 与结构体逐字段照 Xlib.h）
├── x11.rs     # X11 客户端：直调 libX11，override-redirect ARGB 窗口
│              #   XPutImage 软渲染 + 拖动/滚轮/右键 + 点击穿透（libXext SHAPE），仅 `x11` feature
├── widget.rs  # 与后端无关的挂件核心（计时推进、帧内容与布局）
├── clock.rs   # 本地时间读取（自研 TZif + POSIX 规则解析，不依赖 C 库时区 API）
├── sysinfo.rs # 系统状态采样（CPU 差分 / MemAvailable / 电量），只读 /proc 与 /sys
├── gif.rs     # 最小 GIF89a 解码器（LZW + 交错 + 子矩形 + disposal），托盘动图图标用
├── textsrc.rs # 外部文本源：读一个文件的第一行顶替状态行，带大小上限与变化检测
├── ipc.rs     # 命令行意图解析 + 单实例转发（Unix 套接字）；二次启动 = 下命令
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

packaging/   # 随 release 发布的 freedesktop 资产
├── io.github.panzhifu.tinyticker.desktop      # 应用菜单入口
├── io.github.panzhifu.tinyticker.svg          # 图标（与托盘图标同款）
└── io.github.panzhifu.tinyticker.metainfo.xml # AppStream 元数据
```

技术栈：Wayland 侧不用任何 Rust GUI/协议库——`src/sys/wayland.rs` 声明 libwayland-client 的 C API 并在运行时 dlopen，扩展协议（layer-shell / viewporter / fractional-scale / relative-pointer / cursor-shape）的接口描述符按协议 XML 手写。托盘与通知同理：`src/sys/dbus.rs` 声明 libdbus-1 的 C API，`src/tray/` 自己实现 StatusNotifierItem + com.canonical.dbusmenu + 通知发送。字形也是同一手法：`src/sys/freetype.rs` 运行时 dlopen `libfreetype.so.6` 取六个函数，按 `offsetof` 探测出的偏移读槽位字段，取不到库或字体就退回内置点阵。因此 **Wayland 极小版零第三方 crate，`ldd` 只有 libc、libm 与 libgcc_s**——构建不需要任何 `-dev` 包，freetype 与 fontconfig 一样都是发行版自带的运行时库。

X11 侧同理：`src/sys/x11.rs` 逐字段照 `Xlib.h` 镜像 ABI，`src/x11.rs` 自己建 override-redirect 的 depth-32 ARGB 窗口、`XPutImage` 软渲染、`XShapeCombineRectangles` 做点击穿透，拖动靠 `XGrabPointer` + 根坐标。因此**两种构建都零第三方 crate**（`Cargo.lock` 里只有 tinyticker 自己），实测只硬链 `libc`、`libm` 与 `libgcc_s`，X11 / Wayland / D-Bus / FreeType 全部运行时 dlopen。

## 已知限制

与 Catime 的逐条代码级差距分析（差在哪、为什么差、补齐要付多少）见 [GAP.md](GAP.md)；功能对照表见 [COMPARISON.md](COMPARISON.md)。

- **计时数字只有 ASCII**：那是刻意的——8x8 点阵按整数倍放大才有这套像素风，且所有文字特效的模糊半径与位移量都是照 `8 × scale` 的字形尺寸标定的。唯一会进数字行的非 ASCII 是 `timeout_text`：那句到点顶替的话由字形层补 TTF 字形（实测中文正常，只是那一行不再是点阵质感）
- 状态行的非 ASCII 依赖机器上有 `libfreetype.so.6` 和一块中文字体；两者任一缺失就退回原行为（非 ASCII 留空位），不会报错也不影响其余功能。`text_font = off` 可以显式要这个原行为
- **不做文本 shaping**：逐码位查字形、水平推进，所以连字、从右到左书写、以及"字母 + 组合附加符"这类需要排字引擎的写法都不处理。状态行只显示它拿到的那一行，不换行
- 托盘动图**只支持 GIF**：WebP 要整个 VP8 codec、PNG 要 inflate + 滤波、JPG 要 DCT，为零第三方依赖的 0.5 MB 二进制都不划算（Catime 那几个格式是靠 Windows 自带的 WIC 解的，Linux 没有等价物）。自写解码器还设了界：画布 ≤128×128、帧数 ≤64、文件 ≤2 MB，超出一律拒收
- `tray_icon = gif` 但 `tray_gif` 没配（或文件不可用）时，图标退回真实时表盘；托盘菜单里那一档会**置灰**并把原因写进标签（「GIF 动图（未配置 tray_gif）」），点了不会有动作
- 鼠标穿透为可选项：默认整窗接收输入（好拖动），开启 `click_through` 后只有文字可点——二者不可兼得
- **Wayland 上"隐藏"不是真的卸载窗口**：layer-shell 规定未锚定的表面尺寸为 0 就是协议错误（`attach(NULL)` 会被 niri 直接掐断客户端），所以隐藏 = 提交一帧全透明像素 + 空输入区域。看不见也点不到，但合成器的窗口列表里它还在（X11 侧是正经的 `XUnmapWindow`，没这个痕迹）
- 百分秒不覆盖时钟挂件：那一行显示的是系统时间，读到的是 `localtime` 换算出的整秒；给时钟加亚秒要先换一条取时路径，而 `centiseconds` 对秒表的意义远大于对挂钟
- 我们自己不注册全局快捷键（Wayland 无统一协议，X11 走 `XGrabKey` 又会与合成器抢键）。替代路径是单实例套接字：把 `tinyticker 25m` 绑进 DE 的自定义快捷键即可，见「用法」一节
- 挂件上没有 Ctrl/Shift+滚轮：Wayland 的 pointer 事件不带 modifier，而 overlay 层挂件默认拿不到键盘焦点，读不到修饰键状态。连续调节一律走托盘（图标上滚动 = 缩放，菜单 = 透明度/配色），X11 侧同样如此，两端行为一致
- 依赖 layer-shell：合成器不提供该协议时（如 GNOME 裸机）直接报错退出，不再退回普通窗口——退回也没意义，全屏会被盖住
- X11 侧的透明需要合成器（picom / KWin / Xwayland）；没有合成器时背景会显示成不透明黑底。缩放系数取 `Xft.dpi / 96`，X11 没有分数缩放协议
- layer-shell 路径的挂件位置受合成器 reserved 区域影响：顶部面板（exclusive zone）会把它往下推

## 致谢与许可

- [font8x8](https://github.com/dhepper/font8x8)（`src/font8x8.rs`），MIT License
- 本项目以 MIT 许可发布，见 [LICENSE](LICENSE)
