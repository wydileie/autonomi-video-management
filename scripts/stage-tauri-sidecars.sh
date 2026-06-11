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

resolve_media_tool() {
  local env_name="$1"
  local tool_name="$2"
  local env_value="${!env_name:-}"
  if [[ -n "$env_value" ]]; then
    printf '%s\n' "$env_value"
    return
  fi
  if [[ -n "${FFMPEG_DIST_DIR:-}" ]]; then
    printf '%s\n' "$FFMPEG_DIST_DIR/$tool_name"
    return
  fi
  if [[ "${AUTVID_ALLOW_SYSTEM_FFMPEG:-}" == "1" ]]; then
    command -v "$tool_name"
    return
  fi
  cat >&2 <<EOF
Missing $tool_name bundle input.

Set $env_name to a self-contained $tool_name executable, or set FFMPEG_DIST_DIR
to a directory containing ffmpeg and ffprobe. For local developer builds only,
set AUTVID_ALLOW_SYSTEM_FFMPEG=1 to copy $tool_name from PATH.
EOF
  exit 1
}

check_media_tool_bundle() {
  local name="$1"
  local src="$2"
  if [[ "${AUTVID_ALLOW_DYNAMIC_FFMPEG:-}" == "1" ]]; then
    echo "warning: allowing dynamically linked $name for developer build" >&2
    return
  fi

  case "$target_triple" in
    *linux*)
      if command -v ldd >/dev/null 2>&1 && ldd "$src" 2>&1 | grep -q "not a dynamic executable"; then
        return
      fi
      cat >&2 <<EOF
$name appears to be dynamically linked.

Desktop release bundles must use self-contained FFmpeg/FFprobe binaries so clean
end-user machines do not need system FFmpeg libraries. Provide static media
tools with FFMPEG_BIN/FFPROBE_BIN or FFMPEG_DIST_DIR. For local developer builds
only, set AUTVID_ALLOW_DYNAMIC_FFMPEG=1 to bypass this check.
EOF
      exit 1
      ;;
    *darwin*)
      if ! command -v otool >/dev/null 2>&1; then
        echo "warning: otool not found; cannot verify $name linkage" >&2
        return
      fi
      if otool -L "$src" | awk 'NR > 1 { print $1 }' | grep -Evq '^(/usr/lib/|/System/Library/)'; then
        cat >&2 <<EOF
$name links to non-system macOS libraries.

Desktop release bundles must use self-contained FFmpeg/FFprobe binaries or a
bundle/signing process that includes their dylibs. Homebrew/MacPorts binaries
are not safe to copy directly into a notarized app. For local developer builds
only, set AUTVID_ALLOW_DYNAMIC_FFMPEG=1 to bypass this check.
EOF
        exit 1
      fi
      ;;
  esac
}

copy_media_tool() {
  local name="$1"
  local env_name="$2"
  local src
  src="$(resolve_media_tool "$env_name" "$name")"
  check_media_tool_bundle "$name" "$src"
  copy_sidecar "$name" "$src"
}

copy_sidecar "antd" "$release_dir/antd"
copy_sidecar "rust_admin" "$release_dir/rust_admin"
copy_sidecar "rust_stream" "$release_dir/rust_stream"
copy_media_tool "ffmpeg" "FFMPEG_BIN"
copy_media_tool "ffprobe" "FFPROBE_BIN"
