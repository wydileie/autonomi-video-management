#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/restore-production.sh --backup-dir DIR --yes
  scripts/restore-production.sh --db-file FILE [--catalog-file FILE] --yes

Restores production state from explicit backup files. This is destructive:
Postgres objects in ADMIN_DB are dropped/replaced, and the catalog bookmark is
overwritten when --catalog-file is provided or DIR/catalog.json exists.

Required safety flag:
  --yes                 Confirm the destructive restore

Environment overrides:
  DOCKER_COMPOSE        Compose command (default: docker compose)
  COMPOSE_ENV_FILE      Compose env file (default: .env.production)
  COMPOSE_FILES         Space-separated compose files
                        (default: docker-compose.yml docker-compose.prod.yml)
  DB_SERVICE            Compose Postgres service (default: db)
  CATALOG_SERVICE       Compose service with catalog volume (default: init_permissions)
  CATALOG_PATH          Catalog bookmark path in container (default: /catalog/catalog.json)
EOF
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"

backup_dir=""
db_file=""
catalog_file=""
confirmed="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --backup-dir)
      [[ $# -ge 2 ]] || { echo "Missing value for --backup-dir" >&2; exit 2; }
      backup_dir="$2"
      shift 2
      ;;
    --db-file)
      [[ $# -ge 2 ]] || { echo "Missing value for --db-file" >&2; exit 2; }
      db_file="$2"
      shift 2
      ;;
    --catalog-file)
      [[ $# -ge 2 ]] || { echo "Missing value for --catalog-file" >&2; exit 2; }
      catalog_file="$2"
      shift 2
      ;;
    --yes)
      confirmed="true"
      shift
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

if [[ "${confirmed}" != "true" ]]; then
  echo "Refusing destructive restore without --yes" >&2
  usage >&2
  exit 2
fi

if [[ -n "${backup_dir}" ]]; then
  [[ -z "${db_file}" ]] || { echo "Use either --backup-dir or --db-file, not both" >&2; exit 2; }
  db_file="${backup_dir%/}/postgres.dump"
  if [[ -z "${catalog_file}" && -f "${backup_dir%/}/catalog.json" ]]; then
    catalog_file="${backup_dir%/}/catalog.json"
  fi
fi

if [[ -z "${db_file}" ]]; then
  echo "Restore requires --backup-dir or --db-file" >&2
  usage >&2
  exit 2
fi

if [[ ! -r "${db_file}" ]]; then
  echo "Postgres backup is not readable: ${db_file}" >&2
  exit 1
fi
db_file="$(cd "$(dirname "${db_file}")" && pwd)/$(basename "${db_file}")"

if [[ -n "${catalog_file}" && ! -r "${catalog_file}" ]]; then
  echo "Catalog backup is not readable: ${catalog_file}" >&2
  exit 1
fi
if [[ -n "${catalog_file}" ]]; then
  catalog_file="$(cd "$(dirname "${catalog_file}")" && pwd)/$(basename "${catalog_file}")"
fi

docker_compose="${DOCKER_COMPOSE:-docker compose}"
compose_env_file="${COMPOSE_ENV_FILE:-.env.production}"
compose_files="${COMPOSE_FILES:-docker-compose.yml docker-compose.prod.yml}"
db_service="${DB_SERVICE:-db}"
catalog_service="${CATALOG_SERVICE:-init_permissions}"
catalog_path="${CATALOG_PATH:-/catalog/catalog.json}"

read -r -a compose_cmd <<< "${docker_compose}"
compose=("${compose_cmd[@]}" --env-file "${compose_env_file}")
for compose_file in ${compose_files}; do
  compose+=(-f "${compose_file}")
done

cd "${repo_root}"

echo "Restoring Postgres from ${db_file}"
"${compose[@]}" exec -T "${db_service}" sh -ceu \
  'pg_restore --clean --if-exists --no-owner --no-acl -U "$ADMIN_USER" -d "$ADMIN_DB"' \
  < "${db_file}"

if [[ -n "${catalog_file}" ]]; then
  echo "Restoring catalog bookmark from ${catalog_file}"
  "${compose[@]}" run --rm --no-deps -T --entrypoint sh "${catalog_service}" -ceu \
    "mkdir -p \"\$(dirname '${catalog_path}')\"; umask 077; cat > '${catalog_path}'; chown 1000:1000 '${catalog_path}'" \
    < "${catalog_file}"
else
  echo "No catalog bookmark file provided; leaving catalog state unchanged"
fi

echo "Restore complete"
