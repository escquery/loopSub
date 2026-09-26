# loopSub 设计文档

> 用字幕驱动美剧学习：Tauri 面板遥控 mpv，字幕获取 / 同步修复 / LLM 翻译 / 句子级播放控制一体化。
> 目标平台：Windows、macOS。开发环境：WSL（日常开发）+ CI（出包）。

---

## 1. 产品定位

辅助 mpv 播放美剧的学习工具：以字幕为核心控制视频播放（点句跳转、单句/区间循环、跟读暂停），
内置字幕获取（内嵌导出 + OpenSubtitles 搜索）、字幕同步修复、LLM 整集预翻译与缓存、
一键复制英文台词（配合大模型 chat 追问用法）。

## 2. 总体架构：焦点模型

**核心决策：Windows 与 macOS 都只有一个顶层窗口；libmpv 是不接收输入的原生显示层，WebView 是唯一交互层。**

```text
┌─ loopSub 主窗口 ─────────────────────────────┐
│ 顶栏 / 播放控制                              │
│ ┌──────────────────────┐ ┌────────────────┐ │
│ │ 原生视频层            │ │ 字幕学习面板    │ │
│ │ Windows: HWND + wid   │ │ macOS: 常驻     │ │
│ │ macOS: Render API+GL  │ │ Windows: 抽屉   │ │
│ └──────────────────────┘ └────────────────┘ │
└──────────────────────────────────────────────┘
loopSub ──dlopen──> libmpv
loopSub ──spawn──> ffmpeg / ffprobe
```

- 默认播放控制直接调用进程内 libmpv Client API；JSON IPC 只保留手动连接模式。
- Windows 由 mpv 渲染到自建 HWND 子窗口。
- macOS 创建透明 WKWebView + OpenGL 3.2 Core 双缓冲 `NSOpenGLView`；libmpv 更新回调只唤醒专用渲染线程，VideoToolbox 负责硬解。
- Windows/macOS 都以 `hwdec=auto-safe` 初始化，分别优先使用 D3D11VA / VideoToolbox。
- 视频层避开 44px 顶栏、14px 进度条与右侧字幕面板，不参与鼠标和键盘输入。
- 最小化、隐藏或被完全覆盖时停止提交视频帧；macOS 默认用逻辑分辨率 surface，由 WindowServer 缩放到 Retina，设置页可切回高分辨率。
- 主窗口关闭时整个进程退出，因此不会遗留独立 mpv 窗口或焦点问题。

### 三种状态三种界面

| 状态 | 界面 |
|---|---|
| 播放中 | macOS 完整面板常驻，当前句自动高亮；Windows 使用右侧字幕抽屉 |
| 暂停 | 面板尺寸不变，继续显示句子列表与操作按钮 |
| 浏览 | 主面板：全量句子列表 / 搜索 / 翻译管理 / 设置 |

## 3. mpv 控制（Client API；手动模式兼容 JSON IPC）

- 默认：同进程 `mpv_command` / `mpv_get_property` / `mpv_set_property`。
- 播放状态用 `mpv_observe_property` 订阅并在事件线程合并为最多每 200ms 一次推送，避免 WebView 高频跨桥轮询。
- 手动连接外部 mpv 时：mac/Linux Unix socket、Windows 命名管道；以前端每 500ms 一次的聚合读取兜底。
- libmpv handle、RenderContext 和原生视频层生命周期均随主窗口。

### 功能 → 命令映射

| 功能 | 实现 |
|---|---|
| 点句跳转 | `seek <start> absolute` |
| 上一句/下一句 | `sub-seek -1` / `sub-seek 1` |
| 单句循环 | 读 `sub-start`/`sub-end` → 设 `ab-loop-a/b`；取消设 `"no"` |
| 区间循环 | 句子列表"设 A 于此 / 设 B 于此" |
| 跟读模式（句末暂停） | 订阅 `time-pos`，≥ 当前句 end 时 `pause` |
| 加减速 | `speed` 属性（0.1 步进，0.25~3.0） |
| 译文显隐 | `secondary-sid` 切换（mpv 渲染模式下） |
| 字幕延迟微调 | `sub-delay` / `secondary-sub-delay`（双轨默认联动） |
| 瞬态反馈 | `show-text "..." 1000` 显示在视频画面上 |
| 对白增强 | 保存后实时增删 `af`；Windows：短时压缩 + limiter；macOS：AC-3/E-AC-3 标准 DRC + 三段语音 equalizer，切换 DRC 时重载当前 Dolby 音轨 |
| 状态同步 | 订阅 `pause` / `time-pos` / `duration` / `eof-reached` / `sub-start` / `sub-end` |

**当前句判定不依赖 mpv**：订阅 `time-pos` 在程序自己的句子表里反查
（最后一条 `start <= t` 的句子），不受 mpv 字幕显示开关影响。

## 4. 字幕显示

- Windows 与 macOS 默认都由 **mpv 在视频画面渲染字幕**，行为与原生 mpv 一致；右侧句子面板同时保留
- 设置可切换为“仅右侧面板显示”，此时统一设置 `sub-visibility=no`
- 字幕轨始终加载（`sid` 保留），保证 `sub-text`/`sub-start`/`sub-end`、逐句跳转和延迟调整可用
- 渲染设置保存后实时更新当前播放器，不需要重新打开视频

## 5. 字幕获取管线

```
加载视频
 ├─ ffprobe 探测字幕轨
 │   ├─ 文本轨(subrip/ass/mov_text) → ffmpeg 导出到缓存目录 → 使用
 │   ├─ 仅图形轨(PGS/VobSub)        → 标记"内嵌不可用" → 搜索
 │   └─ 无字幕轨                    → 搜索
 ├─ 搜索（此刻才校验 OpenSubtitles API Key）
 │   ├─ moviehash 精确匹配（文件大小 + 首尾 64KB 校验和）
 │   └─ 未命中 → 按文件名解析(剧名/SxxExx/release组)列候选 → 用户手选
 ├─ 字幕就绪 → 大写还原 → 构建句子表 → 面板可用
 └─ 后台：翻译管线滚动预翻（领先播放进度 ~10 分钟）
```

全程异步，面板显示阶段状态（提取中/搜索中/翻译 37%），视频可先播。

### 字幕同步问题背景（为什么搜索的字幕会越播越偏）

| 模式 | 表现 | 成因 |
|---|---|---|
| 恒定偏移 | 从头固定差几秒 | 片头/recap 差异 |
| 线性漂移 | 越播越偏 | 帧率错配（25fps PAL vs 23.976 NTSC，42 分钟可累计 >100s） |
| 分段跳变 | 中途突变 | 广告断点/删减差异 |

对策：moviehash 源头选对 → ffsubsync 音频对齐兜底 → 播放期 `sub-delay`（恒定）/`sub-speed`（漂移）微调，延迟值按视频 hash 持久化。

## 6. 字幕处理

- **SRT/ASS 解析**：自建句子表（序号、start、end、文本）
- **大写还原**（内嵌字幕常为 EIA-608 全大写）：
  - 规则法（默认）：整体小写 → 句首大写 → `i/i'm/i'll/i've/i'd` 还原 → 常见专名表
  - LLM 法（设置页开关）：复用翻译批处理管线与缓存
  - 只作用于展示层与复制，不改原文件
- **复制英文**：
  - 数据层序列化（绝不从 DOM 复制），中文行加 `user-select: none` 双保险
  - 多选：Shift+点击选区间 / hover checkbox / 快捷键选当前句
  - 可配置剪贴板模板（如"请逐句讲解以下台词的生词短语：\n{台词}"），可选附带上下文 N 句

## 7. LLM 翻译管线（继承 gpt-subtrans 实战结论）

### 分块（两级）

- 时间间隙 > **60s** 切场景（scene）
- 场景内按行数分批：**min 10 / max 30 行**；超大批**在最大时间间隙处递归二分**（两侧 ≥ min）

### 上下文

- 不给前文原文，给**摘要链**：每批响应要求模型输出 `<summary>`（本批）与 `<scene>`（场景累计），后续批次 prompt 携带最近 ~10 条历史摘要 + 剧名/简介 + 人名表
- **术语表**：模型每批输出 `<terminology>`（`原文::译文`），接收方用本批原文/译文做防伪校验（源词须在原文、译词须在译文、方向反了自动交换、冲突保留旧值），注入下一批

### 行格式与解析

- `#编号\nOriginal>\n原文\nTranslation>\n译文`；指令含 few-shot（允许按目标语言语法调整内容，禁止增减行数）
- 解析：1 个严格正则 + 逐级放宽的 fallback；编号未命中按 Original 文本模糊匹配；检测原文/译文写反自动交换
- 校验：缺编号、空译文、超长(>120 字符)、换行过多(>2)

### 错误恢复（三级降级）

1. 拆半重翻（最大间隙处劈开分别翻再合并）
2. 撞 token 上限 → 去掉 context 重试
3. 带错误明细 retry（"这些问题：…请修正"，温度 +0.1），强调禁止合并行

### 效率与缓存

- 块间并发（默认 4），重试退避 ~3s，max_retries=1
- **JIT 预翻译**：优先翻译播放位置之后的块，领先 ~10 分钟，头几块翻完即可开播
- 按**行内容 hash** 缓存（换源字幕大部分行可复用）；批粒度断点续翻

### 惰性校验

OpenSubtitles Key 在首次搜索时校验；LLM 配置在首次翻译时校验。缺失 → 面板内联提示 + 跳转设置页，不在启动时打扰。

## 8. 快捷键（PotPlayer 风格，全部可在设置页改绑 + 冲突检测）

| 键 | 功能 |
|---|---|
| `Space` | 播放/暂停 |
| `←` `→` | ±2s（`seek relative+exact`） |
| `↑` `↓` | 上一句 / 下一句 |
| `x` / `c` | 减速 / 加速 0.1 步进 |
| `z` | 速度还原 1.0 |
| `[` / `]` | 设 A 点 / B 点 |
| `Ctrl+[` `Ctrl+]` | A/B 点 -100ms |
| `Alt+[` `Alt+]` | A/B 点 +100ms |
| `Enter` | 单句循环开关 |
| `R` | 跟读模式开关 |
| `T` | 译文显隐 |
| `V` | 选中/取消当前句 |
| `Ctrl+C`（有选区） | 复制选中句英文（带模板） |
| `Alt+←` `Alt+→` | 字幕延迟 ∓0.1s（双轨联动） |
| `Alt+Shift+←` `Alt+Shift+→` | 字幕延迟 ∓0.5s |
| `Alt+0` | 延迟归零 |
| `S` | 展开/收起字幕抽屉（仅 Windows） |
| `W` | 召回视频窗口 |

所有快捷键操作通过 mpv `show-text` 在视频画面弹瞬态反馈（如 `A: 00:12.3`、`1.3x`）。

## 9. 状态面板

- 常亮徽章条：`AB循环 12.3→15.8s`、`跟读`、`译文`、`1.3x`、`字幕+0.2s`，**点击徽章即关闭对应功能**
- macOS 面板始终完整展开，徽章条常驻

## 10. 音频

| 项 | 默认 | 说明 |
|---|---|---|
| 对白增强 | 开 | Windows 固定参数压缩；macOS 使用 Dolby 解码器 DRC + 轻量包内置 equalizer；保存后实时生效，均不跨 AB 学习历史响度 |
| 音量上限 | 关闭 | 可选 150%/200%（`volume-max`） |
| 中置声道提升 | 关（进阶） | 5.1 片源 `pan` 滤镜加权 FC 进立体声 |

## 11. 设置页全景

| 分区 | 设置项 | 默认 |
|---|---|---|
| 快捷键 | 全键位可改 + 冲突检测 + 恢复默认 | 上表 |
| 音频 | 对白增强 / 音量上限 | 开 / 关闭 |
| 字幕 | 大写还原：规则法/LLM；渲染：面板/mpv；延迟步长 | 规则法 / mpv / 0.1s |
| 复制 | 模板、附带上下文句数 | 内置模板 / 0 |
| OpenSubtitles | API Key | 空（用时校验） |
| LLM | Base URL / 模型 / API Key / 并发 / 批次大小 / 超时 | 空（用时校验） |
| 缓存 | 缓存目录、清理策略 | 系统应用数据目录 |
| 窗口 | 停靠侧、失焦沉底、切回召回 mpv、macOS Retina 省电渲染 | 右 / 开 / 开 / 开 |

## 12. 缓存目录结构

```
<cache_dir>/
├── originals/           # 提取/下载的字幕原文（<video_hash>.srt）
├── truecased/           # 大写还原结果
├── translated/          # 译文（<video_hash>.<model>.srt）+ 批粒度进度
└── videos/              # 按视频 hash 的播放配置（sub-delay、速度、A/B 点）
```

`video_hash` 采用 OpenSubtitles moviehash 算法（文件大小 + 首尾 64KB 的 u64 校验和），搜索与缓存共用同一键。

## 13. 平台差异与分发

| | Windows | macOS | WSL（开发） |
|---|---|---|---|
| 播放控制 | 进程内 libmpv（Win32 子窗口） | 进程内 libmpv（Render API + NSOpenGLView） | 手动 IPC / 系统 mpv |
| mpv/ffmpeg 分发 | 内置 DLL + 绿色 exe | 内置同架构 dylib + 静态可执行文件 | 系统安装 |
| 出包 | GitHub Actions `windows-latest` | `macos-latest` ARM + `macos-15-intel` | 不出包 |
| WebView | WebView2 (Chromium) | WKWebView | WebKitGTK（≈mac 预览） |

- 子进程隐藏控制台：Windows `CREATE_NO_WINDOW`
- 前端：纯静态 HTML/JS + `withGlobalTauri`，无 node 依赖；后期可迁移框架

## 14. 里程碑

- **v1（闭环）✅ 已完成**：面板遥控 mpv（播放/seek/速度/AB/单句循环/跟读）+ 内嵌字幕导出 + 句子列表（点击跳转/多选复制英文）+ 快捷键 + 设置页（含音频、热键改绑）+ 窗口行为（失焦沉底/切回召回 mpv）
- **v1.5 ✅ 已完成**：OpenSubtitles 搜索（moviehash 精确 + 文件名解析回退）+ LLM 翻译管线（两级分块/摘要链/术语表防伪/错误明细重试）+ 缓存 + 译文显隐
- **v2 ✅ 已完成**：进程内 libmpv；Windows HWND 单窗口内嵌；macOS Render API + OpenGL 单窗口内嵌及常驻字幕面板；翻译并发与断点续翻；字幕自动对齐；Anki 导出；Windows/macOS 安装包内置同架构 libmpv、ffmpeg 与 ffprobe。
