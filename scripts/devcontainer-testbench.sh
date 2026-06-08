#!/usr/bin/env bash
set -Eeuo pipefail

IMAGE="${DEVBENCH_IMAGE:-autvid-devcontainer-testbench}"
CONTAINER="${DEVBENCH_CONTAINER:-autvid_devcontainer_testbench}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE="${DEVBENCH_WORKSPACE:-/workspace}"
DEV_PATH="/opt/venv/bin:/usr/local/python/current/bin:/usr/local/py-utils/bin:/usr/local/jupyter:/usr/local/share/nvm/current/bin:/usr/local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/mssql-tools18/bin:/usr/local/cargo/bin"

usage() {
  cat <<'USAGE'
Usage: scripts/devcontainer-testbench.sh <command> [args...]

Run the VS Code devcontainer image as a headless Codex test bench.

Commands:
  build             Build the devcontainer image
  up                Start the detached test bench and run postStart setup
  down              Stop and remove the detached test bench
  restart           Recreate the detached test bench
  status            Show container status and key environment checks
  shell             Open an interactive shell as the vscode user
  exec <cmd...>     Run a command as the vscode user
  logs              Show recent container logs

Environment:
  DEVBENCH_IMAGE      Docker image tag. Default: autvid-devcontainer-testbench
  DEVBENCH_CONTAINER  Container name. Default: autvid_devcontainer_testbench

The script keeps VS Code devcontainers working by using the same Dockerfile, but
it does not require VS Code to be open.
USAGE
}

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 127
  }
}

host_env() {
  local name="$1"
  local value="${!name:-}"
  if [[ -z "$value" && "$(uname -s)" == "Darwin" ]] && command -v launchctl >/dev/null 2>&1; then
    value="$(launchctl getenv "$name" 2>/dev/null || true)"
  fi
  if [[ -z "$value" && "$name" == "GITHUB_TOKEN" ]] && command -v gh >/dev/null 2>&1; then
    value="$(gh auth token 2>/dev/null || true)"
  fi
  printf '%s' "$value"
}

docker_sock_args() {
  if [[ -S /var/run/docker.sock ]]; then
    printf '%s\n' "-v" "/var/run/docker.sock:/var/run/docker.sock"
  fi
}

optional_home_mount_args() {
  local source="$1"
  local target="$2"
  if [[ -e "$source" ]]; then
    printf '%s\n' "-v" "${source}:${target}:cached"
  fi
}

container_exists() {
  docker container inspect "$CONTAINER" >/dev/null 2>&1
}

container_running() {
  [[ "$(docker container inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null || true)" == "true" ]]
}

build_image() {
  need docker
  docker build -t "$IMAGE" -f "$ROOT_DIR/.devcontainer/Dockerfile" "$ROOT_DIR/.devcontainer"
}

start_container() {
  need docker
  if container_running; then
    echo "$CONTAINER is already running"
    return 0
  fi
  if container_exists; then
    docker rm "$CONTAINER" >/dev/null
  fi

  local github_token brave_api_key autonomi_wallet_key
  github_token="$(host_env GITHUB_TOKEN)"
  brave_api_key="$(host_env BRAVE_API_KEY)"
  autonomi_wallet_key="$(host_env AUTONOMI_WALLET_KEY)"

  local -a args=(
    run -d
    --name "$CONTAINER"
    --workdir "$WORKSPACE"
    -v "$ROOT_DIR:$WORKSPACE:cached"
    -e ANTD_NETWORK="${ANTD_NETWORK:-local}"
    -e PATH="$DEV_PATH"
    -e GITHUB_TOKEN="$github_token"
    -e BRAVE_API_KEY="$brave_api_key"
    -e AUTONOMI_WALLET_KEY="$autonomi_wallet_key"
    -p "${DEVBENCH_HTTP_PORT:-18080}:80"
    -p "${DEVBENCH_ADMIN_PORT:-18000}:8000"
    -p "${DEVBENCH_STREAM_PORT:-18081}:8081"
    -p "${DEVBENCH_ANTD_REST_PORT:-18082}:8082"
    -p "${DEVBENCH_ANTD_GRPC_PORT:-15051}:50051"
  )

  while IFS= read -r item; do args+=("$item"); done < <(docker_sock_args)
  while IFS= read -r item; do args+=("$item"); done < <(optional_home_mount_args "$HOME/.claude" /home/vscode/.claude)
  while IFS= read -r item; do args+=("$item"); done < <(optional_home_mount_args "$HOME/.codex_vscode" /home/vscode/.codex)
  while IFS= read -r item; do args+=("$item"); done < <(optional_home_mount_args "$HOME/.config/gh" /home/vscode/.config/gh)

  args+=("$IMAGE" sleep infinity)
  docker "${args[@]}" >/dev/null

  docker exec --user vscode "$CONTAINER" bash -lc "export PATH='$DEV_PATH'; cd '$WORKSPACE' && bash .devcontainer/post_start.sh"
}

stop_container() {
  need docker
  if container_exists; then
    docker rm -f "$CONTAINER" >/dev/null
  fi
}

status_container() {
  need docker
  docker ps --filter "name=^/${CONTAINER}$" --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}'
  if container_running; then
    docker exec --user vscode "$CONTAINER" bash -lc '
      export PATH='"$DEV_PATH"'
      test -n "$GITHUB_TOKEN" && echo "GITHUB_TOKEN=set" || echo "GITHUB_TOKEN=missing"
      test -n "$BRAVE_API_KEY" && echo "BRAVE_API_KEY=set" || echo "BRAVE_API_KEY=missing"
      curl -fsS --max-time 2 http://localhost:8082/health >/dev/null && echo "antd=healthy" || echo "antd=not-ready"
    '
  fi
}

case "${1:-}" in
  build)
    build_image
    ;;
  up)
    build_image
    start_container
    ;;
  down)
    stop_container
    ;;
  restart)
    stop_container
    build_image
    start_container
    ;;
  status)
    status_container
    ;;
  shell)
    need docker
    container_running || start_container
    docker exec -it --user vscode "$CONTAINER" bash -lc "export PATH='$DEV_PATH'; cd '$WORKSPACE' && exec bash"
    ;;
  exec)
    shift
    [[ $# -gt 0 ]] || { echo "Usage: $0 exec <cmd...>" >&2; exit 2; }
    need docker
    container_running || start_container
    docker exec --user vscode "$CONTAINER" bash -lc "export PATH='$DEV_PATH'; cd '$WORKSPACE' && exec \"\$@\"" bash "$@"
    ;;
  logs)
    need docker
    docker logs "$CONTAINER" "${@:2}"
    ;;
  --help|-h|help|"")
    usage
    ;;
  *)
    echo "Unknown command: $1" >&2
    usage >&2
    exit 2
    ;;
esac
