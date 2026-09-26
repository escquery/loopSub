# resources/

随包携带的外部组件，由 CI（.github/workflows/release.yml）或本地依赖脚本在打包前按目标架构下载：

- Windows：`libmpv-2.dll`（shinchiro mpv-dev）及 `ffmpeg.exe`、`ffprobe.exe`（gyan.dev）
- macOS：`libmpv.dylib` 及依赖库（media-kit LGPL video-default）、`ffmpeg`、`ffprobe`（Martin Riedl 静态构建）

本地打包：Windows 运行 `scripts/setup_windows_deps.ps1 bundle`，macOS 运行
`scripts/setup_macos_deps.sh bundle`。释放位置见 tauri.conf.json
`bundle.resources`、src/bins.rs 与 src/mpv/embed.rs。源码开发也可使用两个脚本的
`dev` 模式把依赖放到 target/debug。
