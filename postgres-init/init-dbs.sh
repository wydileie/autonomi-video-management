#!/usr/bin/env bash
set -Eeuo pipefail

# Runs once when the Postgres container is first initialised.

read_secret() {
  local env_name="$1"
  local file_name="$2"
  local file_path="${!file_name:-}"

  if [[ -n "${file_path}" ]]; then
    if [[ ! -r "${file_path}" ]]; then
      echo "${file_name} is not readable: ${file_path}" >&2
      exit 2
    fi
    head -n 1 "${file_path}"
    return
  fi

  printf '%s' "${!env_name:-}"
}

: "${ADMIN_DB:?ADMIN_DB is required}"
: "${ADMIN_USER:?ADMIN_USER is required}"
: "${BACKUP_USER:=autvid_backup}"

ADMIN_PASS="$(read_secret ADMIN_PASS ADMIN_PASS_FILE)"
BACKUP_PASS="$(read_secret BACKUP_PASS BACKUP_PASS_FILE)"

if [[ -z "${ADMIN_PASS}" ]]; then
  echo "ADMIN_PASS or ADMIN_PASS_FILE is required" >&2
  exit 2
fi

if [[ -z "${BACKUP_PASS}" ]]; then
  echo "BACKUP_PASS or BACKUP_PASS_FILE is required" >&2
  exit 2
fi

psql \
  -v ON_ERROR_STOP=1 \
  -v admin_db="${ADMIN_DB}" \
  -v admin_user="${ADMIN_USER}" \
  -v admin_pass="${ADMIN_PASS}" \
  -v backup_user="${BACKUP_USER}" \
  -v backup_pass="${BACKUP_PASS}" \
  --username "$POSTGRES_USER" <<-'EOSQL'

-- ── Admin / video service ─────────────────────────────────────────────────────
CREATE DATABASE :"admin_db";
CREATE USER :"admin_user" WITH ENCRYPTED PASSWORD :'admin_pass';
CREATE USER :"backup_user" WITH ENCRYPTED PASSWORD :'backup_pass';
GRANT ALL PRIVILEGES ON DATABASE :"admin_db" TO :"admin_user";
GRANT CONNECT ON DATABASE :"admin_db" TO :"backup_user";

EOSQL

psql \
  -v ON_ERROR_STOP=1 \
  -v admin_user="${ADMIN_USER}" \
  -v backup_user="${BACKUP_USER}" \
  --username "$POSTGRES_USER" \
  --dbname "$ADMIN_DB" <<-'EOSQL'

ALTER SCHEMA public OWNER TO :"admin_user";
GRANT ALL ON SCHEMA public TO :"admin_user";
GRANT USAGE ON SCHEMA public TO :"backup_user";
GRANT SELECT ON ALL TABLES IN SCHEMA public TO :"backup_user";
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO :"backup_user";
ALTER DEFAULT PRIVILEGES FOR USER :"admin_user" IN SCHEMA public
  GRANT SELECT ON TABLES TO :"backup_user";
ALTER DEFAULT PRIVILEGES FOR USER :"admin_user" IN SCHEMA public
  GRANT USAGE, SELECT ON SEQUENCES TO :"backup_user";

EOSQL
