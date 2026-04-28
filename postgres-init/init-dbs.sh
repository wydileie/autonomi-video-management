#!/usr/bin/env bash
set -e

# Runs once when the Postgres container is first initialised.

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL

-- ── Admin / video service ─────────────────────────────────────────────────────
CREATE DATABASE $ADMIN_DB;
CREATE USER $ADMIN_USER WITH ENCRYPTED PASSWORD '$ADMIN_PASS';
GRANT ALL PRIVILEGES ON DATABASE $ADMIN_DB TO $ADMIN_USER;

EOSQL

# Connect to the admin DB and create the video schema
psql -v ON_ERROR_STOP=1 --username "$ADMIN_USER" --dbname "$ADMIN_DB" <<-EOSQL

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE IF NOT EXISTS videos (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    title            TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    description      TEXT,
    status           TEXT NOT NULL DEFAULT 'pending',
    manifest_address TEXT,
    catalog_address  TEXT,
    error_message    TEXT,
    job_dir          TEXT,
    final_quote      JSONB,
    final_quote_created_at TIMESTAMPTZ,
    approval_expires_at TIMESTAMPTZ,
    show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
    show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE,
    created_at       TIMESTAMPTZ DEFAULT NOW(),
    updated_at       TIMESTAMPTZ DEFAULT NOW(),
    user_id          TEXT
);

CREATE TABLE IF NOT EXISTS video_variants (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    video_id         UUID NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    resolution       TEXT NOT NULL,
    width            INTEGER NOT NULL,
    height           INTEGER NOT NULL,
    video_bitrate    INTEGER NOT NULL,
    audio_bitrate    INTEGER NOT NULL,
    segment_duration FLOAT NOT NULL DEFAULT 10.0,
    total_duration   FLOAT,
    segment_count    INTEGER,
    created_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS video_segments (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    variant_id       UUID NOT NULL REFERENCES video_variants(id) ON DELETE CASCADE,
    segment_index    INTEGER NOT NULL,
    autonomi_address TEXT NOT NULL,
    autonomi_cost_atto TEXT,
    autonomi_payment_mode TEXT,
    duration         FLOAT NOT NULL DEFAULT 10.0,
    byte_size        BIGINT,
    local_path       TEXT,
    created_at       TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (variant_id, segment_index)
);

CREATE INDEX IF NOT EXISTS idx_videos_status    ON videos(status);
CREATE INDEX IF NOT EXISTS idx_variants_video   ON video_variants(video_id);
CREATE INDEX IF NOT EXISTS idx_segments_variant ON video_segments(variant_id);

EOSQL
