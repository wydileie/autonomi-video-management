#!/usr/bin/env bash
set -Eeuo pipefail

log() {
  printf '%s %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*"
}

usage() {
  cat <<'EOF'
Usage:
  backup-sidecar schedule
  backup-sidecar run

Environment:
  SQLITE_DB_PATH             SQLite database path (default: /var/lib/autvid/autvid.sqlite3)
  BACKUP_DIR                 Backup parent directory (default: /backups)
  BACKUP_PREFIX              Backup directory prefix (default: autvid)
  BACKUP_SCHEDULE            daily@HH:MM, interval:SECONDS, or once
                             (default: daily@02:00)
  BACKUP_RUN_ON_START        Run once before entering schedule loop (default: false)
  BACKUP_RETENTION_DAYS      Delete matching backups older than N days; 0 disables
                             (default: 14)
  BACKUP_RETENTION_COUNT     Keep only newest N matching backups; 0 disables
                             (default: 0)
  BACKUP_CATALOG             Copy CATALOG_PATH when present (default: true)
  CATALOG_PATH               Catalog state path (default: /var/lib/autvid/catalog/catalog.json)
  BACKUP_FILE_OWNER          Optional numeric owner for completed backups, UID:GID
  BACKUP_SQLITE_WAIT_SECONDS Seconds to wait for the SQLite database (default: 120)
                             BACKUP_DB_WAIT_SECONDS is accepted as a legacy alias.
  BACKUP_TEXTFILE_DIR        Optional node-exporter textfile collector directory
  BACKUP_TEXTFILE_NAME       Metric filename in BACKUP_TEXTFILE_DIR
                             (default: autvid_backup.prom)
EOF
}

is_true() {
  case "${1:-}" in
    true|TRUE|True|1|yes|YES|Yes|y|Y) return 0 ;;
    *) return 1 ;;
  esac
}

is_nonnegative_integer() {
  [[ "${1:-}" =~ ^[0-9]+$ ]]
}

require_nonnegative_integer() {
  local name="$1"
  local value="$2"

  if ! is_nonnegative_integer "${value}"; then
    log "Invalid ${name}: ${value}. Expected a non-negative integer."
    exit 2
  fi
}

validate_config() {
  if [[ "${BACKUP_DIR}" == "/" ]]; then
    log "Refusing to use / as BACKUP_DIR"
    exit 2
  fi

  if [[ "${BACKUP_PREFIX}" == *"/"* || -z "${BACKUP_PREFIX}" ]]; then
    log "BACKUP_PREFIX must be a non-empty directory-name prefix"
    exit 2
  fi

  require_nonnegative_integer BACKUP_RETENTION_DAYS "${BACKUP_RETENTION_DAYS}"
  require_nonnegative_integer BACKUP_RETENTION_COUNT "${BACKUP_RETENTION_COUNT}"
  require_nonnegative_integer BACKUP_SQLITE_WAIT_SECONDS "${BACKUP_SQLITE_WAIT_SECONDS}"

  if [[ -n "${BACKUP_FILE_OWNER:-}" && ! "${BACKUP_FILE_OWNER}" =~ ^[0-9]+:[0-9]+$ ]]; then
    log "BACKUP_FILE_OWNER must be numeric UID:GID when set"
    exit 2
  fi

  if [[ -n "${BACKUP_TEXTFILE_NAME:-}" && "${BACKUP_TEXTFILE_NAME}" == *"/"* ]]; then
    log "BACKUP_TEXTFILE_NAME must be a filename, not a path"
    exit 2
  fi
}

wait_for_sqlite() {
  local started_at
  local now

  started_at="$(date -u +%s)"
  while [[ ! -r "${SQLITE_DB_PATH}" ]]; do
    now="$(date -u +%s)"
    if (( now - started_at >= BACKUP_SQLITE_WAIT_SECONDS )); then
      log "SQLite database did not become readable within ${BACKUP_SQLITE_WAIT_SECONDS}s: ${SQLITE_DB_PATH}"
      return 1
    fi
    sleep 2
  done
}

write_manifest() {
  local backup_dir="$1"
  local timestamp="$2"
  local catalog_status="$3"

  cat > "${backup_dir}/manifest.env" <<EOF
BACKUP_CREATED_UTC=${timestamp}
BACKUP_PREFIX=${BACKUP_PREFIX}
SQLITE_DB_PATH=${SQLITE_DB_PATH}
SQLITE_DB_FILE=autvid.sqlite3
CATALOG_PATH=${CATALOG_PATH}
CATALOG_STATUS=${catalog_status}
CATALOG_FILE=catalog.json
BACKUP_SCHEDULE=${BACKUP_SCHEDULE}
BACKUP_RETENTION_DAYS=${BACKUP_RETENTION_DAYS}
BACKUP_RETENTION_COUNT=${BACKUP_RETENTION_COUNT}
EOF
}

write_checksums() {
  local backup_dir="$1"

  (
    cd "${backup_dir}" || exit 1
    if [[ -f catalog.json ]]; then
      sha256sum autvid.sqlite3 catalog.json manifest.env > SHA256SUMS || exit 1
    else
      sha256sum autvid.sqlite3 manifest.env > SHA256SUMS || exit 1
    fi
  )
}

run_backup() {
  local timestamp
  local backup_dir
  local tmp_dir
  local catalog_status

  mkdir -p "${BACKUP_DIR}" || return 1

  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  backup_dir="${BACKUP_DIR%/}/${BACKUP_PREFIX}-${timestamp}"
  tmp_dir="${backup_dir}.tmp"

  if [[ -e "${backup_dir}" || -e "${tmp_dir}" ]]; then
    log "Backup path already exists: ${backup_dir}"
    return 1
  fi

  rm -rf "${tmp_dir}" || return 1
  mkdir -p "${tmp_dir}" || return 1
  trap 'rm -rf "${tmp_dir:-}"' RETURN

  log "Waiting for SQLite database at ${SQLITE_DB_PATH}"
  wait_for_sqlite || return 1

  log "Writing SQLite backup to ${backup_dir}/autvid.sqlite3"
  sqlite3 "${SQLITE_DB_PATH}" ".backup '${tmp_dir}/autvid.sqlite3'" || return 1

  catalog_status="disabled"
  if is_true "${BACKUP_CATALOG}"; then
    if [[ -f "${CATALOG_PATH}" ]]; then
      log "Copying catalog bookmark from ${CATALOG_PATH}"
      cp "${CATALOG_PATH}" "${tmp_dir}/catalog.json" || return 1
      catalog_status="present"
    else
      log "Catalog bookmark is absent at ${CATALOG_PATH}"
      catalog_status="absent"
    fi
  fi

  write_manifest "${tmp_dir}" "${timestamp}" "${catalog_status}" || return 1
  write_checksums "${tmp_dir}" || return 1
  chmod -R go-rwx "${tmp_dir}" || return 1

  if [[ -n "${BACKUP_FILE_OWNER:-}" ]]; then
    chown -R "${BACKUP_FILE_OWNER}" "${tmp_dir}" || return 1
  fi

  mv "${tmp_dir}" "${backup_dir}" || return 1
  trap - RETURN
  LAST_BACKUP_DIR="${backup_dir}"
  log "Backup complete: ${backup_dir}"
}

cleanup_by_age() {
  if (( BACKUP_RETENTION_DAYS == 0 )); then
    return 0
  fi

  log "Deleting ${BACKUP_PREFIX}-* backups older than ${BACKUP_RETENTION_DAYS} days"
  find "${BACKUP_DIR}" \
    -mindepth 1 \
    -maxdepth 1 \
    -type d \
    -name "${BACKUP_PREFIX}-*" \
    -mtime "+${BACKUP_RETENTION_DAYS}" \
    -print \
    -exec rm -rf {} +
}

cleanup_by_count() {
  local retention_count="${BACKUP_RETENTION_COUNT}"
  local excess
  local i
  local backup_dirs=()

  if (( retention_count == 0 )); then
    return 0
  fi

  mapfile -t backup_dirs < <(
    find "${BACKUP_DIR}" \
      -mindepth 1 \
      -maxdepth 1 \
      -type d \
      -name "${BACKUP_PREFIX}-*" \
      -print | sort
  )

  excess=$(( ${#backup_dirs[@]} - retention_count ))
  if (( excess <= 0 )); then
    return 0
  fi

  log "Deleting ${excess} oldest ${BACKUP_PREFIX}-* backups to keep ${retention_count}"
  for (( i = 0; i < excess; i++ )); do
    log "Deleting ${backup_dirs[i]}"
    rm -rf "${backup_dirs[i]}"
  done
}

cleanup_backups() {
  mkdir -p "${BACKUP_DIR}" || return 1
  cleanup_by_age
  cleanup_by_count
}

backup_count() {
  find "${BACKUP_DIR}" \
    -mindepth 1 \
    -maxdepth 1 \
    -type d \
    -name "${BACKUP_PREFIX}-*" \
    -print | wc -l
}

write_backup_metrics() {
  local status="$1"
  local backup_dir="${2:-}"
  local started_at="$3"
  local finished_at
  local duration
  local backup_size
  local backup_created
  local retained_count
  local textfile_dir="${BACKUP_TEXTFILE_DIR:-}"
  local textfile_name="${BACKUP_TEXTFILE_NAME:-autvid_backup.prom}"
  local tmp_file
  local metric_file

  if [[ -z "${textfile_dir}" ]]; then
    return 0
  fi

  finished_at="$(date -u +%s)"
  duration=$(( finished_at - started_at ))
  backup_size=0
  backup_created=0
  retained_count=0

  if [[ "${status}" == "success" && -n "${backup_dir}" && -d "${backup_dir}" ]]; then
    backup_size="$(du -sb "${backup_dir}" | awk '{print $1}')"
    backup_created="${finished_at}"
  fi

  if [[ -d "${BACKUP_DIR}" ]]; then
    retained_count="$(backup_count)"
  fi

  mkdir -p "${textfile_dir}" || return 1
  tmp_file="${textfile_dir%/}/.${textfile_name}.$$"
  metric_file="${textfile_dir%/}/${textfile_name}"

  if [[ "${status}" != "success" && -f "${metric_file}" ]]; then
    backup_created="$(awk '/^autvid_backup_last_success_timestamp_seconds / {print $2}' "${metric_file}" | tail -n 1)"
    if ! [[ "${backup_created}" =~ ^[0-9]+$ ]]; then
      backup_created=0
    fi
  fi

  cat > "${tmp_file}" <<EOF
# HELP autvid_backup_last_success_timestamp_seconds Unix timestamp of the last successful AutVid backup.
# TYPE autvid_backup_last_success_timestamp_seconds gauge
autvid_backup_last_success_timestamp_seconds ${backup_created}
# HELP autvid_backup_last_run_timestamp_seconds Unix timestamp of the last AutVid backup attempt.
# TYPE autvid_backup_last_run_timestamp_seconds gauge
autvid_backup_last_run_timestamp_seconds ${finished_at}
# HELP autvid_backup_last_duration_seconds Duration of the last AutVid backup attempt.
# TYPE autvid_backup_last_duration_seconds gauge
autvid_backup_last_duration_seconds ${duration}
# HELP autvid_backup_last_size_bytes Size of the last successful AutVid backup directory.
# TYPE autvid_backup_last_size_bytes gauge
autvid_backup_last_size_bytes ${backup_size}
# HELP autvid_backup_last_status Last AutVid backup status, labeled by status.
# TYPE autvid_backup_last_status gauge
autvid_backup_last_status{status="success"} $([[ "${status}" == "success" ]] && printf '1' || printf '0')
autvid_backup_last_status{status="failure"} $([[ "${status}" == "failure" ]] && printf '1' || printf '0')
# HELP autvid_backup_retained_count Number of retained AutVid backup directories.
# TYPE autvid_backup_retained_count gauge
autvid_backup_retained_count ${retained_count}
EOF
  mv "${tmp_file}" "${metric_file}" || return 1
}

run_backup_cycle() {
  local started_at

  LAST_BACKUP_DIR=""
  started_at="$(date -u +%s)"
  if run_backup; then
    if ! cleanup_backups; then
      write_backup_metrics failure "" "${started_at}" || log "Could not write backup textfile metrics"
      return 1
    fi
    write_backup_metrics success "${LAST_BACKUP_DIR}" "${started_at}" || log "Could not write backup textfile metrics"
    return 0
  fi

  write_backup_metrics failure "" "${started_at}" || log "Could not write backup textfile metrics"
  return 1
}

run_once() {
  validate_config
  run_backup_cycle
}

interval_sleep_seconds() {
  local value="${BACKUP_SCHEDULE#interval:}"

  if ! [[ "${value}" =~ ^[1-9][0-9]*$ ]]; then
    log "Invalid BACKUP_SCHEDULE=${BACKUP_SCHEDULE}. Expected interval:SECONDS."
    exit 2
  fi

  printf '%s\n' "${value}"
}

daily_sleep_seconds() {
  local daily_time="${BACKUP_SCHEDULE#daily@}"
  local today
  local target_epoch
  local now_epoch

  if ! [[ "${daily_time}" =~ ^([01][0-9]|2[0-3]):[0-5][0-9]$ ]]; then
    log "Invalid BACKUP_SCHEDULE=${BACKUP_SCHEDULE}. Expected daily@HH:MM in UTC."
    exit 2
  fi

  today="$(date -u +%F)"
  now_epoch="$(date -u +%s)"
  target_epoch="$(date -u -d "${today} ${daily_time}:00 UTC" +%s)"

  if (( target_epoch <= now_epoch )); then
    target_epoch="$(( target_epoch + 86400 ))"
  fi

  printf '%s\n' "$(( target_epoch - now_epoch ))"
}

next_sleep_seconds() {
  case "${BACKUP_SCHEDULE}" in
    interval:*) interval_sleep_seconds ;;
    daily@*) daily_sleep_seconds ;;
    once) printf '0\n' ;;
    *)
      log "Invalid BACKUP_SCHEDULE=${BACKUP_SCHEDULE}. Use daily@HH:MM, interval:SECONDS, or once."
      exit 2
      ;;
  esac
}

schedule_loop() {
  local sleep_seconds

  validate_config

  if [[ "${BACKUP_SCHEDULE}" == "once" ]]; then
    run_backup_cycle
    return 0
  fi

  if is_true "${BACKUP_RUN_ON_START}"; then
    if ! run_backup_cycle; then
      log "Initial backup failed; continuing scheduler"
    fi
  fi

  while true; do
    sleep_seconds="$(next_sleep_seconds)"
    log "Next backup in ${sleep_seconds}s using BACKUP_SCHEDULE=${BACKUP_SCHEDULE}"
    sleep "${sleep_seconds}"

    if ! run_backup_cycle; then
      log "Scheduled backup failed; waiting for the next run"
      continue
    fi
  done
}

main() {
  export SQLITE_DB_PATH="${SQLITE_DB_PATH:-/var/lib/autvid/autvid.sqlite3}"
  export BACKUP_DIR="${BACKUP_DIR:-/backups}"
  export BACKUP_PREFIX="${BACKUP_PREFIX:-autvid}"
  export BACKUP_SCHEDULE="${BACKUP_SCHEDULE:-daily@02:00}"
  export BACKUP_RUN_ON_START="${BACKUP_RUN_ON_START:-false}"
  export BACKUP_RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-14}"
  export BACKUP_RETENTION_COUNT="${BACKUP_RETENTION_COUNT:-0}"
  export BACKUP_CATALOG="${BACKUP_CATALOG:-true}"
  export CATALOG_PATH="${CATALOG_PATH:-/var/lib/autvid/catalog/catalog.json}"
  export BACKUP_SQLITE_WAIT_SECONDS="${BACKUP_SQLITE_WAIT_SECONDS:-${BACKUP_DB_WAIT_SECONDS:-120}}"
  export BACKUP_TEXTFILE_DIR="${BACKUP_TEXTFILE_DIR:-}"
  export BACKUP_TEXTFILE_NAME="${BACKUP_TEXTFILE_NAME:-autvid_backup.prom}"

  trap 'log "Shutdown requested"; exit 0' INT TERM

  case "${1:-schedule}" in
    schedule) schedule_loop ;;
    run|once) run_once ;;
    -h|--help|help) usage ;;
    *)
      log "Unknown command: $1"
      usage >&2
      exit 2
      ;;
  esac
}

main "$@"
