#!/usr/bin/env bash
set -e

# This script will run once when the Postgres container is first started,
# thanks to the official Postgres image behavior with /docker-entrypoint-initdb.d/ scripts.

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL

-- 1) Create Keycloak DB & user
CREATE DATABASE $KEYCLOAK_DB;
CREATE USER $KEYCLOAK_USER WITH ENCRYPTED PASSWORD '$KEYCLOAK_PASS';
GRANT ALL PRIVILEGES ON DATABASE $KEYCLOAK_DB TO $KEYCLOAK_USER;

-- 2) Create Python Admin DB & user
CREATE DATABASE $ADMIN_DB;
CREATE USER $ADMIN_USER WITH ENCRYPTED PASSWORD '$ADMIN_PASS';
GRANT ALL PRIVILEGES ON DATABASE $ADMIN_DB TO $ADMIN_USER;

EOSQL
