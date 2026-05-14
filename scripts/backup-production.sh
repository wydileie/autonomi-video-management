#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/backup-production.sh [--output-dir DIR] [--timestamp TIMESTAMP]

Creates a timestamped production backup directory containing:
  - autvid.sqlite3     SQLite database copy
  - autvid.sqlite3-wal SQLite WAL file, when present
  - autvid.sqlite3-shm SQLite shared-memory file, when present
  - catalog.json       Latest catalog state, when present
  - manifest.env       Backup metadata

Environment overrides:
  BACKUP_OUTPUT_DIR    Backup parent directory (default: ./backups)
  BACKUP_PREFIX        Backup directory prefix (default: autvid)
  AUTVID_DATA_HOST_PATH Host app-data path (default: ./.autvid/app_data)
  SQLITE_DB_NAME       SQLite database filename (default: autvid.sqlite3)
  CATALOG_PATH         Catalog state path (default: AUTVID_DATA_HOST_PATH/catalog/catalog.json)
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

backup_prefix="${BACKUP_PREFIX:-autvid}"
data_host_path="${AUTVID_DATA_HOST_PATH:-./.autvid/app_data}"
sqlite_db_name="${SQLITE_DB_NAME:-autvid.sqlite3}"

case "${output_dir}" in
  /*) ;;
  *) output_dir="$(pwd)/${output_dir}" ;;
esac

mkdir -p "${output_dir}"
backup_dir="${output_dir}/${backup_prefix}-${timestamp}"
if [[ -e "${backup_dir}" ]]; then
  echo "Backup path already exists: ${backup_dir}" >&2
  exit 1
fi
mkdir -p "${backup_dir}"

cd "${repo_root}"

case "${data_host_path}" in
  /*) ;;
  *) data_host_path="${repo_root}/${data_host_path}" ;;
esac
catalog_path="${CATALOG_PATH:-${data_host_path}/catalog/catalog.json}"
db_path="${data_host_path%/}/${sqlite_db_name}"

if [[ ! -r "${db_path}" ]]; then
  echo "SQLite database is not readable: ${db_path}" >&2
  exit 1
fi

echo "Writing SQLite backup to ${backup_dir}/${sqlite_db_name}"
cp "${db_path}" "${backup_dir}/${sqlite_db_name}"
for suffix in -wal -shm; do
  if [[ -r "${db_path}${suffix}" ]]; then
    cp "${db_path}${suffix}" "${backup_dir}/${sqlite_db_name}${suffix}"
  fi
done

echo "Writing catalog state to ${backup_dir}/catalog.json when present"
if [[ -r "${catalog_path}" ]]; then
  cp "${catalog_path}" "${backup_dir}/catalog.json"
  catalog_status="present"
else
  if [[ ! -s "${backup_dir}/catalog.json" ]]; then
    rm -f "${backup_dir}/catalog.json"
    catalog_status="absent"
  fi
fi

cat > "${backup_dir}/manifest.env" <<EOF
BACKUP_CREATED_UTC=${timestamp}
AUTVID_DATA_HOST_PATH=${data_host_path}
SQLITE_DB_PATH=${db_path}
CATALOG_PATH=${catalog_path}
CATALOG_STATUS=${catalog_status}
SQLITE_DB_FILE=${sqlite_db_name}
CATALOG_FILE=catalog.json
EOF

echo "Backup complete: ${backup_dir}"
