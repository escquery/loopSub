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

由此不需要全局热键。libmpv 以 dlopen 方式进程内加载（无子进程、无 IPC 连接与重试），以 `osc=no input-default-bindings=no` 初始化；Windows 输出到自建 Win32 子窗口，macOS 通过 libmpv Render API 输出到主窗口内的 Retina `NSOpenGLView`，并启用 VideoToolbox 硬件解码。

## 功能

**句子级播放控制**
- 逐句跳转（↑↓）、点击句子跳转、±2s 微调、进度条拖动定位（AB 区间高亮）
- AB 区间循环、单句循环、A/B 点 100ms 级微调
- 跟读模式（句末自动暂停）、0.25–3.0x 变速
- 字幕延迟微调（±0.1s / ±0.5s，按视频 hash 记忆）
- 对白增强（Windows 使用短时压缩，macOS 使用轻量 libmpv 支持的语音频段均衡）

**字幕获取管线**
- 内嵌字幕：`ffprobe` 探测 → `ffmpeg` 导出文本轨（图形轨 PGS/VobSub 自动识别并提示）
- OpenSubtitles 搜索：moviehash 精确匹配 → 文件名解析（S01E03 / release 组）回退列候选
- EIA-608 全大写内嵌字幕的大写还原（规则法 / LLM 法）
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
| Windows 便携版 | `loopsub-portable.zip`（解压即用，`tools\pack_portable.ps1` 本地产出） | 同上，全部随包内置 |
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

## 从源码运行

依赖：Rust ≥ 1.77、libmpv 与 ffmpeg、Tauri 系统依赖（Linux: `webkit2gtk-4.1` 等，见 [Tauri 文档](https://v2.tauri.app/start/prerequisites/)）。

- **Windows**：从 [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake/releases) 下载 `mpv-dev` 包，将其中的 `libmpv-2.dll` 与 ffmpeg/ffprobe（gyan.dev 静态构建）放到 `src-tauri/target/debug/`（或运行后在设置页指定目录）
- **Debian/Ubuntu**：`sudo apt install libmpv2 ffmpeg`
- **macOS**：无需 Homebrew。脚本按当前架构下载预编译的 libmpv、ffmpeg、ffprobe（兼容 macOS 11+）到 `target/debug`

```bash
./scripts/setup_macos_deps.sh                    # macOS：只下载二进制，不本地编译依赖
cargo run --manifest-path src-tauri/Cargo.toml   # 开发运行（无 node 步骤）
cd src-tauri && cargo test --lib                 # 单元测试
```

macOS 本地 Release 可执行文件先运行 `./scripts/setup_macos_deps.sh release`；打包可分发的 `.app/.dmg` 则先运行 `./scripts/setup_macos_deps.sh bundle`。

## 打包

CI 自动出包（`.github/workflows/release.yml`，三平台矩阵：Windows / macOS Intel `macos-15-intel` / macOS ARM）：

```bash
git tag v0.1.0 && git push origin v0.1.0   # 或在 Actions 页手动触发
```

CI 构建前按目标架构下载 libmpv（Windows：shinchiro mpv-dev；macOS：media-kit LGPL video-default）与 ffmpeg（Windows：gyan.dev；macOS：Martin Riedl 静态构建）注入安装包，产物汇总为草稿 Release。本地打包需 `cargo install tauri-cli` 后 `cargo tauri build`。

## 项目结构

```
├── src/                     # 前端（纯静态 HTML/JS/CSS，withGlobalTauri）
│   ├── index.html           # 面板布局（顶栏/列表/搜索/翻译条/设置抽屉）
│   ├── main.js              # 状态机 + 数据驱动快捷键 + mpv 事件同步
│   └── settings.js          # 设置页（11 分区 + 30 键位改绑）
├── src-tauri/src/
│   ├── lib.rs               # Tauri 命令装配与应用状态
│   ├── mpv/                 # libmpv dlopen C API（embed）+ 自建视频窗口（vidwin）+ IPC（手动连接外部 mpv）
│   ├── media/               # ffprobe/ffmpeg 字幕探测与导出
│   ├── subtitle/            # SRT 解析、句子表、大写还原
│   ├── opensub/             # OpenSubtitles（moviehash + 搜索 + 下载）
│   ├── translate/           # LLM 翻译管线（llm/prompt/parser/编排器）
│   ├── settings/            # 设置模型与持久化
│   ├── cache/               # 缓存目录（原文/译文/每视频播放配置）
│   ├── bins.rs              # 外部二进制解析 + CREATE_NO_WINDOW
│   ├── sync.rs              # 字幕-音频自动对齐（能量包络互相关）
│   ├── anki.rs              # Anki 制卡（截图 + 音频切片 + AnkiConnect）
│   ├── explorer_menu.rs     # 资源管理器右键菜单（HKCU 按扩展名注册）
│   └── winctl.rs            # Windows mpv 窗口召回（SetWindowPos）
├── .github/workflows/       # CI 自动打包
├── tools/                   # 本地工具（便携版打包 / 副屏调试，gitignored）
└── DESIGN.md                # 完整设计文档（焦点模型/翻译策略/里程碑）
```

## 文档与路线

详细设计决策（焦点模型、翻译管线策略、字幕同步问题分析）见 [DESIGN.md](DESIGN.md)。已完成 v1（播放闭环）、v1.5（搜索 + 翻译）与 v2（libmpv 进程内嵌入单窗口、字幕自动对齐、Anki 导出、资源管理器集成）；后续规划：翻译并发与断点续翻。

## License

待定。
