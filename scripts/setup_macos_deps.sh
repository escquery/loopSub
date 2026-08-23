#!/usr/bin/env bash
# 为本地开发/打包下载预编译的原生 macOS 依赖，不使用 Homebrew。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-dev}"
LIBMPV_VERSION="v0.7.2"

case "${LOOPSUB_MAC_ARCH:-$(uname -m)}" in
  arm64|aarch64)
    ARCH="arm64"
    ;;
  x86_64|amd64)
    ARCH="amd64"
    ;;
  *)
    echo "不支持的架构：${LOOPSUB_MAC_ARCH:-$(uname -m)}" >&2
    exit 1
    ;;
esac

case "$MODE" in
  dev)
    DEST="$ROOT/src-tauri/target/debug"
    ;;
  release)
    DEST="$ROOT/src-tauri/target/release"
    ;;
  bundle)
    DEST="$ROOT/src-tauri/resources"
    ;;
  *)
    echo "用法：$0 [dev|release|bundle]" >&2
    echo "  dev      放到 target/debug，供 cargo run 使用（默认）" >&2
    echo "  release  放到 target/release，供 cargo build --release 使用" >&2
    echo "  bundle   放到 resources，供 cargo tauri build 打包" >&2
    exit 2
    ;;
esac

CACHE="$ROOT/src-tauri/target/macos-deps/${ARCH}-${LIBMPV_VERSION}"
PACKAGE="libmpv-libs_${LIBMPV_VERSION}_macos-${ARCH}-video-default"

fetch() {
  local url="$1" output="$2"
  echo "下载：$url"
  curl --fail --location --retry 8 --retry-all-errors --connect-timeout 20 \
    --progress-bar -o "$output" "$url"
}

if [[ ! -f "$CACHE/.complete" ]]; then
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/loopsub-deps.XXXXXX")"
  trap 'rm -rf "$WORK"' EXIT
  mkdir -p "$WORK/out"

  fetch \
    "https://github.com/media-kit/libmpv-darwin-build/releases/download/${LIBMPV_VERSION}/${PACKAGE}.tar.gz" \
    "$WORK/libmpv.tar.gz"
  tar -xzf "$WORK/libmpv.tar.gz" --strip-components=1 -C "$WORK/out"

  # 下载包保留了构建机的 Nix rpath；增加同目录 rpath 后，所有依赖 dylib
  # 都可以从可执行文件旁或 .app/Contents/Resources 中解析。
  for dylib in "$WORK"/out/*.dylib; do
    if ! otool -l "$dylib" | grep 'path @loader_path ' >/dev/null; then
      install_name_tool -add_rpath @loader_path "$dylib"
    fi
    codesign --force --sign - "$dylib" >/dev/null
  done

  for tool in ffmpeg ffprobe; do
    fetch \
      "https://ffmpeg.martin-riedl.de/redirect/latest/macos/${ARCH}/release/${tool}.zip" \
      "$WORK/${tool}.zip"
    unzip -oqj "$WORK/${tool}.zip" "$tool" -d "$WORK/out"
    chmod +x "$WORK/out/$tool"
    codesign --force --sign - "$WORK/out/$tool" >/dev/null
  done

  rm -rf "$CACHE"
  mkdir -p "$(dirname "$CACHE")"
  mv "$WORK/out" "$CACHE"
  touch "$CACHE/.complete"
fi

mkdir -p "$DEST"
rm -f "$DEST"/*.dylib "$DEST/ffmpeg" "$DEST/ffprobe"
cp "$CACHE"/*.dylib "$CACHE/ffmpeg" "$CACHE/ffprobe" "$DEST/"
chmod +x "$DEST/ffmpeg" "$DEST/ffprobe"

echo
echo "依赖已安装到：$DEST"
file "$DEST/libmpv.dylib" "$DEST/ffmpeg" "$DEST/ffprobe"
"$DEST/ffmpeg" -version | head -n 1
