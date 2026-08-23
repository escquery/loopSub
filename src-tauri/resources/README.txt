# resources/

随包携带的外部组件，由 CI（.github/workflows/release.yml）在打包前按目标架构下载：

- Windows：`libmpv-2.dll`（shinchiro mpv-dev）及 `ffmpeg.exe`、`ffprobe.exe`（gyan.dev）
- macOS：`libmpv.dylib` 及依赖库（media-kit LGPL video-default）、`ffmpeg`、`ffprobe`（Martin Riedl 静态构建）

释放位置见 tauri.conf.json `bundle.resources`、src/bins.rs 与 src/mpv/embed.rs。
源码开发默认按设置目录 / PATH / Homebrew 查找，也可自行放到 target/debug。
