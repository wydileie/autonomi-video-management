#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/backup-production.sh [--output-dir DIR] [--timestamp TIMESTAMP]

Creates a timestamped production backup directory containing:
  - postgres.dump      Custom-format pg_dump of ADMIN_DB
  - catalog.json       Latest catalog bookmark, when present
  - manifest.env       Backup metadata

Environment overrides:
  DOCKER_COMPOSE       Compose command (default: docker compose)
  COMPOSE_ENV_FILE     Compose env file (default: .env.production)
  COMPOSE_FILES        Space-separated compose files
                       (default: docker-compose.yml docker-compose.prod.yml)
  BACKUP_OUTPUT_DIR    Backup parent directory (default: ./backups)
  BACKUP_PREFIX        Backup directory prefix (default: autvid)
  DB_SERVICE           Compose Postgres service (default: db)
  CATALOG_SERVICE      Compose service with catalog volume (default: init_permissions)
  CATALOG_PATH         Catalog bookmark path in container (default: /catalog/catalog.json)
EOF
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"

output_dir="${BACKUP_OUTPUT_DIR:-${repo_root}/backups}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir)
      [[ $# -ge 2 ]] || { echo "Missing value for --output-dir" >&2; exit 2; }
      output_dir="$2"
      shift 2
      ;;
    --timestamp)
      [[ $# -ge 2 ]] || { echo "Missing value for --timestamp" >&2; exit 2; }
      timestamp="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

docker_compose="${DOCKER_COMPOSE:-docker compose}"
compose_env_file="${COMPOSE_ENV_FILE:-.env.production}"
compose_files="${COMPOSE_FILES:-docker-compose.yml docker-compose.prod.yml}"
backup_prefix="${BACKUP_PREFIX:-autvid}"
db_service="${DB_SERVICE:-db}"
catalog_service="${CATALOG_SERVICE:-init_permissions}"
catalog_path="${CATALOG_PATH:-/catalog/catalog.json}"

case "${output_dir}" in
  /*) ;;
  *) output_dir="$(pwd)/${output_dir}" ;;
esac

read -r -a compose_cmd <<< "${docker_compose}"
compose=("${compose_cmd[@]}" --env-file "${compose_env_file}")
for compose_file in ${compose_files}; do
  compose+=(-f "${compose_file}")
done

mkdir -p "${output_dir}"
backup_dir="${output_dir}/${backup_prefix}-${timestamp}"
if [[ -e "${backup_dir}" ]]; then
  echo "Backup path already exists: ${backup_dir}" >&2
  exit 1
fi
mkdir -p "${backup_dir}"

cd "${repo_root}"

echo "Writing Postgres backup to ${backup_dir}/postgres.dump"
"${compose[@]}" exec -T "${db_service}" sh -ceu \
  'pg_dump --format=custom --no-owner --no-acl -U "$ADMIN_USER" "$ADMIN_DB"' \
  > "${backup_dir}/postgres.dump"

echo "Writing catalog bookmark to ${backup_dir}/catalog.json when present"
if "${compose[@]}" run --rm --no-deps -T --entrypoint sh "${catalog_service}" -ceu \
  "if [ -f '${catalog_path}' ]; then cat '${catalog_path}'; fi" \
  > "${backup_dir}/catalog.json"; then
  if [[ ! -s "${backup_dir}/catalog.json" ]]; then
    rm -f "${backup_dir}/catalog.json"
    catalog_status="absent"
  else
    catalog_status="present"
  fi
else
  rm -f "${backup_dir}/catalog.json"
  catalog_status="failed"
  echo "Warning: catalog bookmark backup failed; Postgres backup was created" >&2
fi

cat > "${backup_dir}/manifest.env" <<EOF
BACKUP_CREATED_UTC=${timestamp}
COMPOSE_ENV_FILE=${compose_env_file}
COMPOSE_FILES=${compose_files}
DB_SERVICE=${db_service}
CATALOG_SERVICE=${catalog_service}
CATALOG_PATH=${catalog_path}
CATALOG_STATUS=${catalog_status}
POSTGRES_DUMP=postgres.dump
CATALOG_FILE=catalog.json
EOF

echo "Backup complete: ${backup_dir}"
