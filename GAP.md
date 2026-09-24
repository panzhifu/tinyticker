# tinyticker ↔ Catime 功能差距（代码级）

> 生成日期：2026-09-24。配套阅读 [COMPARISON.md](COMPARISON.md)——那份是"谁有什么"的对比表，这份是"差在哪、为什么差、补要付多少"的差距分析。
>
> **取证基准**
> - tinyticker：工作区版本（含未提交的 `src/effect.rs` / `src/text.rs` / `src/ipc.rs`），157 项内联测试全通过；`cargo build --release` 实测 601 400 B（0.574 MiB），`--no-default-features` 592 640 B（0.565 MiB）。本文档写完后又长了字形层、套接字与这一批格式/图标项，见 §5.1、§九 与各表的"已做"标注。
> - Catime：本地克隆 `../Catime`，`resource/resource.h:9` 写的是 **1.6.2**（README 顶栏还停在 1.5.0），约 99 178 行 C，配置项 **92 个**（`src/config/config_defaults.c:18-129` 的 `CONFIG_METADATA[]`）。
> - 下文 Catime 的每条断言都给了 `文件:行`；tinyticker 的每条都给本仓库的 `文件:行`。凡是只读到声明没读到实现的，标注"未证"。

---

## 零、结论先行

差距**不是"少几个功能"**，而是三条根因派生出来的一棵树。Catime 92 个配置项里约六成聚在那三条根上，把它们解开，剩下的差距绝大多数是便宜的一行到一天活。

| | tinyticker | Catime |
|---|---|---|
| 语言 / 体量 | 11.3 k 行 Rust，0.57 MB | 99 k 行 C，995 KB（32 位） |
| 配置项 | 23 个键 | 92 个元数据项 + 3 个隐式段 |
| 用户输入通路 | **只有鼠标**（拖动 / 滚轮 / 左中右键）+ 托盘菜单 | 鼠标 + 键盘 + 14 个全局热键 + 25 个 `.rc` 对话框模板（另有一整套纯代码自绘的 `dialog_modern_*`） |
| 可显示字符 | 数字行走 8×8 点阵（仅 ASCII）；状态行的其它码位由运行时 dlopen 的 libfreetype 补 | 任意 Unicode，13 款内嵌 TTF + 系统字体回落 |
| 解码能力 | GIF89a（自研） | WIC（GIF/WebP/PNG/JPG/BMP/TIF/ICO）+ 手写 ANI + miniz + miniaudio + stb |

三条根因：

1. **R1 没有键盘通路。** Wayland 侧我们主动把 layer-shell 的键盘交互设为 0（`src/wl.rs:241`，注释即"永不抢焦点"），也从未 `get_keyboard`；X11 侧建窗事件掩码里根本没有 `KeyPressMask`（`src/x11.rs:115-119`）。于是所有需要输入的东西——时间输入框、HEX 调色板、插件脚本路径、Markdown 文件路径、热键编辑器——**不可达**，而不是"没做"。
2. **R2 只有 8×8 ASCII 位图字体。** `font8x8.rs` 仅 `FONT8X8_BASIC`，码位 ≥128 曾一律留空位。——**2026-09-24 已补**：`src/text.rs` 用运行时 dlopen 的 libfreetype 填点阵覆盖不到的码位，数字行仍走点阵。剩下没解决的是"任意文本进数字行"和 shaping（连字 / RTL / 组合附加符），见 §5.1。
3. **R3 没有进程外能力。** 无音频输出、无 TLS、不解码 PNG/WebP/JPG、不执行外部脚本、不连网络。`on_finish` 是唯一的外向出口（`src/config.rs`）。

---

## 一、计时内核

Catime 的计时核心比"整数秒 + 心跳"精密一档，而且它的精度是**显示层**的，不是闰秒级的过设计。

| 差距 | Catime 怎么做 | tinyticker 现状 | 障碍与成本 |
|---|---|---|---|
| **百分之一秒显示** | `TimeComponents.centiseconds`（`include/drawing/drawing_time_format.h:15-20`），`(ms%1000)/10`；`CLOCK_SHOW_MILLISECONDS`（`src/config/config_defaults.c:59`）+ 专用热键 | **已做（2026-09-24）**：亚秒余量进了状态（`Timer::cs`，`timer.rs:100`），`tick_at` 一次采样的差值同时喂给秒位与百分位（`timer.rs:316`），`display_centis`（`timer.rs:142`）→ `render::format_centis`（`render.rs:54`）出 `45.32s` / `m:ss.cc` / `h:mm:ss.cc`；心跳多了 20ms 一档（`CENTIS_INTERVAL`，`widget.rs:36`），开关是 `centiseconds` 配置项 + 托盘「外观 ▸ 百分之一秒」勾选框 | 与 Catime 还差两点：① **时钟挂件不覆盖**（那一行显示的是 `localtime` 换算出的整秒，要百分秒得先换一条取时路径）；② 没有专用热键——CLI 侧还没开这个开关，但套接字在，用户暂时只能改配置或点托盘 |
| **自适应心跳阶梯** | `GetTimerInterval()`：毫秒 20 / 动画色 66 / 时钟带秒 250 / 计时 100 / 空闲 1000（`src/config/config_misc_display.c:76-84`），且 ≤33 ms 时改用 `timeSetEvent` + `timeBeginPeriod` | 三档常量：`DRAW_INTERVAL` 200 ms、`ANIM_INTERVAL` 50 ms、`CENTIS_INTERVAL` 20 ms（#15 带来的），由 `Widget::tick_interval()` 选（`widget.rs:154`） | 20 ms 那一档已经在了，缺的是**空闲降到 1 s** 与**时钟带秒的 250 ms** 这两档下探；`timeBeginPeriod` 的等价物我们不需要——实测 20 ms 档读数按百分之一秒正常翻动，std 的定时器精度够用。**成本 XS** |
| **取整的非对称性** | 隐藏百分秒时倒计时用 **ceil**（`(remainingMs+999)/1000`），显示百分秒时用 floor（`drawing_time_format.c:100-105`）——为了不出现"开局先跳 1 秒" | **已对齐**（#15 顺带做的）：显示百分秒时读 `display_centis`（向下取整到百分位），隐藏时读 `display_secs`（还挂在 `secs` 那一格，等价于 ceil），两条路径分别是 `render.rs:54` 与 `render.rs:35`，由 `Widget::shows_centis()` 选 | 之所以自然成立：`secs` 本来就是"这一格还没走完"的向上那一格，而 `secs*100 - cs` 就是向下取整的剩余量。测试钉在 `timer.rs` 的 `countdown_centiseconds_descend_through_the_second_boundary` |
| **暂停不计时** | 恢复时把暂停时长同时加到 `g_target_end_time` 和 `g_start_time`（`timer.c:247-266`） | 暂停即冻结 `secs`，等价 | **已对齐** |
| **休眠/唤醒补时** | QPC 与 `GetTickCount64` 两条时间线比对，差 ≥200 ms 就平移截止时间（`timer.c:75-107`） | `tick_at` 按真实时长差一次算清整秒与百分位（`timer.rs:316`），余量存在 `cs` 里 | 做百分秒**不需要**把这套改成按毫秒：一次采样的差值同时喂两个粒度，本来就没有"秒与百分位不同源"的窗口。真正没解决的是 `Instant` 用的是 `CLOCK_MONOTONIC`（不含挂起时长），所以休眠期间倒计时等于跟着停住——与 Catime 平移截止时间的**结果**一致，只是我们没有它那句"检测到有休眠"的显式判定。**不改** |
| **超时动作** | 9 种枚举 `TIMEOUT_ACTION_{MESSAGE,LOCK,SHUTDOWN,RESTART,OPEN_FILE,SHOW_TIME,COUNT_UP,OPEN_WEBSITE,SLEEP}`（`include/timer/timer.h:33-43`）；关机/重启/休眠**从不落盘**，`WriteConfigTimeoutAction` 把 INI 强改回 `MESSAGE`，只在内存里武装一次（`config_core.c:133-146`）——菜单里干脆放了一行**置灰表头**写着"以下动作仅一次性"（`src/tray/tray_menu_submenus.c:58-140`）；关机/重启/休眠执行前要 `AdjustTokenPrivileges(SE_SHUTDOWN_NAME)`（`timer_events_system.c:63-103`）；打开 URL 过 scheme 白名单（`timer_events_timeout.c:84-96`） | `on_finish` 一条任意 shell（`config.rs`），能覆盖锁屏/关机/开文件 | **我们缺的是那个安全语义**而不是能力：`on_finish` 是常驻配置，一次误留就每次结束都关机。建议加显式一次性动作（跑完自动回落）。**成本 XS-S**，值得优先 |
| **到点整行换成自定义文本** | `CLOCK_TIMEOUT_TEXT`（`include/timer/timer.h:87`，`config_defaults.c:61`），`"0"` 表示隐藏数字 | 状态行硬编码 `DONE`（`widget.rs:263-275`），数字行归零 | 只能显示 ASCII（R2）。加 `timeout_text` 键即可。**成本 XS** |
| **无重复/贪睡** | 全文 grep `snooze` 零命中；普通倒计时跑完 `total` 归 0、窗口变空（`timer_events_main.c:74-78`），**不自动重启** | 归零显示 DONE 并保持 | **我们更好**。记进领先项 |

> **做 #15 时踩到的那条，记在这里免得有人再踩**：把亚秒余量搬进状态之后，第一版 `tick_at` 每次都对基准到 `now`、只把 `d.subsec_micros()/10_000` 累进 `cs`。看着没错，但心跳周期不是 10ms 的整数倍——`poll` 的超时按毫秒取整，真实周期约 19.5ms，于是每格都被截掉 0.95 个百分秒，**跑动中的读数正好慢一半**（实测 9.36 秒墙钟只走了 4.44 秒）。单位测试全绿，因为注入的都是整毫秒/整秒的差值。改法是基准只前进"被算走的那整百分秒"，零头照旧留在基准里（`timer.rs:316`），并加了一条 `sub_hundredth_heartbeat_jitter_does_not_slow_the_clock` 钉住 19.5ms 周期。**教训**：这类"取整方向"的 bug 只在真实时钟上暴露，注入假时间的测试看不见——凡是改了推进节奏的东西，都要用带时间戳的截图量一次斜率。

---

## 二、时间输入：两边语法互不兼容，且 Catime 的默认值更"反直觉"

这是唯一一个"我们和它都对了、但彼此不通用"的地方，必须在文档里写清，否则跨软件迁移的用户会踩。

| 输入 | Catime | tinyticker |
|---|---|---|
| `25` | **25 分钟**（`time_parser_advanced.c:32-83`：1 段 = 分钟） | 25 秒（`parse.rs:10-49`：裸数字 = 秒） |
| `90` | 90 分钟 | 90 秒 |
| `25s` / `25m` / `25h` | 都支持，单位字母可出现在任意位置（`time_parser.c:68-92` 只放行 `h m s t` + 数字 + 空白） | 支持 `s/m/h`，必须跟在数字后 |
| `25 30` | 25 分 30 秒 | 不支持空格分隔多段裸数字 |
| `1 30 20` | 1 时 30 分 20 秒（≤10 段的按位推断，`time_parser.c:162-216`） | `1h 30m 20s` 必须带单位 |
| `14 30t` | 倒计时到 17:20（尾部 `t` = 绝对时刻，已过则算明天，`timer.c:117-161`） | ❌ |
| `14:30` / `14:30:45` | ❌ **冒号直接被校验拒掉**（`:` 不在放行字符集里） | ✅（`parse.rs:52-73`） |
| `1.5h` / `2d`（小数、天） | ❌ 两者都不收 | ❌ 两者都不收（`parse.rs` 只认 `s/m/h`，非整数即失败） |

结论：**没有一边需要向另一边靠拢的必要**，但值得做加法——补 `t` 后缀绝对时刻和 `d` 天单位是纯增量，不破坏我们"裸数字 = 秒"的既有语义。

- **成本 XS**：`parse.rs` 加一个尾字符分支 + 一个单位表项；`parse.rs` 现有 7 项测试直接扩。
- 顺带：预设上限 `MAX_PRESETS = 24`（`config.rs:25`）vs Catime `MAX_TIME_OPTIONS 50`（`include/timer/timer.h:22-24`）。改常量的事，但我们的理由是"菜单再长就该分页了，而我们不分页"（`config.rs:24-25`）——**这条限制该保留**，除非先做托盘菜单分页（Catime 有 `tray_menu_pagination.c`）。

---

## 三、显示格式

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| 补零策略 | 三档 `TimeFormatType{DEFAULT, ZERO_PADDED, FULL_PADDED}`（`config_types.h:32-39`），按时/分/秒三档量级分别展开，注释写明是为了"防止视觉抖动"（`timer/timer.h:7`） | **已做（2026-09-24）**：`render::Pad{None,Zero,Full}` 三档，值串与 Catime 的 `none`/`zero`/`full` 对齐，`format_time` 与 `format_centis` 共用一个 `format_parts`（`render.rs:29-115`）；配置项 `time_pad`，托盘「外观 ▸ 时间格式」三选一。`Zero`/`Full` 档同时去掉 `s` 后缀——宽度固定正是这一档的意义 | 补零与百分秒是两个正交开关，四个组合都有测试钉住 |
| 隐藏秒 | `CLOCK_SHOW_SECONDS`（默认 **FALSE**，`config_defaults.c:57`）；时钟模式下关秒时连百分秒一起压掉（`drawing_time_format.c:255-263`） | **已做（2026-09-24）**：`clock_seconds`（默认 **true**，保住旧行为）+ 托盘「外观 ▸ 时间格式 ▸ 时钟显示秒」。`clock::format_clock` 把秒整段抹掉（`HH:MM` / `hh:mm AM`），不是截断字符串，AM/PM 留着 | 我们那条"关秒时连百分秒一起压掉"的规则**不适用**：百分秒只作用于计时模式，时钟挂件本来就没有亚秒（见 §一 表下的注） |
| 12/24 小时 | `CLOCK_USE_24HOUR`，12 小时制换算是 `0→12`、`>12→-12`，**全仓库没有 AM/PM 标记**（`drawing_time_format.c:51-57`） | `clock_12h` 且**带 AM/PM**（`clock.rs:558-573`，11 字符） | **我们更明确**。记进领先项 |

---

## 四、番茄钟

Catime 的番茄钟是**任意 N 段序列**，我们是**固定经典配方**——这条 COMPARISON.md 已经写了，这里补上代码边界和成本：

- `PomodoroConfig{int work_time, short_break, long_break, times[10], times_count, loop_count}`（`include/config/config_types.h:47-53`），而 `work/short/long` **只是 `times[0..2]` 的别名**（`src/config/config_misc_pomodoro.c:56-58`：赋完 `times[]` 再 `work_time = times[0]`、`short_break = times[1]`、`long_break = times[2]`）。也就是说 `POMODORO_TIME_OPTIONS` 默认值 `1500,300,1500,600`（`config_defaults.h:30`）本身就是 4 段、两个工作段。
- 段数上限 10（`MAX_POMODORO_TIMES`，`config_defaults.h:34`），每段 ≤86 400 s（`MAX_POMODORO_OPTION_SECONDS = MAX_TIME_OPTION_SECONDS`，`config_defaults.h:36`），循环数 1-100（`pomodoro_loop_dialog.rc` 的提示文本 + 输入校验）。
- 阶段推进用 `g_target_end_time += nextDuration*1000`（在**旧**截止时间上累加）来避免串联漂移（`timer_events_pomodoro.c:133-141`）。
- 判定"这拍是不是番茄钟"用的是启发式：`CLOCK_TOTAL_TIME == pomodoro_initial_times[index]`（`timer_events_pomodoro.c:37-46`）——**手设一个相同时长的一元倒计时会被误认成番茄钟**。这是我们不该抄的一处。
- 我们：`Phase{Work,Break,LongBreak}` + `round/cycles`（`timer.rs:43-49,244-275`），长休息按"跑满 `pomo_rounds` 轮"插入，收工判定挂在组界上而不是长休息本身。

差距是"序列长度 3 vs 10、以及段数自由"。**成本 S**：`Phase` 从 enum 换成 `Vec<u32>` + 索引，`pomo_status()` 的 `WORK n` 标签改成 `n/N`；顺带能删掉 `Phase` 的三态分支。真正的连带成本在托盘：Catime 为每段单独做了重设时长的对话框（`window_commands_timer.c:283-297`），我们只能整串从配置文件改（`presets` 那条"任一段非法整条作废"的规则可以直接复用）。

---

## 五、渲染、字体与文字特效

### 5.1 字体（R2）—— 2026-09-24 已补齐状态行那半边

| | Catime | tinyticker |
|---|---|---|
| 字体来源 | 13 款内嵌、**出厂即为裁剪过的 "Essence" 子集**（`resource/embedded_assets.json:47-98`），首运行解到 `%LOCALAPPDATA%\Catime\resources\fonts`；仓库里 `asset/font/original/` 是未裁剪原件，`tools/` 下**没有**子集化工具——那是在线工具（README:139-145） | 数字行：内置 8×8 点阵（仅 ASCII）。状态行：点阵没有的码位交给**宿主系统字体**，二进制里不带任何字体数据 |
| 加载 | `AddFontResourceExW(FR_PRIVATE)` + 卸载上一款（`font_manager.h:45-48`）；手写大端 TTF `name` 表解析拿家族名（`font/font_ttf_parser.c`，290 行） | 运行时 dlopen `libfreetype.so.6` 取六个函数（`src/sys/freetype.rs`）；字体靠一批已知绝对路径 + 按名字偏好扫字体根发现（`src/text.rs`），`.ttc` 里挑 `SC` 子字面以免拿到日文写法 |
| 字形缓存 | 四张独立表 + 单调世代号失效：metrics 512 / bitmap 32 / per-tag 256×4 / 失败字体负缓存 256（`drawing_text_stb_types.h:13-21`）；缺字逐字回落到主字体之外的备用字体（`drawing_text_stb.h:126`） | 一张 `(码位, 像素高) → Option<Ink>` 表，`None` 是"这字体画不出这个码位"的负缓存（`text.rs`）。回落方向相反于 Catime：**点阵优先，TTF 只补点阵没有的**，所以纯 ASCII 一帧里 freetype 零调用、画面与改动前逐字节相同 |
| 任意文本 | 可以；官方口径是"只显示数字和符号 `0-9` `:` `.`，所以字体绝大部分字形可以砍掉"（README:143） | 状态行可以（含中文）；数字行不行，且**不做 shaping**——逐码位查字形 + 水平推进，连字 / RTL / 组合附加符都不处理 |

实现取舍记在这，免得后人以为是疏忽：

- **数字行不换 TTF 是刻意的**。所有特效的模糊半径与位移量都按 `8 × scale` 的点阵格标定（`effect.rs` 的模块注释本来就写着"常数按缩放重定"），换成几十像素的轮廓字形会把这套标定前提抽掉，还会让纯 ASCII 用户的画面无故改变。
- **不内嵌字体数据**。GB2312 6.7 k 字 @16×16 是 214 KB、@24×24 约 480 KB，直接推翻 0.5 MB 的卖点；现成的点阵 CJK（unifont / 文泉驿）还都是 GPL。用宿主字体既免体积也免授权。
- 代价是 README 原来那句"无任何运行时字体或 UI 依赖"不再成立，已改。新增的是**运行时**依赖（和 libwayland / libX11 / libdbus 同构，构建仍不需要任何 `-dev` 包，`ldd` 仍只有 libc 与 libgcc_s），实测整层 **+26 KB**（552 896 → 579 520 B）。
- 真机验证过三件事：中文与点阵数字同基线混排、`text_effect = neon` 时覆盖度图吃到灰度边缘（特效作用在 mask 上，不是直接贴像素）、`text_font = off` 时非 ASCII 精确退回留空位。

### 5.2 颜色与渐变

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| 预设调色板 | `DEFAULT_COLOR_OPTIONS_INI` 实测 **30 项**：9 个纯色 + 21 条渐变（1 条 5 停靠点）（`config_constants.h:46-53`） | 6 套四色预设（`config.rs:83-90`） | **XS**：抄它 30 条的取值即可对齐，我们的 `Gradient` 语法（`_` 分隔、≤20 停靠点、>2 自动流动）本来就兼容它的格式 |
| 具名渐变预设 | `GRADIENT_{CANDY,BREEZE,FROST,SUNSET,STREAMER}` 可以直接当 `CLOCK_TEXT_COLOR` 的值写（`color/gradient.h:16-24`） | 只有十六进制停靠点 | **XS**：`Gradient::parse` 前加一张名字表 |
| CSS 颜色名 / `rgb()` | 解析器先查 CSS 名表，再试 `#RGB`/`#RRGGBB`，再试 `rgb(...)`/裸三元组，失败原样返回；RGB 分隔符连**全角逗号分号**都收（`color/color_parser.c:157-199`） | `#RRGGBB`/`0xRRGGBB`/裸 6 位（`render.rs:21-32`） | **S**，且是纯解析层，无体积压力 |
| 取色器对话框 | 真自制控件：SV 画布 + 色相条 + 预览 + HEX/R/G/B 输入 + 已存色板 + 屏幕取色（`color/color_picker_*.c`，共 6 文件；`resource/color_dialog.rc`） | ❌（R1） | **L**：屏幕取色在 Wayland 上还要 portal，不划算；HEX 输入需要键盘 |
| 纯黑改写 | 配置层把 `#000000` 静默改成 `#000001`，因为它的透明靠 `LWA_COLORKEY`，纯黑是色键（`config_recovery.c:87-91`） | 无此问题（预乘 alpha） | **不是差距，是它的历史包袱** |

### 5.3 七种文字特效

值串逐字一致（`include/text_effect.h:10-30` vs `config.rs:353-357`），但 Catime 的常数是**按几十像素 TTF 字形**定的，我们是**按 8×8 位图**重定的（`effect.rs:4-8` 已明说）。逐条对照后，"同一族观感"这个说法站得住：

| 特效 | Catime 关键参数 | tinyticker 对应 |
|---|---|---|
| Glow | padding 12、单次模糊半径 4、加法合成（`drawing_effect_glow.c:13,82-91`） | 双层模糊 `2s`@220 + `s`@150 + 本体，加法（`effect.rs`） |
| Neon | 腐蚀 2 求双线轮廓 → 模糊 2 → 模糊 12；亮核阈值 `tubeAlpha>160`、斜坡 ×3 | 腐蚀求边 → 模糊 → 白色亮核阈值 190、×4 |
| Glass | 阴影模糊 4/偏移 3、斜面偏移 2、自上而下高光 `(255-y*255/h)>>3`、折射>50 出边缘、镜面 `h³/25500`、蓝色偏置 | 位移差求上下沿、同一 `>>3` 亮度斜坡、增益 40/16 |
| Holographic | 模糊 10 做两遍；RGB 棱镜取 `i-1/i/i+1` 三采样；边缘 = `(mag*85)>>8` 再**平方**/256。`timeOffset` 被 `(void)` 丢弃 | 双次模糊 + 三色位移 + 同一"梯度幅度→平方→封顶"链 |
| Liquid | 2048 项 sin LUT、振幅 4.0 px、相位偏移 `0/682/1365`（即 1/3、2/3 周期）、涡旋 +500、质量<24 丢弃、4 次幂高光 LUT、菲涅尔与色散 | 256 项 LUT、三段正弦、相位 +85/+170、`v<24` 丢弃、增益高光 |
| Aqua | **按字高自适应**：位移 `clamp((h+2)/8,5,22)`、阴影偏移、模糊半径各自随 h 变；分形 value-noise、频率 8000 ppm、种子 11、周期 14 000 ms/7 拍、Q8 定点双线性 | 网格 value-noise + 双线性上采样、`seed=phase/14000*7`（同一周期）、位移 ∝ `scale` |
| Retro | 硬阴影偏移**恰好 3 px**，两趟，无模糊；自动对比：亮度 `<120` 用白否则黑 | 偏移 = `scale`、同一条 `(299r+587g+114b)/1000 < 120` 判据 |

> 值得抄的一条设计：Catime 用**每像素回调** `GetGlowGradientColor` 让所有特效都能吃到渐变而不是单色（`drawing_text_stb_gradient_blend.c:76-158`），并且 RETRO 的阴影色取渐变末端。我们已经是这么做的（`effect.rs` 里 retro 用 `gradient tail`）。**已对齐。**

真正未对齐的只有两处，都是它的工程而不是观感：
- **共享暂存缓冲池 + 延迟收缩**：三张静态缓冲、`CRITICAL_SECTION` 保护，`needed ≤ size/4` 且 `size ≥ 256 KiB` 时 5 s 后释放（`drawing_effect.c:8-11,81-98`）。我们每帧现分配 `Mask`。单线程模型下**不需要**，但**峰值内存**值得测一次再决定是否加缓存池。成本 S（内存收益，非功能收益）。
- **`timeSetEvent`/`timeBeginPeriod` 那套 Win32 定时器精度管理**：Wayland/X11 无对应物，心跳由我们的事件循环决定。**不适用。**

---

## 六、窗口与交互

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| **点击穿透** | ✅ **且默认开启**：非编辑态一律 `SetClickThrough(hwnd, !CLOCK_EDIT_MODE)`（`window_events.c:41`），`EndEditMode` 收尾再打开（`drag_scale_edit.c:143`）。还做了"软穿透"：`HasClickableRegions()` + 一个定时器，光标移到可点区域时才取消 `WS_EX_TRANSPARENT`（`window_visual_effects.c:164-190`） | ✅ 但**默认关闭**：`click_through` 可选，Wayland 用 `wl_region` 输入域、X11 用 SHAPE，都取两行文字的并集框（`widget.rs:76-98`） | 见 §十勘误。**我们的做法更干净**（声明式区域，无需 hover 轮询），差距只剩"要不要把默认值改成 true" |
| **编辑模式** | 显式一档状态：**双击挂件**进入（`window_message_mouse.c:257-264`），右键退出（带 250 ms  shields + 500 ms 菜单抑制，`:20-24,189-211`），Ctrl+右键 = 直接切换（`:223-226`），也可走托盘勾选或热键。进则强制置顶、去穿透、挂拖放、开亚克力模糊；出则全部还原（`src/drag_scale_edit.c:91-168`）。缩放和不透明度**只在编辑态可调**（`:167-179`） | ❌ 没有这个状态位；拖动/缩放/穿透三者各自独立开关，且**永远全开** | 见下一行。加一档"编辑态"是 **S**：托盘触发即可，不需要键盘 |
| 挂件上的手势 | 双击进编辑 · 右键退出 · Ctrl+右键切换 · 左键拖 · 编辑态滚轮缩放 · 编辑态 Ctrl+滚轮改不透明度 · Ctrl 临时用快档步长（`drag_scale_scale_input.c:44-46`） · 方向键移动，Ctrl+方向键大步长（`window_message_commands.c:20-66`） | 左键拖 · 滚轮缩放 · 右键关闭 · **中键无处理**（两端都没有，`wl.rs`/`x11.rs` 未见 BTN_MIDDLE 分支） | 它的三个滚轮手势全部依赖修饰键，我们拿不到（见本节"滚轮调透明度"行）。**但它的中键是空的，我们的中键在托盘上**（重置） |
| 托盘上的手势 | **左键 = 计时控制菜单**，**右键 = 设置菜单**（`src/tray/tray_click.c:44-48`，协议侧只解 `NIN_SELECT`/`WM_LBUTTONUP` 与 `WM_RBUTTONUP`/`WM_CONTEXTMENU`，`tray_event_protocol.c:25-29`）。**没有中键、没有双击、没有滚轮**——`CLOCK_WM_TRAY_OPACITY_WHEEL` 有个"保留消息"的名字但**没有任何发送方**（`resource_app_ids.h:34`，仅在 `window_procedure.c:167-169` 挂着处理分支） | **左键 = 开始/暂停、中键 = 重置、右键 = 菜单、图标上滚轮 = 缩放**（`tray.rs:758-776`，`ItemIsMenu` 因此报 `false`） | **我们手势更多**，见 §十一第 9 条 |
| 键盘移动 | 方向键挪窗，步长可配 `MOVE_STEP_SMALL`=10 / `_LARGE`=50（`config_defaults.c:46-47`） | ❌（R1） | **L**：要开 `keyboard-interactive` 并自解 keymap→keysym（正常要走 xkbcommon，与零依赖冲突）。**建议不做** |
| 多显示器 | 按 `WINDOW_MONITOR_ID` + 显示器内偏移存，显示器不在时特殊处理（`window_core_placement.c:176`） | 全局逻辑坐标对（`window_x/y`），Wayland 侧 layer 的 configure 宽高被丢弃（`wl.rs:794`） | Wayland 下挂件无法指定 output（layer-shell 无此请求）。**平台边界，不做** |
| 缩放范围 | `WINDOW_SCALE` **0.5-20.0**，另有独立的 `PLUGIN_SCALE`（`config_recovery_window.c:15-48`） | `zoom` 0.5-3.0（`config.rs:274-280`） | **XS**：放宽上限。但 3.0 时字形已经 24 px，再大是糊。建议 0.5-6.0 |
| 字体大小 | `CLOCK_BASE_FONT_SIZE` **8-500**（`config_recovery.c:47-52`） | 数字行恒为 `8 × scale × 2` 的点阵格，状态行的 TTF 像素高是 `12 × cell`（`text.rs`），两者都不能单独调 | 数字行是刻意保留点阵的（见 §5.1）；要"状态行字号可调"只需把 `TTF_PX_PER_CELL` 提成一个配置项，**XS** |
| 滚轮调透明度 | Ctrl+滚轮改不透明度，普通滚轮改缩放，步长各两档可配（`OPACITY_STEP_NORMAL`=1 / `_FAST`=5，`SCALE_STEP_*`=10/15） | ❌ 拿不到修饰键（Wayland pointer 事件不带 modifier，overlay 挂件也没有键盘焦点）→ 一律走托盘：图标上滚动缩放、菜单里 5 档透明度 | **平台边界**，已在 README:212 说明。差距只剩"透明度 5 档 vs 连续"——把 `ALPHA_STEPS` 换成 0/16/32/…/255 的 16 档也就是改数组。**XS** |
| 隐藏/显示 | `HOTKEY_TOGGLE_VISIBILITY` + 菜单项，`NO_DISPLAY` 启动模式（`startup_mode.c:31`） | **已做（2026-09-24）**：托盘根菜单「👻 隐藏挂件」（带勾选）+ `tinyticker --hide` / `--show` 经 #16 那条套接字下命令，所以 DE 快捷键直接绑得到。计时器照常跑，心跳回落到 200ms 档（实测隐藏时 10 秒 CPU 从 100ms 掉到 < 10ms） | **Wayland 这边不能用 `attach(NULL)` 卸载表面**：layer-shell 规定未锚定的表面尺寸为 0 就是协议错误，niri 会直接掐断客户端（实测报 `width 0 requested without setting left and right anchors`）。所以"隐藏"= 提交一帧**全透明**像素 + 一块**没加任何矩形的空 `wl_region`**（0×0 矩形也不行，`set_input_region` 会报 `invalid arguments`）。X11 侧则是正经的 `XUnmapWindow`。代价：Wayland 上表面仍然 mapped，会出现在合成器的窗口列表里，只是看不见、点不到 |
| 开机自启 | 三态 `AUTO_START_PREFERENCE = DEFAULT/ENABLED/DISABLED`（`startup_policy.h:7-14`） | 靠我们发布的 `.desktop` 让 DE 打开启自启（README:55） | **XS**：写 `~/.config/autostart/`。已经给了手工命令，做成托盘开关即可 |

---

## 七、托盘与系统监视

Catime 的托盘远不止一个图标：它是一个带**动画子系统 + 数值叠加 + 资源自适应播放速率**的独立模块（`src/tray/` 光 .c 就 77 个文件，另配 24 个头）。

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| 动图格式 | GIF / WebP / ANI 走动画路径，ICO/PNG/BMP/JPG/TIF 走静态；除 ANI 有手写 RIFF 解析（`tray_animation_decoder_ani.c:151-163`，≤512 帧）外，**全部依赖系统 WIC**（`tray_animation_decoder_wic.c:28-45`）；还支持**"一个文件夹当一段动画"**的帧序列源（`tray_animation_loader_folder.c`，205 行） | 只有 GIF87a/89a，自写 LZW + 交错 + disposal，硬界：画布 ≤128×128、≤64 帧、≤2 MB（`gif.rs:18-24`） | PNG 要 inflate + 滤波、WebP 要 VP8、JPG 要 DCT。**建议只做 PNG**：一个 minimal inflate（Catime 引 `miniz`，我们的 `sysinfo`/`clock` 经验表明这类自研约 300-400 行）+ 5 种滤波，约 +12 KB；APNG 顺带就有。WebP/JPG **不做**。成本 L |
| 播放速率被真实指标驱动 | `ANIMATION_SPEED_METRIC = ORIGINAL/MEMORY/CPU/TIMER/FIXED`（`config_types.h:24-30`），并且有一张用户可改的 **0-100 % 负载 → 速度倍率曲线**（`ANIMATION_SPEED_DEFAULT` + `ANIMATION_SPEED_MAP_10..100`，线性插值，容量 128 点，`config_animation_common.c:93-118`）——内存吃紧时托盘动画**真的会慢下来** | 固定按派发节拍重绘，`ICON_EVERY=1 s` 采样、字节变了才发 `NewIcon`（`tray.rs:592-624`） | 采样器已经有了（`sysinfo.rs`），只差把 `percent` 映成帧间隔。**S**，且是我们少数"抄了确实好玩"的项 |
| 图标里画百分数 | `CreatePercentIcon16(int percent)` 真的把 `42%` 用 `TextOutW` 烤进 16×16 DIB，字号保高缩宽，文字色 `auto` 跟随深浅色主题、背景可 `transparent`（`tray_animation_percent.h:58-69`、`percent_text.c:60-135`） | 画的是**内盘水位**（`dy >= 12.5 - p*0.25`），三档配色 80/220/120 → 255/200/80 → 240/90/90，充电一律绿（`tray.rs:410-428`） | 要烤数字进 32×32 只能用我们的位图字体，ASCII 够用。**XS-S**：在表盘/水位之外加一档"数字"。是风格取舍不是能力差距 |
| Caps Lock 指示灯 | 有一档内置图标 `__capslock__`，`A`/`a` 字形（`percent_caps.c`） | ❌ | X11 可读 LED 掩码，Wayland **无协议**（只有拿到焦点时的 modifier）。**平台分裂，不做** |
| 网络速率 | 每网卡上/下行 B/s，排除 loopback，`GetIfTable2` 动态解析并回落旧 API，独立采样线程，单位按资源管理器口径缩放（`system_monitor_network_api.c:9-116`、`utils/network_rate.h`） | **已做（2026-09-24）**：`/proc/net/dev` 差分（排 loopback，其余网卡求和），速率按**真实间隔**折算而不是按节拍假定；托盘 `tray_icon = network` 一档水位表 | 差距只剩**读数的呈现**：我们把它压成一根对数水位（1 KB/s = 空盘，10 MB/s = 满盘，上下行取大），Catime 是文字数字。独立采样线程不需要——采样本来就挂在 1s 的图标派发节拍上。数字档见 #23 |
| 磁盘 / 温度 / 每核 | ❌ 全都没有（明确只实现了 aggregate CPU、RAM、battery、network） | ❌ | 两边都缺，不是差距。若要做，`/proc/diskstats` 同样 XS |
| 菜单规模 | **两个菜单**：左键是计时控制（暂停/继续（未按运行置灰）、重新开始、隐藏/显示窗口 · 时间显示 ▸（当前时间✓ / 24 小时✓ / 显示秒✓）· 番茄钟 ▸（开始、**每一段各一项且当前段打勾**、`Loop Count: %d`、Combination）· 正计时✓ / 倒计时 · N 条快捷预设），右键是设置（编辑模式✓ · 超时动作 ▸ · 预设管理 ▸ · 热键设置 · 格式 ▸ · 字体 ▸ · 颜色 ▸（**自绘色板项**，`MFT_OWNERDRAW`，标签只有 `1..N`，`src/tray/tray_menu_format_color.c:95-144` + `window_message_menu_draw.c:27-50`）· 样式 ▸ · 插件 ▸ · 托盘图标 ▸ · 帮助 ▸ · 退出）。长的 id 区间按 **2/3 屏高自动分页成"更多 ▸"链**（`include/tray/tray_menu_pagination.h:16-55`）（`src/tray/tray_menu.c:63-285`、`tray_menu_pomodoro.c:148-200`） | 单菜单：开始/暂停/重置 · **隐藏挂件**（2026-09-24）· 时长预设 ▸ · 模式 ▸ · 外观 ▸（透明度/配色/特效/图标内容/**时间格式** 五个子菜单，全带勾选） · 退出（`tray.rs:243-338`）。**不分页**，所以预设上限 24（`config.rs:24-25` 的理由写在注释里） | 差距是"入口数"不是"能力"。**格式 ▸ 已经挂了**（补零三档 + 显示秒 + 百分秒，2026-09-24）。剩下可做的是：**每段番茄钟单独一项**、以及 `重置位置` / `恢复默认设置` 两项（Catime 的 `Reset` 会重建配置并重注册热键，`window_commands_reset.c:55-94`）。全部 **XS-S** |
| 悬停提示 | tooltip 里带 **CPU / 内存 / 上下行 / 开机时长 / 当前动画速率档** 多行文本（`tray_tooltip.c:18-53,85-149`），近图标 200 ms、远离 1000 ms 降频轮询，图标矩形靠 `Shell_NotifyIconGetRect` 缓存（`tray_events.c:21-24,89-100`） | tooltip 固定 `"TinyTicker" / "极简悬浮计时器"`（`tray.rs:916-983`） | SNI 侧我们能设 `ToolTip`，但**Wayland/X11 都拿不到光标是否在图标上**，所以只能随 `NewIcon` 一起定长更新，做不到"靠近才采样"。加两行指标 **XS** |
| 拖放导入 | `CF_HDROP` 一种格式（`ole_drop_target.c:118,175`）：`.ttf/.otf/.ttc` → `resources/fonts`，`.gif/.webp/.ani/.png/...` → `resources/animations`，递归扫目录并限条目数与体积，**恰好拖一个就自动应用**（`window_drop_import.c:13-136`、`window_drop_target.c:80-103`） | ❌ | **L**：Wayland 要 `data-device` + `primary-selection` 全套，X11 要 XDND。它这个设计很讨喜，但我们没有可导入的资产类别（唯一能用的是 GIF 动图），**收益不明确** |
| 悬停即预览 | 整套 `menu_preview` 子系统：靠 `WM_MENUSELECT` 驱动，鼠标**停在菜单项上就临时应用该值**——覆盖颜色、字体、3 档时补、百分秒、显示秒、24 小时制、全部特效、全部动画 id（`window_message_menu_preview.c:136-186`）；弹层/分隔线/置灰项直接拒绝（`:115-134`），30 ms 起效 / 50 ms 取消去抖，分组互相取消；离开菜单循环时安排还原，确定才提交。连**心跳频率都随预览变**（`menu_preview_state.c:33-49`，被 `drawing_time_format.c:228-233` 与 `config_misc_display.c:77-79` 消费） | ❌ 我们是**点一下立即生效并写回**（`tray.rs:131-141` 的 `SetAlpha`/`SetPalette`/`SetEffect`/`SetIcon`） | 这是两种哲学：它"试了再说、可反悔"，我们"所见即所得、无中间态"。要抄就得给每个可预览项做一份可撤销快照，**S**；在只有 8 种特效 / 30 套配色 / 5 档透明度的规模下，我们的方案不吃亏。**优先级低**，但顺带有个真问题值得修：托盘的勾选态是靠内存里的当前值算的，而**后端没有任何反向通道**，所以手改配置文件之后勾会漂——这也是 §八"配置热加载"那条的同一个结构性缺口 |。

---

## 八、提醒、声音、配置与外部内容

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| 通知渠道 | 三档可选并自动回落：Toast → 系统模态 → 托盘气泡（`notification.h:5-11`）；自制 Toast 是分层窗口，淡入/停留/淡出三态，宽度随文本长度算，圆角靠 16 采样覆盖率，**Toast 里的文字本身可以是流动渐变**（`notification_render_gradient.c:12-86`） | 走 `org.freedesktop.Notifications`，带"再来一次"按钮，`ActionInvoked` 经消息过滤器回主循环（`tray.rs:642-848`） | 我们把它**委托给宿主通知中心**是正确取舍（符合平台惯例）。可加一项：无通知服务时退回自绘浮层——但那需要第二个 surface。**M，低优先** |
| 声音 | miniaudio 解 MP3/WAV，三级回落（miniaudio → `PlaySoundW` → `MessageBeep`，`audio_player.h:4-8`），音量 0-100 即时生效，大文件后台加载不挡倒计时，`"SYSTEM_BEEP"` 哨兵值 | ❌ 完全无声 | **L**：Linux 上要 ALSA 或 PipeWire。可 dlopen `libasound.so.2`（与现有 `sys/` 手法同构），WAV/PCM 解码约 200 行，MP3 需自研解码（**不做**）。是差距清单里唯一"值得但昂贵"的非显示项 |
| 配置热加载 | 独立线程 `FindFirstChangeNotificationW` 监听**目录**，200 ms 去抖，再按 `(存在,mtime,大小)` 三元组复核，确认真改了才 `WM_APP_CONFIG_CHANGED` 全量重应用（`config_watcher_thread.c:104-183`）；另有 INI 层 100 ms 节流的 stat 兜底（`config_ini_read.c:8-12`）——手改文件即时生效，含颜色/特效/字体/时补/预设/番茄序列/通知/托盘动画 | ❌ 改配置必须重启 | **M**：需要 (a) 一个 inotify 读取端（std 没有，得 dlopen `libc` 的 `inotify_init1`，与现有 FFI 手法一致），(b) 后端到挂件的反向通道——**目前一条都没有**，托盘→主循环的通道是有的，可复用。是"低成本感受大幅提升"的一档 |
| 配置自愈 | 版本变更时**重建 + 回填**（逐 recognised key 拷回，未知键丢弃），含命名迁移规则表；校验失败的值会**写回**磁盘；所有写入走临时文件 + `MoveFileEx` 原子替换 + 命名互斥量，且几乎每个 `WriteConfig*` 都先比较"运行值 == 磁盘值"再决定写不写（为了避免自己触发自己的 watcher） | 未知键静默忽略、非法值回落默认（`config.rs:238-245,360`）；退出时整体重写。**原子写已补（2026-09-24）**：`write_atomic` 写同目录 `<name>.<pid>.tmp` → `sync_all` → `rename`，失败时清掉临时文件（`config.rs:242-266`） | 剩下的差距是"回填 + 迁移规则表 + 比较后再写"这三件事，而**我们目前没有 watcher**，比较后再写省不下任何 syscall，所以不做 |
| 插件 | 目录 `resources/plugins`，上限 32 个；**无 manifest、无配置项**，`PluginInfo{name,displayName,path,isRunning,lastModTime}`；认 **74 种脚本扩展名、明确拒绝编译产物**（`plugin_extensions.h:9-86`）；**启动是用户动作**：托盘插件菜单里点某一项，经 SHA-256 信任门后才 `CreateProcessW(解释器 "路径", CREATE_NO_WINDOW, SW_HIDE)`，解释器未知时回落 `ShellExecuteExW`（`plugin_process_launcher.c:22-70`）——**没有任何开机自动拉起路径**，热重载线程只盯"当前/最后一个运行中"的那一个插件（`plugin_manager_hot_reload.c:47-58`），即**同一时刻只有一个活动插件**（`g_activePluginIndex`）。数据面是插件自己目录下的 `output.txt`，被目录 watcher + `memcmp` 去抖（`plugin_data_watcher_thread.c:91-94`）；行内控制标签 `<notify>` / `<notify:type:timeout>` / `<exit>`（`plugin_data_notify_parse.c:42`）；信任列表存 INI `[PluginTrust] PLUGIN_0..31`，每项的值是"路径 + 竖线 + 64 位 sha256"，三键按钮 Cancel / Run Once / Trust && Run，>64 MiB 拒算；退出时 Job Object + Toolhelp 进程树杀（`plugin_process_tree.c:84-121`）；**计时器一启动就把插件全停掉**（`CleanupBeforeTimerAction`，`window_timer_commands.c:132-142`） | 只做显示那半边：`text_source` 指向的文件首行顶替状态行，≤64 KB，`(大小,mtime)` 变化检测，含任一控制字符整行拒收，非 UTF-8 整文件不采信（`textsrc.rs:18-90`）。**不执行任何外部程序** | 真正的差距只剩"**执行 + 信任 + 进程树回收**"这三件事，而**不是**"它替你启动、我们不启动"（那句两边都不成立，见 §十勘误 7）。Linux 侧要抄就是：显式声明一条命令 + 首次询问 + 记录 (路径, sha256) + `prctl(PR_SET_PDEATHSIG)` 或放进自建 cgroup 做进程回收。**M，且引入攻击面**，建议维持"只读不执行" |
| Markdown | 真子集渲染：链接、ATX 标题、粗/斜/粗斜/行内代码/删除线、**任务清单可勾选并写回源文件**、引用块 + GitHub 的五种 alert 类型、`<color>` 标签（≤8 停靠点逐位插值）、`<font:Name>` 标签；图片**真的走 WinHTTP 下载**（同步/异步两套，带取消代际与句柄跟踪）；命中区 ≤64 个（`src/markdown/` 共 25 个 .c） | ❌ | 依赖 R1（要能输入文件路径）与多行排版 + 富文本度量，这两样都还是空的。**XL，不做** |

---

## 九、系统集成

| 差距 | Catime | tinyticker | 成本 |
|---|---|---|---|
| **全局热键** | 14 个，全部默认 `None`（`config_defaults.c:110-123`），语法 `Ctrl+Alt+A` / `F1-F24` / `0xNN` VK / 34 个具名键；拒纯修饰键与 IME 键（`config_hotkey.c:186-207`） | ⚠️ **不注册快捷键，改由 DE 替我们做**（2026-09-24）：用户在 KDE / GNOME / niri 的自定义快捷键里绑 `tinyticker 25m`，命令经下面那条套接字进到在跑的实例。覆盖"开始某时长 / 换模式"这一大类 | 零（复用下一行那条套接字）。缺的是"暂停/继续""显隐"这类无时长语义的动作，等 #18 做完补个子命令入口 |
| 二次启动 = 下命令 | CLI 是一等入口：`s`显时间 `u`秒表 `p`番茄 `r`重启 `h`帮助 `e`编辑态 `v`显隐 `pr`暂停 `q1-q3`快捷预设 `p<N>`第 N 个预设，以及裸时间表达式（`cli.c:219-255`）；命令经 `WM_COPYDATA` **转发给已在跑的实例**而不是起第二个窗口（`window_message_commands.c:74-105`）；**非法输入不报错，直接启动默认计时器**——注释里写明"可预测的行为好过打断用户"（`cli.h:38-44`） | ✅ **已做**（2026-09-24，`src/ipc.rs`）：`$XDG_RUNTIME_DIR/tinyticker.sock`，不可用时退到 `/tmp/tinyticker-<uid>.sock`，0600；一次连接一行，参数用 U+001F 分隔（因为 `"1h 30m 10s"` 里有真空格），收端回一个 `K` 才让发端退出。命令走**既有的** `cmd_tx` 通道进主循环，**两个后端一行没改** | 已付：+12.5 KB。被 kill 留下的死套接字文件会先确认没人听再 unlink 接管 |
| 自动更新 | 手写 JSON 取值器（无库），semver 含**预发布标签**比较（`1.3.0-alpha2`，`update_parser.c:44-180`），下载 URL 过白名单，后台线程可取消，**Release Notes 用 Markdown 渲染** | ❌ 靠包管理器 / 手动 Releases | Linux 侧正确答案是"交给发行版"。若要做，`spawn curl` 取 GitHub API 就够（不引 TLS）。**S，低优先** |
| 崩溃与日志 | 分级 + 轮转（命名移位链）+ 节流 flush（≥ERROR 立即）+ `signal` 异常处理器 + 动态加载 `RtlGetVersion` 绕过 manifest 兼容谎报（`log_*.c`） | 无日志文件；X11 错误非致命化并记录（`x11.rs:39-51`）；`panic = "abort"` | **不做**。0.5 MB 的单一职责程序不需要日志轮转 |
| 国际化 | 10 个 locale，编在同一个自定义 zlib 容器里（`embedded_assets.json:6-44`），首启用系统语言、切换走**托盘「帮助 → Language」子菜单的母语名**（`简体中文/繁體中文/English/Français/Deutsch/日本語/한국어/Português/Русский/Español`，`language_def.h:27-36`），**没有语言对话框**（`window_commands_language.c:16-28`）；还带**本地化时长串**（俄语 一/少/多 三条复数规则 + CJK 去空格）（`localized_duration.c:24-97`） | 托盘文案硬编码中文；挂件画不出非 ASCII | 托盘侧 i18n **S**（一张文案表 + 语言检测，形态和它一样：子菜单而非对话框）；挂件侧受 R2 限制。它的 `preset_label`（我们的 `config.rs:56-69`）已经在做同一件事，只是只有中文 |
| CI 冒烟模式 | `--ci-smoke --ci-config-dir=<path>` 可注入配置目录并限时自退（`config_path_sources.c:68-126`、`main_ci.c`） | ❌（配置路径不可覆盖） | **XS**：加 `--config-dir` / `TINYTICKER_CONFIG` 环境变量。**建议顺手做**，它是我们自己写集成测试的前置 |

---

## 十、对 COMPARISON.md 的勘误（读码后推翻）

1. **"点击穿透 Catime ❌" 是错的。** Catime 有，而且是**默认态**：`SetClickThrough(hwnd, !CLOCK_EDIT_MODE)`（`src/window_procedure/window_events.c:41`），退出编辑模式时再打开（`src/drag_scale_edit.c:143`），另有基于 `HasClickableRegions()` 的 hover 软穿透（`src/window/window_visual_effects.c:164-190`）。我们的差异是**默认关**（整窗可拖）且用声明式输入区域——这仍是个优点，但"Catime 没有"这句得删。
2. **Catime 版本写错了**：文档记 v1.4.0，本地克隆 `resource/resource.h:9` 是 **1.6.2**。
3. **"绝对时间 14:30 两边都 ✅" 不成立**：Catime 的时间校验只放行数字 + 空格 + `h m s t`（`src/utils/time_parser.c:68-92`），冒号会被拒；它的绝对时刻写法是 `14 30t`。我们反过来不支持 `t` 后缀。两边都 ✅ 但**语法互不兼容**。
4. **"15 种预设" 应为 30**：`DEFAULT_COLOR_OPTIONS_INI`（`include/config/config_constants.h:46-53`）实测 30 条 = 9 纯色 + 21 渐变。
5. **"字体精简工具"**：工具在 Catime 官网（README:139-145），仓库 `tools/` 里只有 `optimize_icon.js`/`optimize_png.js`/`prepare_embedded_resources.js`，**不含子集化**；仓库自带的 13 款是已裁剪成品。
6. **番茄钟那句"Catime 是任意阶段序列 × 重复次数"**成立且更强：`work/short/long` 三字段只是 `times[0..2]` 的别名（`src/config/config_misc_pomodoro.c:56-58`），序列上限 10 段。
7. **"Catime 自动拉起插件脚本" 两边都说过头了**（README:21、COMPARISON.md:57）。代码里插件启动只有一条路径：用户在托盘插件菜单点某一项（`window_commands_plugin.c`，经 SHA-256 信任门 → `plugin_process_launcher.c:22-70`），**没有开机/加载时自动拉起**；热重载只重启"当前运行中的那一个"。正确的对照说法是：**它跑脚本但要求你手动逐个启动并显式信任，我们一个都不跑**。
8. **"托盘左键直接输入时长"要加限定**：Catime 左键开的是计时控制菜单（`tray_click.c:44-48`），时长输入框是从该菜单/热键 `HOTKEY_CUSTOM_COUNTDOWN` 再进一层 `ShowCountdownInputDialog`（`window_message_dialogs.c:12-20`）得到的，且**能弹框是因为它有完整的对话框与键盘通路**（我们的 R1）。
9. **"14:30 两边都 ✅"** → 见勘误 3。同时把文档里的 Catime 版本号统一改成 1.6.2。

---

## 十一、tinyticker 领先或等价

写差距文档不记这个就会跑偏。以下 10 条是本次逐行读码后确认的：

1. **点击穿透的实现质量**：我们把输入区域声明成两行文字的并集框（`widget.rs:76-98` → Wayland `wl_region` / X11 SHAPE），一次设定、无轮询；Catime 要靠定时器在 hover 时改 `WS_EX_TRANSPARENT`（因为它必须让整窗先收到鼠标消息才能判断 hover）。
2. **零第三方代码依赖**：Catime 的"纯 C"依赖 `libs/stb`（stb_truetype）、`libs/miniaudio`、`libs/miniz` 三个 vendored 库；我们的 `Cargo.lock` 里只有本项目，`ldd` 只剩 libc 与 libgcc_s。
3. **时区是自己算的**：TZif v2/v3 的 64 位块 + POSIX 规则文法（`Jn`/`n`/`M m.w.d`，含 `M3.5.0` 这类缺省写法）全部自研（`clock.rs:132-496`）；Catime 直接调 `_tzset()`/`localtime_s`（`timer.c:117-161`）。
4. **12 小时制带 AM/PM**：Catime 的 12 小时制**没有任何 AM/PM 标记**（`drawing_time_format.c:51-57`）。
5. **到期后行为**：Catime 普通倒计时跑完把 `total` 归 0、窗口**变空白**（`timer_events_main.c:74-78`）；我们显示 `DONE` 并保持，且有"再来一次"按钮。
6. **GIF 是自研解码器**：LZW + 交错 + disposal 三态 + 子矩形，且所有硬界都在解码前检查、畸形输入返回 `None` 不 panic（`gif.rs:78-455`）。Catime 靠 WIC，等于把这件事外包给操作系统。
7. **发布产物完整性**：版本化不可变产物名 + `SHA256SUMS.txt` + freedesktop 元数据；且 README 明说"产物尚未做发布签名，需要强信任链请自行 `cargo build --release`"。
8. **体积**：0.57 MB 两个产物（592 640 / 601 400 字节），含**两整套**手写窗口系统客户端 + 托盘 + 时区 + GIF + 特效管线 + 字形层 + 单实例套接字，且二进制里不含任何字体数据；995 KB 那份是 32 位且依赖 OS 的 WIC/audio/GDI+ 全家桶。X11 后端整份只值 8 760 B（601 400 − 592 640，本次实测），这就是"零依赖"在体积上的实际价格。
9. **托盘手势比 Catime 多**：我们左键=开始/暂停、中键=重置、右键=菜单、图标上滚轮=缩放四种；Catime 只有左键=计时菜单、右键=设置菜单两种，**中键无实现、双击无实现、托盘滚轮是一个没有任何发送方的保留消息**（`resource_app_ids.h:34`）。为了拿到左键激活我们还把 `ItemIsMenu` 报了 `false`（`tray.rs:916-983`）——这是 SNI 规范里正确的做法。
10. **不执行外部代码**：Catime 有 74 种脚本扩展、SHA-256 信任列表、Job Object 进程树回收这一整套（虽然启动仍需用户点），我们一个字节都不执行（`textsrc.rs:9`）。对一个常驻桌面的小组件，这是**更安全的默认**，不是能力缺失。

---

## 十二、补齐成本排序（可直接当排期表）

成本按"改动面 + 是否触碰根因"给档：`XS` 改常量/加一个键，`S` 单个模块内，`M` 跨模块或加新通道，`L` 新机制，`XL` 结构性、会推翻现有卖点。

### 第一梯队：便宜到没有理由不做

| # | 项 | 档 | 落点 |
|---|---|---|---|
| 1 | `--config-dir` / 环境变量覆盖配置路径（顺带解锁集成测试） | XS | `config.rs:187-215` |
| 2 | ~~网络速率图标档（`/proc/net/dev` 差分）~~ **已做（2026-09-24）** | XS | `sysinfo.rs` 加 parse + 差分，`tray.rs` 加 `IconMode::Network` 与对数水位。**实测 XS**：无新 syscall、无新线程，体积见 §十二 末那条更新的合计数字 |
| 3 | 调色板预设 6 → 30 套（直接取 Catime 的 30 条值） | XS | `config.rs:83-90` |
| 4 | ~~时/分/秒三档补零 + `show_seconds` 开关~~ **已做（2026-09-24）** | XS | `render::Pad` 三档 + `clock_seconds`；`format_time` / `format_centis` 共用 `format_parts`。见 §五 |
| 5 | `timeout_text`：到点把数字整行换成自定义文本，`0` = 隐藏 | XS | `widget.rs:263-288` |
| 6 | 解析器加法扩展：`t` 后缀绝对时刻、`d` 天单位（不动"裸数字 = 秒"） | XS | `parse.rs:10-73` |
| 7 | ~~配置原子写（tmp + `rename`）~~ **已做（2026-09-24）** | XS | `write_atomic`：同目录临时文件 + `sync_all` + `rename`，失败清临时文件。目录项没有 fsync，所以极端掉电的后果是"退回上一版配置"而不是"配置被写坏" |
| 8 | `zoom` 上限 3.0 → 6.0；透明度 5 档 → 16 档 | XS | `config.rs:274-280`、`config.rs:95-101` |
| 9 | 通知文案自定义 + 可关 | XS | `config.rs` + `tray.rs::notify` |
| 10 | 托盘 tooltip 带上实时指标 | XS | `tray.rs:916-983` |
| 11 | 超时动作改"一次性武装"（跑完自动回落，避免常驻关机配置） | S | `config.rs` + `main.rs` 结束分支 |
| 12 | 开机自启开关（写 `~/.config/autostart/`） | XS | `tray.rs` 新菜单项 |
| 13 | 托盘加「恢复默认设置」/「重置窗口位置」 | XS | `config.rs` 出厂值已齐；Catime 的 `Reset` 会重建配置（`window_commands_reset.c:55-94`） |
| 14 | ~~外观 ▸ 里再挂「时间格式 ▸」~~ **已做（2026-09-24）** | XS | 「时间格式 ▸」现在装着 补零三档（单选）· 时钟显示秒（勾选）· 百分之一秒（勾选，从「外观」下挪了进来——有了同族项就不该单挂）。#14 原本还包含"百分秒"，那项在 #15 时先落了 |

### 第二梯队：一天到几天，收益立竿见影

| # | 项 | 档 | 说明 |
|---|---|---|---|
| 15 | ~~**百分之一秒显示**~~ **已做（2026-09-24）** | S（实测 S） | 亚秒余量进了状态（`Timer::cs`），一次采样同时推秒位与百分位；20 ms 心跳档 + `format_centis`；开关 = `centiseconds` 配置 + 托盘「外观 ▸ 百分之一秒」。**+1.3 KB**（592,816 → 594,160 字节），实测代价是单核 0.9%（同场景整秒档 < 0.1%），比原先估的"×10"温和：只在跑动时提频，且画布就 240×120。踩过的坑见 §一 表下那条注 |
| 16 | ~~**单实例 + 参数转发（Unix 域套接字）**~~ **已做（2026-09-24）** | ~~M~~ → 实测 S | `src/ipc.rs`，+12.5 KB。**回报最高的一项兑现了**：`tinyticker 25m` 第二次调用变成给在跑的实例下命令，**全局快捷键从此不需要我们自己实现**（让用户绑 DE 快捷键）。它同时就是 #17、#18 需要的那条反向通道——而且命令复用既有的 `cmd_tx`，两个后端一行没改 |
| 17 | **配置热加载（inotify）** | M | 与 #16 共用那条反向通道。手改配置即时生效 |
| 18 | ~~隐藏 / 显示挂件~~ **已做（2026-09-24）** | S | 托盘「👻 隐藏挂件」+ `--hide` / `--show`（复用 #16 的套接字）。**没用 SNI `Status`**：那一属性说的是图标本身该不该收进溢出区，报 `Passive` 只会让用户在需要把它点回来的时候找不到图标。Wayland 侧的实现约束见 §六 表下那条 |
| 19 | 编辑态一档（强制置顶 + 关穿透 + 忽略归零） | S | 绕开 R1 拿到 Catime 编辑模式 80 % 的价值 |
| 20 | 番茄钟序列 3 段 → ≤10 段 | S | `Phase` enum 换 `Vec<u32>` + 索引；顺带删掉三态分支。做完再给托盘加"每段一项、当前段打勾"（对齐 `tray_menu_pomodoro.c:166-183`） |
| 21 | 托盘动画速率被 CPU/内存负载驱动 | S | 采样器现成 |
| 22 | 颜色解析补 CSS 名 / `rgb()` / `#RGB` | S | 纯 `render.rs:21-32` |
| 23 | 图标内容加"数字百分数"档 | S | 我们位图字体画 ASCII 数字够用 |

### 第三梯队：贵，先想清楚要不要

| # | 项 | 档 | 为什么贵 |
|---|---|---|---|
| 24 | 声音提醒（WAV/PCM） | L | dlopen `libasound.so.2` + 重采样 + 混音策略。MP3 不做 |
| 25 | PNG / APNG 托盘动图 | L | inflate + 5 种滤波，约 +12 KB（有 X11 后端 8.7 KB 作标定）。WebP / JPG 建议永久排除 |
| 26 | 键盘输入通路 | L | layer-shell `keyboard-interactive` + keymap 协议 + 自研 keysym→UTF-8（正常要走 xkbcommon，与零依赖冲突） |
| 27 | ~~任意文本 / CJK / TTF 光栅化~~ **已做（2026-09-24）** | ~~XL~~ → 实测 L | 走 dlopen libfreetype + 宿主字体，不内嵌字体数据、不引 crate，+26 KB。状态行已支持中文，数字行按设计保留点阵。剩下的缺口只有 shaping（连字 / RTL / 组合附加符）与"任意文本进数字行"，见 §5.1 |
| 28 | 插件执行 + 信任 + 进程树回收 | M-L | 能力不难，**攻击面是成本**。建议永久维持"只读不执行"（`textsrc.rs:9`） |
| 29 | Markdown 渲染 | XL | 依赖 26 + 27 |
| 30 | 拖放导入 | L | Wayland `data-device` 全套，收益不明 |

### 明确不做（平台边界，不是偷懒）

- **多显示器定位**：layer-shell 没有"把这个 surface 放到那个 output"的请求。
- **Caps Lock 指示**：Wayland 只在持有焦点时给出 modifier。
- **任务栏嵌入**（`src/taskbar_monitor/`，15 个文件）：Windows 专有做法。Linux 的对应物是面板/状态栏，那是另一个软件类别。
- **Ctrl+滚轮**：Wayland pointer 事件不带 modifier。
- **系统级自启注册 / 更新向导**：交给发行版与包管理器。

---

## 十三、下一步建议

按 #16 → #15 → #17 的顺序做：**先把套接字那条反向通道打通**（它同时解锁"二次启动下命令"、"配置热加载"、"隐藏/显示"，并且让全局快捷键这件事以"用户自己绑 DE 快捷键"的形态免费得到），再补百分秒显示（秒表的可读性），再收 #1-#14 那批一小时内可见效的。

> 2026-09-24 更新：#27（CJK / TTF，原估 XL）、#16（单实例 + 参数转发，原估 M）、**#15（百分秒显示，原估 S）**、**#2（网络速率图标）**、**#4 + #14（补零三档 / 显示秒 / 「时间格式」子菜单）**、**#7（配置原子写）**、**#18（隐藏 / 显示挂件）**已做完。第一梯队 #1-#14 里现在只剩 #1、#3、#5、#6、#8、#9、#10、#11、#12、#13 十项没动，全是 XS。
>
> #15 落下来顺手收掉的：§一 的"取整的非对称性"（当时评估为"只在做了百分秒时才成为问题"，现在两条读法都在，测试钉住）、以及心跳阶梯的 20 ms 那一档。#15 **没**收掉的：时钟挂件的百分秒（要换取时路径）、CLI 上的开关（套接字已经在了，加一个 tri-state 参数就行）。
>
> **下一步建议 #17 配置热加载**（inotify 读端 + 复用 #16 那条通道）：现在托盘能改的东西多了（补零 / 显示秒 / 百分秒 / 图标档），"改配置文件要重启"这件事的代价比之前高了一档。之后是 #19 编辑态、#20 番茄钟序列、#21 动画速率随负载、#23 数字图标档。三条根因里 **R2 的状态行那半边已闭合**，R1（键盘）与 R3（进程外能力）未动。
>
> 这一批（#15 + #2 + #4 + #14 + #7 + #18）合计 **+8.6 KB**：含 X11 的默认构建 592,816 → 601,400 字节，仅 Wayland 592,640 字节。另外顺手修了一个后端选择的老毛病：`WAYLAND_DISPLAY=`（空值，某些启动器会留）以前算"在 Wayland 上"，于是既不连 Wayland 也不退回 X11，直接报错退出——现在空值按"没有"处理（`main.rs:run_backend`）。

第三梯队以后不建议为"对齐 Catime"而做，只为"我们自己想要"而做。
