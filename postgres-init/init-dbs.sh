#!/usr/bin/env bash
set -e

# Runs once when the Postgres container is first initialised.

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL

-- ── Admin / video service ─────────────────────────────────────────────────────
CREATE DATABASE $ADMIN_DB;
CREATE USER $ADMIN_USER WITH ENCRYPTED PASSWORD '$ADMIN_PASS';
GRANT ALL PRIVILEGES ON DATABASE $ADMIN_DB TO $ADMIN_USER;

EOSQL

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$ADMIN_DB" <<-EOSQL

ALTER SCHEMA public OWNER TO $ADMIN_USER;
GRANT ALL ON SCHEMA public TO $ADMIN_USER;

EOSQL
