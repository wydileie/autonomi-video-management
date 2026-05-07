#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/smoke-local-devnet.sh [--restart-admin] [--large-original] [--no-original]

Runs an end-to-end smoke test against a running Compose stack:
  1. wait for /api/health and /stream/health
  2. log in as the configured admin
  3. generate a tiny video unless SMOKE_VIDEO_PATH is set
  4. request an upload quote
  5. upload the video, optionally including the original source
  6. approve the final quote
  7. publish the video if needed
  8. fetch public HLS playlists and first segments for each requested rendition

Useful environment variables:
  SMOKE_BASE_URL                 Default: http://localhost
  SMOKE_ADMIN_USERNAME           Default: ADMIN_USERNAME or admin
  SMOKE_ADMIN_PASSWORD           Default: ADMIN_PASSWORD or admin
  SMOKE_RESOLUTIONS              Default: 360p,240p
  SMOKE_VIDEO_PATH               Existing video file to upload
  SMOKE_MAX_WAIT_SECONDS         Default: 900
  SMOKE_POLL_SECONDS             Default: 5
  SMOKE_UPLOAD_ORIGINAL          Default: true
  SMOKE_REQUIRE_ORIGINAL_STORED  Default: same as SMOKE_UPLOAD_ORIGINAL
  SMOKE_PUBLISH_WHEN_READY       Default: true
  SMOKE_RESTART_ADMIN            Default: false
  SMOKE_COMPOSE_ENV              Default: .env.local
  SMOKE_COMPOSE_MODE             local or local-public. Default: local
  SMOKE_LARGE_ORIGINAL           Default: false
  SMOKE_LARGE_ORIGINAL_BYTES     Default: 18874368
USAGE
}

log() {
  printf '[smoke] %s\n' "$*"
}

fail() {
  printf '[smoke] ERROR: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

bool() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) return 0 ;;
    *) return 1 ;;
  esac
}

trim_trailing_slash() {
  printf '%s' "${1%/}"
}

json_get() {
  local key_path="$1"
  local payload_file
  payload_file="$(mktemp "${TMP_DIR:-/tmp}/smoke-json.XXXXXX")"
  cat > "$payload_file"
  python3 - "$key_path" "$payload_file" <<'PY'
import json
import sys

path = sys.argv[1]
with open(sys.argv[2], "r", encoding="utf-8") as handle:
    data = json.load(handle)
current = data
for part in [p for p in path.split(".") if p]:
    if isinstance(current, list):
        current = current[int(part)]
    elif isinstance(current, dict):
        current = current.get(part)
    else:
        current = None
    if current is None:
        sys.exit(1)

if isinstance(current, bool):
    print("true" if current else "false")
elif isinstance(current, (dict, list)):
    print(json.dumps(current, separators=(",", ":")))
else:
    print(current)
PY
  local status=$?
  rm -f "$payload_file"
  return "$status"
}

json_public_video_status() {
  local video_id="$1"
  local payload_file
  payload_file="$(mktemp "${TMP_DIR:-/tmp}/smoke-json.XXXXXX")"
  cat > "$payload_file"
  python3 - "$video_id" "$payload_file" <<'PY'
import json
import sys

video_id = sys.argv[1]
with open(sys.argv[2], "r", encoding="utf-8") as handle:
    videos = json.load(handle)
for video in videos:
    if video.get("id") == video_id:
        print(video.get("status", ""))
        sys.exit(0)
sys.exit(1)
PY
  local status=$?
  rm -f "$payload_file"
  return "$status"
}

request() {
  local method="$1"
  local url="$2"
  local body="${3-}"
  local out="${TMP_DIR}/response.json"
  local code
  local -a args=(-sS -o "$out" -w '%{http_code}' -X "$method" -H 'Accept: application/json' -b "$COOKIE_JAR" -c "$COOKIE_JAR")

  if [[ "$method" =~ ^(POST|PATCH|DELETE)$ ]] && [[ "$url" != */auth/login && "$url" != */auth/refresh ]]; then
    local csrf
    csrf="$(csrf_token || true)"
    [[ -n "$csrf" ]] || fail "Missing CSRF cookie before ${method} ${url}"
    args+=(-H "X-CSRF-Token: ${csrf}")
  fi
  if [[ $# -ge 3 ]]; then
    args+=(-H 'Content-Type: application/json' --data "$body")
  fi

  code="$(curl "${args[@]}" "$url")" || {
    cat "$out" >&2 2>/dev/null || true
    return 1
  }
  if [[ "$code" != 2* ]]; then
    printf 'HTTP %s from %s %s\n' "$code" "$method" "$url" >&2
    cat "$out" >&2
    printf '\n' >&2
    return 1
  fi
  cat "$out"
}

csrf_token() {
  awk '$6 == "autvid_csrf" { value = $7 } END { if (value != "") print value }' "$COOKIE_JAR"
}

wait_url() {
  local url="$1"
  local label="$2"
  local deadline=$((SECONDS + MAX_WAIT_SECONDS))
  until curl -fsS "$url" >/dev/null; do
    if (( SECONDS >= deadline )); then
      fail "Timed out waiting for ${label} at ${url}"
    fi
    sleep "$POLL_SECONDS"
  done
}

compose() {
  local -a args=(--env-file "$COMPOSE_ENV" -f docker-compose.yml -f docker-compose.local.yml)
  if [[ "$COMPOSE_MODE" == "local-public" ]]; then
    args+=(-f docker-compose.local-public.yml)
  fi
  docker compose "${args[@]}" "$@"
}

generate_video() {
  local output="$1"
  log "Generating tiny smoke video at ${output}"
  if ! ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i testsrc=size=640x360:rate=24 \
    -f lavfi -i sine=frequency=880:sample_rate=44100 \
    -t 4 -shortest \
    -c:v libx264 -preset ultrafast -pix_fmt yuv420p \
    -c:a aac -movflags +faststart \
    "$output"; then
    log "libx264 generation failed; falling back to MPEG-4 video"
    ffmpeg -hide_banner -loglevel error -y \
      -f lavfi -i testsrc=size=640x360:rate=24 \
      -f lavfi -i sine=frequency=880:sample_rate=44100 \
      -t 4 -shortest \
      -c:v mpeg4 -q:v 4 -pix_fmt yuv420p \
      -c:a aac -movflags +faststart \
      "$output"
  fi
}

wait_for_ready() {
  local video_id="$1"
  local restarted="false"
  local approved="false"
  local deadline=$((SECONDS + MAX_WAIT_SECONDS))

  while (( SECONDS < deadline )); do
    local video status error_message is_public original_address
    video="$(request GET "${API_URL}/admin/videos/${video_id}")" || fail "Could not fetch admin video ${video_id}"
    status="$(printf '%s' "$video" | json_get status || true)"
    error_message="$(printf '%s' "$video" | json_get error_message || true)"
    log "Video ${video_id} status: ${status:-unknown}"

    case "$status" in
      pending|processing)
        if bool "$RESTART_ADMIN" && [[ "$restarted" == "false" ]]; then
          need docker
          log "Restarting rust_admin to exercise durable job recovery"
          compose restart rust_admin >/dev/null
          wait_url "${API_URL}/health" "admin health after restart"
          restarted="true"
        fi
        ;;
      awaiting_approval)
        if [[ "$approved" == "false" ]]; then
          log "Approving final quote"
          request POST "${API_URL}/admin/videos/${video_id}/approve" >/dev/null
          approved="true"
        fi
        ;;
      uploading)
        ;;
      ready)
        is_public="$(printf '%s' "$video" | json_get is_public || echo false)"
        if bool "$REQUIRE_ORIGINAL_STORED"; then
          original_address="$(printf '%s' "$video" | json_get original_file_address || true)"
          [[ -n "$original_address" ]] || fail "Original source upload was required but original_file_address is empty"
          log "Original source stored at ${original_address}"
        fi
        if ! bool "$is_public"; then
          log "Publishing ready video"
          request PATCH "${API_URL}/admin/videos/${video_id}/publication" '{"is_public":true}' >/dev/null
        fi
        return 0
        ;;
      error|expired)
        fail "Video entered terminal status ${status}: ${error_message:-no error detail}"
        ;;
    esac

    sleep "$POLL_SECONDS"
  done

  fail "Timed out waiting for video ${video_id} to become ready"
}

wait_for_public_catalog() {
  local video_id="$1"
  local deadline=$((SECONDS + MAX_WAIT_SECONDS))
  while (( SECONDS < deadline )); do
    local videos
    videos="$(request GET "${API_URL}/videos")" || fail "Could not fetch public videos"
    if printf '%s' "$videos" | json_public_video_status "$video_id" >/dev/null; then
      log "Video ${video_id} is visible in the public catalog"
      return 0
    fi
    sleep "$POLL_SECONDS"
  done
  fail "Timed out waiting for video ${video_id} to appear in the public catalog"
}

fetch_playlist_and_segment() {
  local playlist_url="$1"
  local label="$2"
  local playlist="${TMP_DIR}/${label}.m3u8"
  local segment="${TMP_DIR}/${label}.ts"
  local deadline=$((SECONDS + MAX_WAIT_SECONDS))

  until curl -fsS "$playlist_url" -o "$playlist"; do
    if (( SECONDS >= deadline )); then
      fail "Timed out fetching playlist ${playlist_url}"
    fi
    sleep "$POLL_SECONDS"
  done

  local segment_path segment_url
  segment_path="$(awk 'NF && $0 !~ /^#/ { print; exit }' "$playlist")"
  [[ -n "$segment_path" ]] || fail "Playlist ${playlist_url} did not contain a segment URL"
  case "$segment_path" in
    http://*|https://*) segment_url="$segment_path" ;;
    /*) segment_url="${BASE_URL}${segment_path}" ;;
    *) segment_url="${playlist_url%/*}/${segment_path}" ;;
  esac

  curl -fsS "$segment_url" -o "$segment"
  local segment_bytes
  segment_bytes="$(wc -c < "$segment" | tr -d ' ')"
  [[ "$segment_bytes" -gt 0 ]] || fail "Fetched empty segment from ${segment_url}"
  log "Fetched ${label} playlist and first segment (${segment_bytes} bytes)"
}

RESTART_ADMIN="${SMOKE_RESTART_ADMIN:-false}"
LARGE_ORIGINAL="${SMOKE_LARGE_ORIGINAL:-false}"
UPLOAD_ORIGINAL="${SMOKE_UPLOAD_ORIGINAL:-true}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    --restart-admin) RESTART_ADMIN="true" ;;
    --large-original) LARGE_ORIGINAL="true"; UPLOAD_ORIGINAL="true" ;;
    --no-original) UPLOAD_ORIGINAL="false" ;;
    *) fail "Unknown argument: $1" ;;
  esac
  shift
done

need curl
need python3
need ffmpeg

BASE_URL="$(trim_trailing_slash "${SMOKE_BASE_URL:-http://localhost}")"
API_URL="$(trim_trailing_slash "${SMOKE_API_URL:-${BASE_URL}/api}")"
STREAM_URL="$(trim_trailing_slash "${SMOKE_STREAM_URL:-${BASE_URL}/stream}")"
ADMIN_USERNAME="${SMOKE_ADMIN_USERNAME:-${ADMIN_USERNAME:-admin}}"
ADMIN_PASSWORD="${SMOKE_ADMIN_PASSWORD:-${ADMIN_PASSWORD:-admin}}"
RESOLUTIONS="${SMOKE_RESOLUTIONS:-360p,240p}"
MAX_WAIT_SECONDS="${SMOKE_MAX_WAIT_SECONDS:-900}"
POLL_SECONDS="${SMOKE_POLL_SECONDS:-5}"
PUBLISH_WHEN_READY="${SMOKE_PUBLISH_WHEN_READY:-true}"
SHOW_MANIFEST_ADDRESS="${SMOKE_SHOW_MANIFEST_ADDRESS:-true}"
REQUIRE_ORIGINAL_STORED="${SMOKE_REQUIRE_ORIGINAL_STORED:-$UPLOAD_ORIGINAL}"
COMPOSE_ENV="${SMOKE_COMPOSE_ENV:-.env.local}"
COMPOSE_MODE="${SMOKE_COMPOSE_MODE:-local}"
LARGE_ORIGINAL_BYTES="${SMOKE_LARGE_ORIGINAL_BYTES:-18874368}"

case "$COMPOSE_MODE" in
  local|local-public) ;;
  *) fail "SMOKE_COMPOSE_MODE must be local or local-public" ;;
esac

TMP_DIR="$(mktemp -d)"
COOKIE_JAR="${TMP_DIR}/cookies.txt"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

wait_url "${API_URL}/health" "admin health"
wait_url "${STREAM_URL}/health" "stream health"

login_body="$(python3 - "$ADMIN_USERNAME" "$ADMIN_PASSWORD" <<'PY'
import json
import sys

print(json.dumps({"username": sys.argv[1], "password": sys.argv[2]}, separators=(",", ":")))
PY
)"
login_response="$(request POST "${API_URL}/auth/login" "$login_body")"
login_user="$(printf '%s' "$login_response" | json_get username)"
[[ "$login_user" == "$ADMIN_USERNAME" ]] || fail "Login response did not include the expected username"
[[ -n "$(csrf_token || true)" ]] || fail "Login did not set the CSRF cookie"
log "Authenticated as ${ADMIN_USERNAME}"

if [[ -n "${SMOKE_VIDEO_PATH:-}" ]]; then
  VIDEO_PATH="$SMOKE_VIDEO_PATH"
  [[ -f "$VIDEO_PATH" ]] || fail "SMOKE_VIDEO_PATH does not exist: ${VIDEO_PATH}"
else
  VIDEO_PATH="${TMP_DIR}/autvid-smoke.mp4"
  generate_video "$VIDEO_PATH"
fi

if bool "$LARGE_ORIGINAL"; then
  log "Expanding source file to ${LARGE_ORIGINAL_BYTES} bytes for direct-file-endpoint smoke"
  truncate -s "$LARGE_ORIGINAL_BYTES" "$VIDEO_PATH"
fi

SOURCE_BYTES="$(wc -c < "$VIDEO_PATH" | tr -d ' ')"
log "Using source ${VIDEO_PATH} (${SOURCE_BYTES} bytes), resolutions=${RESOLUTIONS}, upload_original=${UPLOAD_ORIGINAL}"

quote_request="$(python3 - "$RESOLUTIONS" "$SOURCE_BYTES" "$UPLOAD_ORIGINAL" <<'PY'
import json
import sys

resolutions = [part.strip() for part in sys.argv[1].split(",") if part.strip()]
source_bytes = int(sys.argv[2])
upload_original = sys.argv[3].lower() in {"1", "true", "yes", "on"}
payload = {
    "duration_seconds": 4.0,
    "resolutions": resolutions,
    "source_width": 640,
    "source_height": 360,
}
if upload_original:
    payload["upload_original"] = True
    payload["source_size_bytes"] = source_bytes
print(json.dumps(payload, separators=(",", ":")))
PY
)"
request POST "${API_URL}/videos/upload/quote" "$quote_request" >/dev/null
log "Initial upload quote succeeded"

upload_response="$(
  curl -fsS \
    -b "$COOKIE_JAR" \
    -c "$COOKIE_JAR" \
    -H "X-CSRF-Token: $(csrf_token)" \
    -F "file=@${VIDEO_PATH}" \
    -F "title=Smoke $(date -u +%Y%m%dT%H%M%SZ)" \
    -F "description=Automated local smoke test" \
    -F "resolutions=${RESOLUTIONS}" \
    -F "show_original_filename=false" \
    -F "show_manifest_address=${SHOW_MANIFEST_ADDRESS}" \
    -F "upload_original=${UPLOAD_ORIGINAL}" \
    -F "publish_when_ready=${PUBLISH_WHEN_READY}" \
    "${API_URL}/videos/upload"
)"
VIDEO_ID="$(printf '%s' "$upload_response" | json_get id)"
[[ -n "$VIDEO_ID" ]] || fail "Upload response did not include a video ID"
log "Uploaded video ${VIDEO_ID}"

if bool "$RESTART_ADMIN"; then
  need docker
  log "Restarting rust_admin immediately after upload to exercise durable recovery"
  compose restart rust_admin >/dev/null
  wait_url "${API_URL}/health" "admin health after restart"
  RESTART_ADMIN="false"
fi

wait_for_ready "$VIDEO_ID"
wait_for_public_catalog "$VIDEO_ID"

admin_video="$(request GET "${API_URL}/admin/videos/${VIDEO_ID}")"
manifest_address="$(printf '%s' "$admin_video" | json_get manifest_address || true)"

IFS=',' read -r -a resolution_list <<< "$RESOLUTIONS"
for resolution in "${resolution_list[@]}"; do
  resolution="${resolution//[[:space:]]/}"
  [[ -n "$resolution" ]] || continue
  fetch_playlist_and_segment "${STREAM_URL}/${VIDEO_ID}/${resolution}/playlist.m3u8" "public-${resolution}"
  if [[ -n "$manifest_address" ]]; then
    fetch_playlist_and_segment "${STREAM_URL}/manifest/${manifest_address}/${resolution}/playlist.m3u8" "manifest-${resolution}"
  fi
done

log "Smoke test completed successfully for video ${VIDEO_ID}"
