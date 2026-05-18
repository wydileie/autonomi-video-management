#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/restore-production.sh --backup-dir DIR --yes
  scripts/restore-production.sh --db-file FILE [--catalog-file FILE] --yes

Restores production state from explicit backup files. This is destructive:
the SQLite database is replaced, and the catalog state is overwritten when
--catalog-file is provided or DIR/catalog.json exists. Stop the stack first.

Required safety flag:
  --yes                 Confirm the destructive restore

Environment overrides:
  AUTVID_DATA_HOST_PATH Host app-data path (default: ./.autvid/app_data)
  SQLITE_DB_NAME        SQLite database filename (default: autvid.sqlite3)
  CATALOG_PATH          Catalog state path (default: AUTVID_DATA_HOST_PATH/catalog/catalog.json)
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
  db_file="${backup_dir%/}/${SQLITE_DB_NAME:-autvid.sqlite3}"
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
  echo "SQLite backup is not readable: ${db_file}" >&2
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

cd "${repo_root}"

data_host_path="${AUTVID_DATA_HOST_PATH:-./.autvid/app_data}"
sqlite_db_name="${SQLITE_DB_NAME:-autvid.sqlite3}"
case "${data_host_path}" in
  /*) ;;
  *) data_host_path="${repo_root}/${data_host_path}" ;;
esac
target_db="${data_host_path%/}/${sqlite_db_name}"
catalog_path="${CATALOG_PATH:-${data_host_path}/catalog/catalog.json}"

mkdir -p "$(dirname "${target_db}")"

echo "Restoring SQLite database to ${target_db}"
cp "${db_file}" "${target_db}"
# Repo-created backups contain only the online-backup .sqlite3 file. Keep
# sidecar handling for legacy/manual backups, and remove stale sidecars when
# they are absent from the backup.
for suffix in -wal -shm; do
  source_sidecar="$(dirname "${db_file}")/$(basename "${db_file}")${suffix}"
  if [[ -r "${source_sidecar}" ]]; then
    cp "${source_sidecar}" "${target_db}${suffix}"
  else
    rm -f "${target_db}${suffix}"
  fi
done

if [[ -n "${catalog_file}" ]]; then
  echo "Restoring catalog state from ${catalog_file}"
  mkdir -p "$(dirname "${catalog_path}")"
  cp "${catalog_file}" "${catalog_path}"
else
  echo "No catalog state file provided; leaving catalog state unchanged"
fi

echo "Restore complete"
