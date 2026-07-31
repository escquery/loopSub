# resources/

随包携带的外部程序，由 CI（.github/workflows/release.yml）在打包前下载填充：

- Windows：`ffmpeg.exe`、`ffprobe.exe`（gyan.dev 静态构建）、`mpv/`（mpv.exe + dll，sourceforge 构建）
- macOS：`ffmpeg`、`ffprobe`（evermeet 静态构建，x86_64；ARM 经 Rosetta 2 运行）

释放位置见 tauri.conf.json `bundle.resources` 与 src/bins.rs 的查找逻辑。
本地开发无需放置任何文件（按 PATH / 设置页目录解析）。
