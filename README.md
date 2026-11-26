# rec-r

[![CI](https://github.com/Magicyang876/REC-R/actions/workflows/ci.yml/badge.svg)](https://github.com/Magicyang876/REC-R/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Magicyang876/REC-R)](https://github.com/Magicyang876/REC-R/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Windows 上的截图 / 录屏工具，用 Rust 写，FFmpeg 负责编码，并把能力以 **MCP server** 的形式暴露给 agent。

- 截图：显示器 / 窗口 / 任意矩形 / 整个虚拟桌面
- 录屏：同上四种目标，H.264 MP4，自动挑选最快的编码器
- 托盘 GUI：右键菜单 + 全局快捷键 + 拖拽选区浮层
- MCP：stdio 或 Streamable HTTP，10 个工具，可直接接 Claude Code / Codex / Cursor 等

## 下载

不想自己编译就用 [Releases](https://github.com/Magicyang876/REC-R/releases/latest) 里的单文件 exe
（Windows 10 1809+ / 11 x64，仍需本机有 `ffmpeg`）：

```powershell
# 装到 %LOCALAPPDATA%\Programs\Rec-R，加 PATH 和开始菜单快捷方式
.\rec-r-0.1.3-x86_64-pc-windows-gnu.exe install
.\rec-r-0.1.3-x86_64-pc-windows-gnu.exe install --autostart --http   # 顺手开机自启 + 开 MCP HTTP
```

也可以不安装：exe 放哪都行，双击进托盘，或 `rec-r shot` / `rec-r serve --stdio` 直接用。

## 为什么捕获不走 `gdigrab`

`gdigrab` 能用（7.1.5 在这台 125% DPI 的机器上抓全屏是像素精确的，实测与我们自己的抓取 SSIM 0.9994），
但它满足不了这个工具的三种目标：

- **窗口**：`gdigrab -i title=…` 录的是窗口在屏幕上的那块**区域**，被别的窗口盖住的部分就录成遮挡物；
  Windows Graphics Capture 直接拿窗口内容（可被遮挡、无捕获边框）。
- **区域**：`offset_x/offset_y` 只在桌面坐标系里有效，窗口移动、DPI 变化时容易错位；
  我们直接用 DWM 帧边界（物理像素）算出矩形，同一套坐标给截图和录屏共用。
- **一条管线**：截图和录屏共用同一个捕获层（WGC → GDI 兜底），不存在"截图准、录屏糊"的割裂。

所以画面在进程内按物理像素抓，FFmpeg 只做编码和封装：`rawvideo → pipe:0 → libx264/qsv → mp4`。
帧的节奏由我们控制（定速补帧），保证**录制时长 == 实际时长**，即使编码器一时跟不上也不会音画/时长漂移。

```
┌─ rec-r ──────────────────────────────┐        ┌─ ffmpeg ─┐
│ WGC / BitBlt → BGRA/RGBA 帧          │ stdin  │ rawvideo │
│ Pacer: 定速补帧，保证时长 == 实际时长 │ ─────► │  → mp4   │
└──────────────────────────────────────┘        └──────────┘
```

## 构建

需要 Windows 10 1809+ 和 `ffmpeg`（7.x，含 `libx264`）。

**ffmpeg 找不到也没关系**：启动时会在 PATH、scoop/chocolatey/winget 目录、`%USERPROFILE%\Tools`、
`Downloads`、`C:\ffmpeg\bin` 等常见位置找一遍，命中后把绝对路径写回配置文件
（PowerShell 或资源管理器启动的进程看不到 MSYS 里的 PATH，所以这一步是必要的）。
真没装的话报错信息会直接给出安装/配置命令。

仓库里已经固定了工具链，直接编译即可：

```bash
cargo build --release        # → target/release/rec-r.exe
```

`.cargo/config.toml`（指定 linker）是机器相关的，所以**不进版本库**——仓库里只放
`.cargo/config.toml.example`。新机器上二选一：

```bash
# 方式一：复制一份再改 linker 那一行
cp .cargo/config.toml.example .cargo/config.toml

# 方式二：干脆不要配置文件，用环境变量（优先级高于配置文件）
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=C:/msys64/mingw64/bin/gcc.exe
```

### 如果编译出来的 exe 一启动就崩

一种可能的原因是 `PATH` 中的 `dlltool.exe` 实际是 LLVM 版本，而链接器使用了 GNU ld。
本机测试中，多个 Rust 库通过 LLVM dlltool 导入同一 DLL 时，GNU ld 生成的导入描述符未覆盖所有
IAT 槽位；Windows 加载器因此不会填充这些槽位，程序可能在启动时崩溃。问题不只限于 GNU ld 2.47。

首先检查 `where.exe dlltool` 和 `dlltool --version`，并让 Rust 明确使用与 gcc 配套的 GNU dlltool：

```bash
# 路径改为本机 MinGW 的 dlltool.exe；RUSTFLAGS 会覆盖配置文件中的 rustflags
export RUSTFLAGS="-C dlltool=C:/msys64/mingw64/bin/dlltool.exe -C link-arg=-Wl,--dynamicbase"
cargo build --release
./target/release/rec-r.exe --help
```

也可以把 `"-C", "dlltool=C:/msys64/mingw64/bin/dlltool.exe"` 加到本机的
`.cargo/config.toml` 的 `rustflags` 中。或者使用 rustup 自带的 LLVM 链接器（`rust-lld`）：

```bash
# rust-lld 伪装成 ld.lld，交给 gcc 调用
mkdir -p /c/tmp/lldbin
cp "$(rustc --print sysroot)/lib/rustlib/x86_64-pc-windows-gnu/bin/rust-lld.exe" /c/tmp/lldbin/ld.lld.exe
export RUSTFLAGS="-C link-arg=-B/c/tmp/lldbin -C link-arg=-fuse-ld=lld"
cargo build --release
```

用 MSVC 工具链也行：删掉 `rust-toolchain.toml` 和 `.cargo/config.toml` 里对应条目即可
（MSVC 自带链接器，不需要 gcc）。

### 安装到系统

`target\release\rec-r.exe` 只适合在本目录里跑。想在任意终端敲 `rec-r`（以及让 MCP 客户端用
`"command": "rec-r"` 拉起），用内置的安装命令：

```bash
rec-r install                       # 拷贝 + 用户 PATH + 开始菜单快捷方式
rec-r install --autostart           # 再写 HKCU\…\Run（登录即启动托盘）
rec-r install --autostart --http    # 登录即启动 "rec-r app --http"（常驻 + MCP）
rec-r install --dir D:\Tools\Rec-R   # 换安装目录
rec-r uninstall                     # 反着拆干净：自启、快捷方式、PATH、exe
```

`install` 做四件事，每件都能单独关掉：

| 步骤 | 结果 | 关掉 |
| --- | --- | --- |
| 拷贝 exe | `%LOCALAPPDATA%\Programs\Rec-R\rec-r.exe`（当前用户，不需要管理员） | `--dir` 换目录 |
| 加 PATH | `HKCU\Environment\Path` 追加安装目录（重复执行不会重复添加） | `--no-path` |
| 开始菜单 | `…\Start Menu\Programs\Rec-R.lnk` → `rec-r.exe app --http`（双击无终端窗口） | `--no-shortcut` |
| 开机自启 | `HKCU\…\Run\Rec-R` → `"…\rec-r.exe" app [--http]` | 不加 `--autostart` |

几个要注意的点：

- **装之前先退出正在跑的托盘 app**（右键 → `Exit`）。Windows 允许读正在运行的 exe，但不允许覆盖它，
  所以往同一目录重复安装时如果托盘还在跑，会报共享冲突并提示先退出，而不是静默失败。
- PATH 是写注册表的，**已经开着的终端不会更新**，开个新终端再 `rec-r doctor` 验证。
- 装完以后 `%LOCALAPPDATA%\Programs\Rec-R\rec-r.exe` 才是“系统里的那个”，重新编译只更新
  `target\release`；要更新已安装版本就再跑一次 `rec-r install`（覆盖拷贝）。
- 配置和输出目录不跟着搬：仍是 `%APPDATA%\Rec-R\config.json` 和 `%USERPROFILE%\Videos\Rec-R`。
- `rec-r uninstall` 先关自启、删快捷方式、摘 PATH，再删 exe（`--keep-exe` 只保留文件）；
  托盘还在跑时删不掉 exe，会打印提示而不是静默跳过。

## 快速开始

```bash
rec-r install                   # 装到系统：PATH + 开始菜单（详见“安装到系统”）
rec-r shot                      # 主屏截图 → %USERPROFILE%\Videos\Rec-R\shot-*.png（同时进剪贴板）
rec-r shot -r 100,100,800,600   # 指定区域（物理像素）
rec-r shot -w cursor            # 光标下的窗口
rec-r record --seconds 5        # 主屏录 5 秒
rec-r record -w terminal        # 标题/进程名匹配窗口
rec-r record -w 1576172         # 直接用 hwnd（任何顶层窗口都能录，包括 list_windows 没列出的）
rec-r record -r                 # 拖框选区域并立刻开录（-r 不给值 = 现行选区）
rec-r region                    # 拖框选区域，打印 x,y,w,h
rec-r displays                  # 显示器列表
rec-r windows code              # 可捕获窗口列表（按标题/进程过滤）
rec-r doctor                    # 环境体检（ffmpeg、编码器实测帧率、WGC、DPI、屏幕）
rec-r config --set fps=60       # 改设置（写入 %APPDATA%\Rec-R\config.json）
rec-r autostart on --http       # 开机自启，并同时开 MCP
rec-r icon -o logo.png          # 导出 256px 图标（--ico 导出多尺寸 .ico）
```

`shot` 常用参数：`--format png|jpg|webp`、`--max-edge 1600`、`-q high`、`--inline`（base64 到 stdout）、
`--path -` 交给调用方。`record` 常用参数：`--fps 60`、`-q lossless|high|balanced|fast`、
`--engine wgc|gdi`、`--scale 1280x-2`、`--detach`。

## 托盘 GUI

```bash
rec-r app            # 托盘图标 + 快捷键
rec-r app --http     # 同时开 MCP HTTP 服务（推荐常驻这么跑）
```

双击 `rec-r.exe` 或开始菜单里的 `Rec-R` 快捷方式，等价于 `rec-r app --http`，而且**不会留一个终端窗口**：
`rec-r.exe` 是 console 子系统的（这样 `rec-r shot` 之类才能在终端里输出），双击时 Windows 会给它
分配一个专属控制台。启动时用 `GetConsoleProcessList` 判断“这个控制台只有我自己” → 认为不是从 shell
启动的，于是 `ShowWindow(SW_HIDE)` + `FreeConsole()` 把它隐藏并释放。从终端启动时控制台里还有
shell 本身（进程数 ≥ 2），所以保留终端、日志照常打在 stderr 上。

窗口化运行时日志写 `%APPDATA%\Rec-R\rec-r.log`（纯文本、无颜色码；超过 4 MB 轮转为 `rec-r.log.1`），
`printf`/`println` 也被重定向到同一个文件——否则 `println!` 会往已关闭的 stdout 句柄写而 panic。
同一个场景下重复双击的第二份实例不打印错误（看不到），而是弹一个“already running”消息框。

同一个用户下只跑一个 `rec-r app`：再启动会直接打印提示后退出（不会抢托盘图标和快捷键）。

| 快捷键（默认） | 动作 |
| --- | --- |
| `Ctrl+Alt+S` | 主屏截图 |
| `Ctrl+Alt+A` | 拖框选区截图 |
| `Ctrl+Alt+R` | 开始 / 停止主屏录制 |
| `Ctrl+Alt+Shift+W` | 开始 / 停止录制光标下的窗口 |
| `Ctrl+Alt+Shift+A` | 拖框选区并开始 / 停止录制 |
| `Ctrl+Alt+X` | **停止录制**（不管在录什么，直接停） |
| `Esc`（不需要组合键） | 取消：选区浮层里取消选择、标注窗口里整张作废、**录制中则取消录制并删掉文件** |

`Esc` 的录制取消是只在“正在录”期间挂上的低级键盘钩子（`WH_KEYBOARD_LL`），录完立刻摘掉；
它**不吞键**，前台程序照样收到 `Esc`，所以浏览器里关个弹窗也可能顺手把录制取消掉——
想换成双击 `Esc` 或加个组合键的话改 `src/gui/esc.rs` 里的判定即可。

### 截图标注

`Ctrl+Alt+A` / 菜单里的 `Screenshot -> Region`（以及 `rec-r shot -r` 的浮动选区）拖完框后不是直接存盘，
而是弹出标注窗口：截图按 1:1 显示（屏幕上放不下才等比缩小），鼠标坐标换算回物理像素，
所以标注一定落在真正存下来的那些像素上。工具栏是紧贴截图上方的独立窗口，尽量居中；
橙色外框只在标注时显示截图范围，不会写入图片或剪贴板。工具栏接近屏幕边缘时会自动调整位置。

- **贴图**（`Pin`，工具栏最左边）：把当前（已标注的）截图钉在屏幕上，窗口无边框、置顶，
  位置就是原来截图的位置。左键拖动可移动，右键或 `Esc` 关闭。贴图窗口在标注窗口关闭后仍独立存在，
  直到你自己关掉它。
- **马赛克**（默认工具）：拖一个矩形，里面按 16px 方块取平均色填充。工具栏 `Block` 在 8 / 16 / 28 之间切换，
  块越大越糊。拖动过程中有虚线预览框。
- **方框**（`Box` 工具或按 `B`）：拖一个矩形，沿边画一圈 3px 的当前颜色边框（不填充），
  用来圈重点。虚线拖动预览和颜色跟文字共用同一个 `Color`。
- **文字**（`Text` 工具或按 `T`）：鼠标移到图上时，插入位置会显示一根当前颜色的小竖条（带描边，
  深浅背景都看得见），这就是文字将要落脚的地方；点一下弹出输入框，输入框里 **`Enter` 就是 `OK`、
  `Esc` 就是 `Cancel`**（不用去点按钮），回车后把文字画在那里。
- **字体颜色**：工具栏 `Color` 按钮在 橙 / 红 / 黄 / 绿 / 蓝 / 白 / 黑 之间轮换，**默认橙色**；
  每段文字记住自己当时的颜色，改色不影响已画的。字自带反色描边（浅色配黑边、深色配白边），
  所以白色字压亮背景、黑色字压暗背景也清楚。
- `Undo` / `Ctrl+Z` 撤销上一步（马赛克、方框、文字在同一条操作栈里，可以一路撤到底）。
- `Ctrl+C`：只复制到剪贴板并关闭，不写文件；如果剪贴板不可用，会提示错误并保留编辑窗口。
- `Ctrl+S`、`Enter` 或 `Copy & Save`：保存并关闭；默认也复制到剪贴板，受 `copy_on_shot` 控制。
- `Esc` 或 `Cancel`：整张作废，什么都不写。画完方框或点击工具栏后仍可按 `Esc` 取消。
- 鼠标在图上移动时只重画那一条竖条所占的几十个像素（`WM_PAINT` 按损坏区域裁剪，背景擦除也关掉了），
  所以不会出现整块画面反复重绘的闪烁。
- 马赛克是直接改像素（存盘和剪贴板里都是糊掉的），不是盖一层半透明方块。

窗口截图、各显示器截图、延时截图没有标注环节（即拍即存），但同样会复制到剪贴板。
`copy_on_shot`（默认 `true`）是这批行为的统一开关：不想让截图进剪贴板就
`rec-r config --set copy_on_shot=false`。MCP 和 CLI 的截图走同一条管线，所以也遵守这个开关；
剪贴板被别的程序占着时只记一条 warn 日志，不影响文件落盘。

区域录制用 GDI 抓那一块矩形，并在录制期间用一圈红框标出正在录的范围。红框有三个硬要求，
缺一个就会出问题：

1. **走 GDI**：WGC 只能抓整块显示器再裁切，而 Windows 10 的采集边框关不掉，
   于是框住的是整屏而不是选区。显式传 `--engine wgc` 仍可强制走老路。
2. **框在选区外**：红框窗口比选区每边大 3px，画的是选区外面那一圈，所以录像里没有红边。
3. **不能盖住选区**：红框是**空心窗口**（`SetWindowRgn` = 外框减内框，窗口区域里根本没有
   选区那部分像素），而不是半透明叠层。早期版本用的是 `WS_EX_LAYERED` + 颜色键透明，
   结果选区上顶着一个叠层窗口时，`BitBlt`(CAPTUREBLT) 拿到的合成结果会丢掉 GPU 合成的
   内容（Electron/WebView2 的文字整块消失，录出来只有背景色）——这才是“区域录制失败”的真相。

菜单里的 "Window" / "Record window (active window)" 录的是**右键托盘之前那个活动窗口**，
因为菜单一弹出，光标和前台窗口就都跑到菜单上了；快捷键仍然取**光标下**的窗口。
两者都会跳过托盘、菜单、工具提示这些 shell 窗口，并在没有目标时直接说明原因
（以前会报 `invalid window handle`）。

窗口录制优先用 WGC；如果 WGC 对这扇窗不可用，自动退化到 PrintWindow 逐帧抓
（engine 显示 `gdi/printwindow`，帧率低一些但不丢画面）。

右键托盘图标：截图（主屏 / 各显示器 / 窗口 / 区域 / 延时）、录制（主屏 / 各显示器 / 窗口 / 区域 / 停止）、
打开输出目录、MCP 地址、**复制 MCP 配置**、环境体检、配置文件、快捷键一览、开机自启开关、
**重启**、**Exit**。
每项右侧显示当前生效的快捷键；`Exit` 就是退出程序（托盘图标随之消失）。

`Open output folder` 直接把输出目录交给 `explorer.exe` 打开（不走 `cmd /C start`，那个在 shell 关联
失效时会静默什么都不做）；`Config file / Doctor` 这类文本文件（`.json` `.txt` `.log` `.md` `.toml` `.ini` `.csv`）
固定用记事本打开，其它类型先走 `ShellExecuteW`，失败再退回记事本，总之不会“点了没反应”。

`Copy MCP config` 把一段可以直接粘进 `mcp.json` / `claude_desktop_config.json` 的
`mcpServers` 片段放进剪贴板，`command` 是**当前这个 exe 的绝对路径**（不用再手改路径）：

```json
{
  "mcpServers": {
    "rec-r": {
      "command": "C:\\Users\\me\\AppData\\Local\\Programs\\Rec-R\\rec-r.exe",
      "args": ["serve", "--stdio"]
    }
  }
}
```

`Restart` 用**同样的命令行参数**重启自己（改完配置、换过 exe、MCP 端口卡住时都用它）。
重启不能简单“起新进程再退出”：托盘图标、快捷键和 MCP 端口都还占着，新进程会判定
`already running` 立刻退出。所以旧进程先 spawn 一个
`rec-r respawn --pid <自己的 pid> -- <原来的参数>` 的隐藏助手，助手等旧进程真正退出后
再拉起新进程，参数（`--http`、`--addr` 等）原样带过去。

菜单必须是 `CreatePopupMenu` 建出来的弹出菜单：如果拿 `CreateMenu` 的菜单栏去
`TrackPopupMenu`，Windows 会画成一条 **6px 宽的竖条**（文字全丢），看起来就是“菜单显示有问题”。
绑定了快捷键的菜单项会把当前组合显示在右侧（改了配置这里也跟着变）；
开始/停止录制会给气球通知，选区项是“拖框 + 开录/停录”一步到位。

### 改快捷键

快捷键是可配置的，三种改法等价（改完重启 `rec-r app` 生效）：

```bash
rec-r config --set hotkeys.shot=Ctrl+Shift+5
rec-r config --set hotkeys.record_region=Alt+F9
rec-r config --set hotkeys.record=none          # 关掉这一个
```

或者直接编辑 `%APPDATA%\Rec-R\config.json`，或者让 agent 调 MCP 的 `config` 工具（见下）。

写法：`Ctrl`/`Control`、`Alt`、`Shift`、`Win` 任意组合 + `A`–`Z`、`0`–`9`、`F1`–`F24`、
`PrintScreen`、`Insert`/`Delete`/`Home`/`End`/`PageUp`/`PageDown`、方向键、`Space`、`Numpad0`–`Numpad9`。
`none` / 空字符串表示关掉该快捷键。某个组合被别的程序占用时，注册会自动换成加 `Shift` 的组合，
启动日志（`hotkeys: …`）和托盘菜单里显示的都是**实际生效**的组合。

### 开机自启

```bash
rec-r autostart on             # 写 HKCU\…\Run，登录后启动托盘（只有托盘，不开 MCP）
rec-r autostart on --http      # 登录后 "rec-r.exe" app --http（agent 随时能连）
rec-r autostart status         # 打印当前登记的命令行
rec-r autostart off
```

注册表位置：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `Rec-R`；
托盘菜单里的 `Start with Windows` 勾选项对应同一个开关。

## MCP

两种传输，工具集完全相同：

```bash
rec-r serve            # stdio（Claude Code / Codex / Cursor 直接拉起）
rec-r serve --http     # Streamable HTTP，默认 http://127.0.0.1:7391/mcp
rec-r app --http       # 托盘 + HTTP，常驻推荐这个
```

注册到 Claude Code：

```bash
claude mcp add rec-r -- rec-r serve
```

这也要求 `rec-r` 在 PATH 里（`rec-r install` 会处理）。嫌写路径麻烦就右键托盘图标 →
`Copy MCP config`，直接粘客户端配置，`command` 已经是当前 exe 的绝对路径。

Cursor / 其他客户端（`mcp.json`）：

```json
{
  "mcpServers": {
    "rec-r": {
      "command": "C:/X/REC-R/target/release/rec-r.exe",
      "args": ["serve"]
    }
  }
}
```

走 HTTP 的客户端用 `"url": "http://127.0.0.1:7391/mcp"`；健康检查是 `GET /health`。

| 工具 | 作用 |
| --- | --- |
| `doctor` | 环境体检：ffmpeg 路径/版本、各编码器实测帧率、WGC、DPI、显示器、输出目录 |
| `list_displays` | 显示器：index、设备名、物理坐标与尺寸、DPI、缩放、是否主屏 |
| `list_windows` | 可捕获窗口：hwnd、标题、类名、进程、pid、物理矩形 |
| `screenshot` | 截屏；`inline: true` 直接返回图片块（PNG/JPEG/WebP），否则返回路径。同样遵守 `copy_on_shot` |
| `record_start` | 开录；`engine`/`fps`/`quality`/`scale`/`max_duration_sec` 可选 |
| `record_status` | 正在录的：id、目标、已录秒数、帧数、当前文件大小；空参=全部 |
| `record_stop` | 停一路（带 id）或全部，返回最终 `frames`/`size_bytes`/`path` |
| `select_region` | 弹浮层让人拖框，返回物理像素矩形（人不在时会给超时） |
| `open_output` | 在资源管理器里打开输出目录 |
| `config` | 读/写配置：`key` 留空=读取全部，`key`+`value`=写入（含 `hotkeys.*`、`autostart`） |

目标的写法处处一致（CLI 参数和 MCP 参数同一套解析）：

- `monitor: 0` — 第几个显示器（`list_displays` 的 index）
- `window: "终端"` / `window: "0x1a2b"` / `window: "cursor"` — 标题或进程子串 / 精确 hwnd / 光标下窗口
- `region: "100,200,800,600"` — 物理像素 `x,y,w,h`；`region: "select"` 表示弹浮层现选
- 什么都不给 = 主屏

典型的 agent 用法：

```
list_windows → 找到 hwnd → record_start(window="0x1a2b") → 干活 → record_status 看帧数 → record_stop
screenshot(region="0,0,1280,720", inline=true) → 直接看图判断下一步
select_region() → 人拖完框 → region="x,y,w,h" 交给后续截图/录制
```

## 配置

`%APPDATA%\Rec-R\config.json`（首次运行自动生成；`rec-r config --path` 打印路径）：

```json
{
  "ffmpeg": "C:\\Users\\me\\Tools\\ffmpeg\\bin\\ffmpeg.exe",
  "output_dir": "C:\\Users\\me\\Videos\\Rec-R",
  "fps": 30,
  "encoder": "auto",
  "quality": "balanced",
  "screenshot_format": "png",
  "screenshot_max_edge": 0,
  "http_addr": "127.0.0.1:7391",
  "max_duration_sec": 3600,
  "tray_on_launch": true,
  "autostart": false,
  "hotkeys": {
    "shot": "Ctrl+Alt+S",
    "record": "Ctrl+Alt+R",
    "stop": "Ctrl+Alt+X",
    "record_window": "Ctrl+Alt+Shift+W",
    "shot_region": "Ctrl+Alt+A",
    "record_region": "Ctrl+Alt+Shift+A"
  }
}
```

- `encoder`：`auto` 会实测各编码器 1080p 的编码速度后选最快的（这台机器上 QSV 比 libx264 慢，
  所以 `auto` 选的是 libx264）；也可以写死 `libx264` / `h264_qsv` / `h264_nvenc` / `h264_amf`。
- `quality`：`lossless` / `high` / `balanced` / `fast`（映射到各编码器自己的参数）。
- `screenshot_max_edge`：截图最长边限制（0 = 原尺寸）。
- `copy_on_shot`：截图完成后是否把图放进剪贴板（默认 `true`）。
- 可写键列表：`rec-r config --set` 不带参数时会打印；写错键会提示合法键名。

## 测试脚本

`scripts/` 下全是可直接跑的验证脚本（Python 3，`Pillow` + `numpy`；全部用 PostMessage 注入，
不移动真实鼠标，但脚本进程必须先 `SetProcessDpiAwarenessContext(-4)`，否则坐标会被 DPI 虚拟化）：

| 脚本 | 覆盖内容 |
| --- | --- |
| `mcp_smoke.py` | 最小 MCP HTTP 客户端：`python scripts/mcp_smoke.py http://127.0.0.1:7391/mcp doctor list_displays` |
| `mcp_flow.py` | 截图（内联图片）+ 录制开始/进度/停止的完整流程 |
| `mcp_stdio.py` | stdio 模式启动 `rec-r serve` 并发 JSON-RPC |
| `overlay_test.py` | 通过 MCP 触发选区浮层，用 PostMessage 注入拖拽/Esc，断言返回矩形与取消 |
| `tray_test.py` | 托盘菜单命令（截图 / 录制开关）产出文件 |
| `region_frame_test.py` | 选区录制全链路：红框正好在选区外扩 3px、录像内容 == 同位置截图（SSIM）、四角无红边、能解码、停止键生效 |
| `hotkey_test.py` | 投递 WM_HOTKEY：4 = 选区截图、5 = 选区录制、6 = 停止 |
| `region_cli_test.py` | `rec-r region --json` 独立浮层返回正确矩形 |
| `record_region_test.py` | `rec-r record -r` 与 MCP `region: "select"` 都录出 400x240 |
| `tray_menu_test.py` | 托盘右键（WM_RBUTTONUP / WM_CONTEXTMENU / NIN_SELECT）都能弹出菜单，菜单尺寸正常，`Exit` 可用 |
| `tray_clipboard_restart_test.py` | 托盘 `Copy MCP config`（剪贴板里的 JSON 合法且含绝对路径）与 `Restart`（窗口回来、pid 变了） |
| `tray_no_console_test.py` | 以 `CREATE_NEW_CONSOLE`（等价双击）启动：控制台被隐藏释放、托盘在跑、日志写进 `rec-r.log` |
| `shot_clipboard_annotate_test.py` | 托盘选区截图 → 标注窗口画马赛克 + 文字 → `Copy & Save`：剪贴板里是 CF_DIB 且尺寸等于选区、块内像素一致、文字像素存在、PNG 落盘合法 |
| `esc_cancel_recording_test.py` | 选区浮层里 `Esc` 关掉且不弹标注窗口；托盘开始录制后真按一下 `Esc`，录制停止且文件被删除 |
| `annotate_box_test.py` | 标注窗口有 9 个按钮；`Box` 拖出的方框在剪贴板位图里有橙色边框；输入框里 `Enter` 关闭并落字、`Esc` 关闭且不落字 |
| `tray_open_test.py` | 托盘「打开输出目录」弹出 `CabinetWClass` 窗口且标题是输出目录；「配置文件」弹出 `config.json - Notepad` |
| `odd_region_test.py` | 手拖 401x302 的奇数选区 → 落地视频必须是 400x302 且能解码 |
| `app_shot.py` / `app_record.py` | 直接调运行中托盘 app 的 MCP，截图 / 录一段并打印服务端口径的 target、engine、尺寸 |
| `find_frame.py` / `stack_images.py` | 调试用：把录像帧放回整屏截图里定位（NCC），或把两张图上下拼接对比 |
| `agent_desktop_mcp_check.py` | 握手 agent-desktop 的 MCP 服务，确认 75 个工具可用 |

`cargo test -- --ignored` 还有一个需要真实桌面的用例：`capture_rect_matches_full_desktop_capture`
（区域抓取必须等于整屏抓取的对应切片，这是截图与录屏坐标一致的地基）。

## 录制文件与可播放性

`yuv420p`（以及所有 H.264 编码器）要求边长是偶数，而手拖选区、奇数尺寸窗口很常见。所以：

- 区域选区在解析阶段就取整（`401x301` → `400x300`），`record`/MCP 报的尺寸就是编码尺寸；
- 编码器侧还有一层兜底：真遇到奇数边长先 `crop=trunc(iw/2)*2:trunc(ih/2)*2` 再编码；
- 一个字节都没编出来时（编码器名写错、开录瞬间失败）**不会留下 0 字节的 mp4**：文件会被删掉，
  `record_stop` 和 MCP 都报 `nothing was recorded (the encoder got no frames)`。
  0 字节 mp4 就是之前「打不开 / 没法播放」的根因（缺 `moov` 原子），现在不会再出现。

## 用 agent-desktop 做 GUI 测试

`agent-desktop`（vercel-labs，`%APPDATA%/npm/node_modules/agent-desktop`）以 accessibility tree 驱动
Windows 应用，**默认 headless 语义投递**：`snapshot` / `click` / `type` / `screenshot` 都不移动光标、
不抢前台，只有 `hover` / `drag` / `mouse-*` / `--headed` 才会真的操作鼠标。给它开 MCP：

```bash
pi mcp add agent-desktop -- "$APPDATA/npm/node_modules/agent-desktop/bin/agent-desktop-win32-x64.exe" mcp
pi mcp list      # agent-desktop: connected, 75 tools
```

之后 agent 侧的工具是 `desktop_*`（`desktop_snapshot` / `desktop_click` / `desktop_screenshot` …）。
CLI 等价用法（不经 MCP 也能跑，方便脚本化）：

```bash
agent-desktop status                                  # 权限与适配器状态
agent-desktop list-displays                           # 与 rec-r displays 交叉验证
agent-desktop snapshot --app zen                      # 无障碍树 + @ref
agent-desktop screenshot --app zen shot.png           # 不抢前台地截某个窗口
```

```bash
python scripts/agent_desktop_mcp_check.py             # 握手 + tools/list + desktop_status
```

## 已知限制

- 还没有音频采集（MVP 范围外），MP4 只有视频轨。
- 录制状态是**按进程**维护的：`rec-r stop` 只能停自己进程里的录制；要停托盘 app 里的录制，
  用 MCP 的 `record_stop` 或托盘菜单。
- Windows 10 的 WGC 不支持切换捕获边框，所以窗口捕获用默认边框设置（边框不会出现在画面里）。
  Win11 上也没有额外处理。
- 窗口被最小化时用 DWM 帧边界拿不到有效矩形，直接报错而不是录一张黑图。
- 窗口录制有两条路：WGC（默认，能录被遮挡的内容，跟随窗口移动）和 PrintWindow
  （WGC 不可用时自动使用，受窗口自身绘制逻辑影响，部分 GPU 加速窗口可能只拿到旧帧）。
- 无边框窗口（Qt / Electron 自绘标题栏）用 `-w <hwnd>` 能录，但 `list_windows` 只列
  带边框或可缩放、且不是工具窗的顶层窗口；找不到就用托盘菜单或直接传 hwnd。
- 区域录制跨多个显示器时退回 GDI（WGC 一次只能一个 monitor），GFX 独占/DRM 内容可能录成黑色。
- `rec-r install` 是**按用户**安装：写 `%LOCALAPPDATA%` 和 `HKCU`，不碰 `Program Files`、
  不需要管理员权限；同一台机器多个用户要各装一次。
- 装完的 PATH 只对**之后启动**的进程生效，已经开着的终端/编辑器要重启才能敲 `rec-r`
  （已经在运行的托盘 app 不受影响，它跑的是安装前那份内存镜像）。

## 许可证

MIT，见 [LICENSE](LICENSE)。可以自由使用、修改、商用和再分发，保留版权声明即可。
