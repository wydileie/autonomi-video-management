#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target_triple="${TAURI_TARGET_TRIPLE:-$(rustc -vV | awk '/^host:/ { print $2 }')}"
host_triple="$(rustc -vV | awk '/^host:/ { print $2 }')"
target_root="${CARGO_TARGET_DIR:-$repo_root/target}"
if [[ "$target_triple" == "$host_triple" ]]; then
  release_dir="$target_root/release"
else
  release_dir="$target_root/$target_triple/release"
fi
binary_dir="$repo_root/desktop_app/src-tauri/binaries"

mkdir -p "$binary_dir"

if [[ "$target_triple" == "$host_triple" ]]; then
  cargo build --release -p antd -p rust_admin -p rust_stream
else
  cargo build --release --target "$target_triple" -p antd -p rust_admin -p rust_stream
fi

copy_sidecar() {
  local name="$1"
  local src="$2"
  local dest="$binary_dir/${name}-${target_triple}"
  test -x "$src" || {
    echo "Missing executable sidecar: $src" >&2
    exit 1
  }
  cp "$src" "$dest"
  chmod 755 "$dest"
  echo "staged $dest"
}

copy_sidecar "antd" "$release_dir/antd"
copy_sidecar "rust_admin" "$release_dir/rust_admin"
copy_sidecar "rust_stream" "$release_dir/rust_stream"
copy_sidecar "ffmpeg" "${FFMPEG_BIN:-$(command -v ffmpeg)}"
copy_sidecar "ffprobe" "${FFPROBE_BIN:-$(command -v ffprobe)}"
