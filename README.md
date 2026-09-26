# loopSub

用字幕驱动美剧学习的播放器：Windows 与 macOS 均为单窗口内嵌 libmpv；逐句跳转 / AB 复读 / 跟读暂停 / 字幕搜索与对齐 / LLM 翻译 / Anki 制卡一体化。

> 目标平台：Windows、macOS ｜ 技术栈：Tauri 2 + Rust + 纯静态前端（无 node 依赖）

## 核心理念

**libmpv 是不接收输入的原生显示层，Tauri WebView 是唯一交互层。**

```text
Windows / macOS：
┌─ loopSub 主窗口（唯一窗口、唯一焦点） ─────────────┐
│  顶栏  📂 🕘 🔍 ⚙ 1:1                              │
│  ┌────────────────────────────┐ ┌────────────────┐ │
│  │ 视频区（Windows HWND /      │ │ 字幕列表抽屉    │ │
│  │ macOS Render API + OpenGL） │ │（macOS 常驻）   │ │
│  └────────────────────────────┘ └────────────────┘ │
└────────────────────────────────────────────────────┘
loopSub ──dlopen──> libmpv         （播放控制全部进程内调用）
loopSub ──spawn──> ffmpeg/ffprobe  （字幕探测与导出）
```

由此不需要全局热键。libmpv 以 dlopen 方式进程内加载（无子进程、无 IPC 连接与重试），以 `osc=no input-default-bindings=no hwdec=auto-safe` 初始化；Windows 输出到自建 Win32 子窗口并优先使用 D3D11VA，macOS 通过 libmpv Render API 输出到主窗口内的 OpenGL 3.2 双缓冲 `NSOpenGLView` 并使用 VideoToolbox。

## 功能

**句子级播放控制**
- 逐句跳转（↑↓）、点击句子跳转、±2s 微调、进度条拖动定位（AB 区间高亮）
- AB 区间循环、单句循环、A/B 点 100ms 级微调
- 跟读模式（句末自动暂停）、0.25–3.0x 变速
- 字幕延迟微调（±0.1s / ±0.5s，按视频 hash 记忆）
- 对白增强（保存后立即生效；Windows 使用短时压缩与峰值保护，macOS 使用 Dolby DRC + 三段语音清晰度均衡）

**字幕获取管线**
- 内嵌字幕：`ffprobe` 探测 → `ffmpeg` 导出文本轨（图形轨 PGS/VobSub 自动识别并提示）
- OpenSubtitles 搜索：moviehash 精确匹配 → 文件名解析（S01E03 / release 组）回退列候选
- EIA-608 全大写内嵌字幕的大写还原（规则法 / LLM 法）
- Windows/macOS 默认均在视频画面显示 mpv 字幕，同时保留句子面板；可切换为仅面板显示且立即生效
- 字幕-音频自动对齐（🎯 一键触发）：能量包络互相关估偏移与语速，自研简化 ffsubsync 零外部依赖；结果只写 mpv 属性可逆，按视频记忆自动恢复

**LLM 整集翻译**（OpenAI 兼容 API：DeepSeek / vLLM / 任意兼容端点）
- 两级分块：60s 时间间隙切场景 + 批内最大间隙递归二分（10–30 行/批）
- 摘要链上下文 + 术语表跨批传递（防伪校验）
- 缺行自动带错误明细重试；按视频 hash + 模型名缓存，换源字幕可复用

**学习辅助**
- 一键复制英文台词（数据层序列化，中文行不可选中，支持自定义剪贴板模板）
- Anki 一键制卡（`K`：当前句截图 + 音频切片 + 双语文本 → AnkiConnect；Anki 未启动时兜底导出文件）
- 最近播放历史（MRU 20 条，失效自动剔除）+ 播放位置记忆（≥5s 自动续播）
- 常亮状态徽章（AB / 跟读 / 译文 / 速度 / 延迟），点击即关闭
- macOS 单窗口右侧字幕面板始终完整展开并常驻，不随播放/暂停自动缩放
- 播放状态由 libmpv 属性事件合并推送，不再由 WebView 每 300ms 多属性轮询
- 窗口最小化、隐藏或被完全覆盖时停止提交视频帧；macOS 默认启用可关闭的 Retina 省电渲染

**窗口与系统集成**
- 失焦自动沉底、切回自动召回与置顶（均可在设置页关闭）
- `1:1` 窗口适配视频原始画面（含 DPI 缩放与工作区边界收缩）
- Windows 字幕抽屉按需展开；macOS 右侧 420px 字幕面板默认常驻
- 资源管理器右键菜单「用 loopSub 播放」（Windows，默认开启；HKCU 按 20 个视频扩展名注册，免管理员，设置页可关）

## 安装

从 **GitHub Releases** 下载（`v*` tag 触发 CI 自动构建，见下文「打包」）：

| 平台 | 产物 | 外部程序 |
|---|---|---|
| Windows | `.msi` / `-setup.exe`（NSIS，免管理员） | **已内置 libmpv + ffmpeg，开箱即用** |
| Windows 便携版 | `loopsub-portable.zip`（按下文 PowerShell 命令本地产出） | 同上，全部随包内置 |
| macOS Apple Silicon | `aarch64.dmg` | **已内置原生 libmpv + ffmpeg，开箱即用** |
| macOS Intel | `x64.dmg` | **已内置原生 libmpv + ffmpeg，开箱即用** |

- macOS 未签名公证：首次运行若被拦截，请到「系统设置 → 隐私与安全性」选择“仍要打开”。内置 ffmpeg/ffprobe 与安装包同架构，Apple Silicon 不需要 Rosetta 2。
- 外部程序解析顺序：设置页指定目录 → PATH → 应用资源 → Homebrew。安装包优先使用内置依赖；源码运行时仍会搜索 `/opt/homebrew/lib`、`/usr/local/lib` 及 `opt/mpv/lib`。

## 首次使用

基础功能（内嵌字幕、全部播放控制）**零配置可用**。两项增强按需配置（首次用到时才校验，不会启动时打扰）：

| 功能 | 配置位置 | 说明 |
|---|---|---|
| 在线搜字幕 | 设置 → OpenSubtitles → API Key | 在 [opensubtitles.com](https://www.opensubtitles.com) 注册后于 API 页创建 consumer 获得 |
| LLM 翻译 | 设置 → LLM → Base URL / 模型 / Key | 任意 OpenAI 兼容端点，如 DeepSeek：`https://api.deepseek.com` + `deepseek-chat` |

## 快速上手

1. 启动 loopSub（内嵌的缴械 libmpv 实例随之就绪）
2. 点 📂 打开视频（或拖入窗口 / 从「最近播放」下拉选取）：自动探测内嵌字幕并从上次位置续播，无字幕时点提示条「去搜索」
3. 字幕就绪后：↑↓ 逐句走，`Enter` 单句循环跟读，`[` `]` 框区间反复听
4. 点「翻译」后台滚动预翻，`T` 切换译文显隐；`V` 选中/取消当前句，`Ctrl+C` 复制英文去大模型追问

## 快捷键

全部可在设置页改绑（带冲突检测），默认为 PotPlayer 风格：

| 键 | 功能 | 键 | 功能 |
|---|---|---|---|
| `Space` | 播放/暂停 | `Enter` | 单句循环开关 |
| `←` `→` | ±2s | `R` | 跟读模式开关 |
| `↑` `↓` | 上一句/下一句 | `T` | 译文显隐 |
| `x` / `c` | 减速/加速 0.1 | `V` | 选中/取消当前句 |
| `z` | 速度还原 | `S` | 展开/收起字幕抽屉（仅 Windows） |
| `[` / `]` | 设 A 点/B 点 | `W` | 召回窗口 |
| `Ctrl+[` `Ctrl+]` | A/B 点 −100ms | `Alt+←` `Alt+→` | 字幕延迟 ∓0.1s |
| `Alt+[` `Alt+]` | A/B 点 +100ms | `Alt+Shift+←` `Alt+Shift+→` | 字幕延迟 ∓0.5s |
| `Ctrl+C`（有选区） | 复制选中句英文 | `Alt+0` | 延迟归零 |
| `Shift+[` `Shift+]` | 取消 A / B 点 | `Ctrl+F` | 当前句译文显隐 |
| `K` | 导出当前句为 Anki 卡 | `1` | 窗口适配视频画面 |

所有操作通过 mpv `show-text` 在视频画面弹瞬态反馈（如 `A: 00:12.3`、`1.3x`）。

## 从源码运行与构建

前端是纯静态 HTML/JS，**不需要 Node.js、npm 或 pnpm**。下面所有命令都从仓库根目录（能看到 `src/` 和 `src-tauri/` 的目录）执行。

三个动作不要混淆：

| 目的 | Cargo 命令 | 原生依赖放置位置 |
|---|---|---|
| 开发运行 | `cargo run --manifest-path src-tauri/Cargo.toml` | `src-tauri/target/debug/` |
| 只生成 Release 可执行文件 | `cargo build --release --manifest-path src-tauri/Cargo.toml` | `src-tauri/target/release/` |
| 生成安装包 | `cargo tauri build` | `src-tauri/resources/`，由 Tauri 收进安装包 |

> `cargo clean` 会删除 `src-tauri/target/`，也会删除已放进去的开发/Release 依赖。清理后请重新运行对应的依赖脚本。

### Windows 10/11 x64

#### 1. 安装编译环境

需要以下组件：

- [Rustup](https://rustup.rs/) 与 MSVC Rust 工具链（Rust ≥ 1.77.2）
- Visual Studio 2022 Build Tools 的 **Desktop development with C++** 工作负载及 Windows 10/11 SDK
- Microsoft Edge WebView2 Runtime（Windows 11 通常已内置）
- 7-Zip（依赖脚本用它解压 `mpv-dev`）
- Git

可在管理员或普通 PowerShell 中安装常用组件；Visual Studio 工作负载也可在 Visual Studio Installer 中勾选：

```powershell
winget install --id Git.Git -e
winget install --id Rustlang.Rustup -e
winget install --id 7zip.7zip -e
winget install --id Microsoft.EdgeWebView2Runtime -e
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

安装完成后重新打开 PowerShell，并确认工具链：

```powershell
rustup default stable-x86_64-pc-windows-msvc
rustup target add x86_64-pc-windows-msvc
rustc -V
cargo -V
```

#### 2. 获取源码和开发依赖

```powershell
git clone https://github.com/escquery/loopSub.git
cd loopSub

# 下载 x86_64 libmpv、ffmpeg、ffprobe 到 src-tauri\target\debug\
powershell -ExecutionPolicy Bypass -File .\scripts\setup_windows_deps.ps1 dev
```

脚本从 [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake/releases) 获取最新 `mpv-dev`，从 gyan.dev 获取 ffmpeg/ffprobe，并缓存在 `src-tauri\target\windows-deps\`。需要重新下载最新版时加 `-ForceRefresh`。

#### 3. 开发运行和测试

```powershell
# 启动空播放器
cargo run --manifest-path .\src-tauri\Cargo.toml

# 启动并直接打开视频；-- 后面是传给 loopSub 的参数
cargo run --manifest-path .\src-tauri\Cargo.toml -- "D:\Videos\S01E01.mkv"

# 编译检查和全部 Rust 测试
cargo check --all-targets --manifest-path .\src-tauri\Cargo.toml
cargo test --manifest-path .\src-tauri\Cargo.toml
```

开发运行至少应存在：

```text
src-tauri\target\debug\libmpv-2.dll
src-tauri\target\debug\ffmpeg.exe
src-tauri\target\debug\ffprobe.exe
```

#### 4. 编译 Release 可执行文件

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\setup_windows_deps.ps1 release
cargo build --release --manifest-path .\src-tauri\Cargo.toml

.\src-tauri\target\release\loopsub.exe "D:\Videos\S01E01.mkv"
```

裸 Release 可执行文件依赖同目录的 `libmpv-2.dll`、`ffmpeg.exe` 和 `ffprobe.exe`。制作便携包可直接压缩这四个文件：

```powershell
$portable = ".\dist\loopsub-portable"
New-Item -ItemType Directory -Force $portable | Out-Null
$files = @(
  ".\src-tauri\target\release\loopsub.exe"
  ".\src-tauri\target\release\libmpv-2.dll"
  ".\src-tauri\target\release\ffmpeg.exe"
  ".\src-tauri\target\release\ffprobe.exe"
)
Copy-Item -Path $files -Destination $portable -Force
Compress-Archive -Path "$portable\*" -DestinationPath .\dist\loopsub-portable.zip -Force
```

#### 5. 生成 Windows 安装包

```powershell
cargo install tauri-cli --version "^2" --locked
powershell -ExecutionPolicy Bypass -File .\scripts\setup_windows_deps.ps1 bundle
cargo tauri build --target x86_64-pc-windows-msvc
```

产物位于：

```text
src-tauri\target\x86_64-pc-windows-msvc\release\bundle\msi\*.msi
src-tauri\target\x86_64-pc-windows-msvc\release\bundle\nsis\*-setup.exe
```

如果省略 `--target x86_64-pc-windows-msvc`，产物通常在 `src-tauri\target\release\bundle\` 下。

### macOS（Apple Silicon / Intel）

#### 1. 安装编译环境

支持 macOS 11+；当前开发与实测覆盖 Apple Silicon。只需要 Xcode Command Line Tools 和 Rust，不需要 Homebrew 或 Node.js：

```bash
xcode-select --install

# 尚未安装 Rust 时执行；完成后重新打开终端或 source "$HOME/.cargo/env"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable

uname -m
rustc -V
cargo -V
rustc -vV | grep '^host:'
```

`uname -m` 应为 `arm64` 或 `x86_64`；依赖脚本会下载相同架构的预编译文件。

#### 2. 获取源码和开发依赖

```bash
git clone https://github.com/escquery/loopSub.git
cd loopSub

# 下载原生 libmpv、ffmpeg、ffprobe 到 src-tauri/target/debug/
./scripts/setup_macos_deps.sh dev
```

脚本只下载预编译二进制，不会调用 Homebrew 或本地编译 mpv/FFmpeg。下载内容缓存在 `src-tauri/target/macos-deps/`。

#### 3. 开发运行和测试

```bash
# 启动空播放器
cargo run --manifest-path src-tauri/Cargo.toml

# 启动并直接打开视频
cargo run --manifest-path src-tauri/Cargo.toml -- "/Users/me/Movies/S01E01.mkv"

# 编译检查和全部 Rust 测试
cargo check --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

开发运行至少应存在：

```text
src-tauri/target/debug/libmpv.dylib
src-tauri/target/debug/ffmpeg
src-tauri/target/debug/ffprobe
```

可用下面的命令确认三者架构与当前机器一致：

```bash
file src-tauri/target/debug/libmpv.dylib \
     src-tauri/target/debug/ffmpeg \
     src-tauri/target/debug/ffprobe
```

#### 4. 编译 Release 可执行文件

```bash
./scripts/setup_macos_deps.sh release
cargo build --release --manifest-path src-tauri/Cargo.toml

./src-tauri/target/release/loopsub "/Users/me/Movies/S01E01.mkv"
```

#### 5. 生成 `.app` 和 `.dmg`

```bash
cargo install tauri-cli --version "^2" --locked
./scripts/setup_macos_deps.sh bundle
cargo tauri build
```

Tauri 会自动合并 `src-tauri/tauri.conf.json` 与 `src-tauri/tauri.macos.conf.json`。当前架构的产物位于：

```text
src-tauri/target/release/bundle/macos/loopSub.app
src-tauri/target/release/bundle/dmg/*.dmg
```

本地包默认没有 Developer ID 签名和 Apple 公证，只适合本机测试。正式分发还需配置 Developer ID Application 证书、签名、公证和 staple。

### 依赖脚本模式速查

| 系统 | 开发运行 | Release 可执行文件 | 安装包资源 |
|---|---|---|---|
| Windows PowerShell | `.\scripts\setup_windows_deps.ps1 dev` | `.\scripts\setup_windows_deps.ps1 release` | `.\scripts\setup_windows_deps.ps1 bundle` |
| macOS | `./scripts/setup_macos_deps.sh dev` | `./scripts/setup_macos_deps.sh release` | `./scripts/setup_macos_deps.sh bundle` |

如果程序提示找不到 libmpv/ffmpeg，先确认执行过与当前 Cargo 模式对应的脚本，而不是只把依赖放进另一个目录。也可以在设置页“外部程序”中指定依赖目录。

### 常见构建问题

- Windows 报 `link.exe`、`kernel32.lib` 或 `assert.h` 缺失：在 Visual Studio Installer 修复 **Desktop development with C++** 和 Windows SDK，并从新的 PowerShell 重试。
- Windows 提示脚本执行被禁用：使用上文的 `powershell -ExecutionPolicy Bypass -File ...` 调用方式。
- Windows 下载依赖触发 GitHub API 限流：稍后重试，或保留 `src-tauri\target\windows-deps\` 缓存，不要反复使用 `-ForceRefresh`。
- macOS 报架构不匹配：删除错误目录后重新运行脚本；交叉准备时可显式设置 `LOOPSUB_MAC_ARCH=arm64` 或 `LOOPSUB_MAC_ARCH=x86_64`。
- `error: no such command: tauri`：执行 `cargo install tauri-cli --version "^2" --locked`，并确认 `$HOME/.cargo/bin`（Windows 为 `%USERPROFILE%\.cargo\bin`）在 PATH 中。
- 国内网络无法访问 crates.io 时，可配置可用的 Cargo sparse 镜像后重试；不要把个人镜像配置提交到仓库。

## CI 打包发布

`.github/workflows/release.yml` 使用 Windows MSVC、macOS Intel `macos-15-intel` 和 macOS ARM 三平台矩阵。CI 会按目标架构下载 libmpv、ffmpeg、ffprobe，执行 `cargo tauri build`，并把安装包汇总到草稿 Release。

```bash
# 确认版本号和当前提交后再创建正式 tag
git tag v0.1.0
git push origin v0.1.0
```

也可以在 GitHub Actions 页面手动运行 Release workflow 并填写 tag。Windows 依赖来自 shinchiro/gyan.dev；macOS 依赖来自 media-kit/Martin Riedl。

## 项目结构

```
├── src/                     # 前端（纯静态 HTML/JS/CSS，withGlobalTauri）
│   ├── index.html           # 面板布局（顶栏/列表/搜索/翻译条/设置抽屉）
│   ├── main.js              # 状态机 + 数据驱动快捷键 + mpv 事件同步
│   └── settings.js          # 设置页（11 分区 + 30 键位改绑）
├── src-tauri/src/
│   ├── lib.rs               # Tauri 命令装配与应用状态
│   ├── mpv/                 # libmpv Client/Render API、macOS OpenGL 层、Windows HWND、手动 IPC
│   ├── media/               # ffprobe/ffmpeg 字幕探测与导出
│   ├── subtitle/            # SRT 解析、句子表、大写还原
│   ├── opensub/             # OpenSubtitles（moviehash + 搜索 + 下载）
│   ├── translate/           # LLM 翻译管线（llm/prompt/parser/编排器）
│   ├── settings/            # 设置模型与持久化
│   ├── cache/               # 缓存目录（原文/译文/每视频播放配置）
│   ├── bins.rs              # 外部二进制解析 + CREATE_NO_WINDOW
│   ├── sync.rs              # 字幕-音频自动对齐（能量包络互相关）
│   ├── anki.rs              # Anki 制卡（截图 + 音频切片 + AnkiConnect）
│   └── explorer_menu.rs     # 资源管理器右键菜单（HKCU 按扩展名注册）
├── scripts/                 # Windows/macOS 预编译原生依赖安装脚本
├── .github/workflows/       # CI 自动打包
└── DESIGN.md                # 完整设计文档（焦点模型/翻译策略/里程碑）
```

## 文档与路线

详细设计决策（焦点模型、翻译管线策略、字幕同步问题分析）见 [DESIGN.md](DESIGN.md)。已完成 v1（播放闭环）、v1.5（搜索 + 翻译）与 v2（libmpv 进程内嵌入单窗口、翻译重试与断点续翻、字幕自动对齐、Anki 导出、资源管理器集成）；后续重点是 Windows 真机功耗验收、安装包验证以及 macOS 签名公证。

## License

待定。
