CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE IF NOT EXISTS videos (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    title TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    manifest_address TEXT,
    catalog_address TEXT,
    error_message TEXT,
    job_dir TEXT,
    job_source_path TEXT,
    requested_resolutions JSONB,
    final_quote JSONB,
    final_quote_created_at TIMESTAMPTZ,
    approval_expires_at TIMESTAMPTZ,
    is_public BOOLEAN NOT NULL DEFAULT FALSE,
    show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
    show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE,
    upload_original BOOLEAN NOT NULL DEFAULT FALSE,
    original_file_address TEXT,
    original_file_byte_size BIGINT,
    original_file_autonomi_cost_atto TEXT,
    original_file_autonomi_payment_mode TEXT,
    publish_when_ready BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    user_id TEXT
);

ALTER TABLE videos
    ADD COLUMN IF NOT EXISTS manifest_address TEXT,
    ADD COLUMN IF NOT EXISTS catalog_address TEXT,
    ADD COLUMN IF NOT EXISTS error_message TEXT,
    ADD COLUMN IF NOT EXISTS job_dir TEXT,
    ADD COLUMN IF NOT EXISTS job_source_path TEXT,
    ADD COLUMN IF NOT EXISTS requested_resolutions JSONB,
    ADD COLUMN IF NOT EXISTS final_quote JSONB,
    ADD COLUMN IF NOT EXISTS final_quote_created_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS approval_expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS upload_original BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS original_file_address TEXT,
    ADD COLUMN IF NOT EXISTS original_file_byte_size BIGINT,
    ADD COLUMN IF NOT EXISTS original_file_autonomi_cost_atto TEXT,
    ADD COLUMN IF NOT EXISTS original_file_autonomi_payment_mode TEXT,
    ADD COLUMN IF NOT EXISTS publish_when_ready BOOLEAN NOT NULL DEFAULT FALSE;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'videos_status_check'
    ) THEN
        ALTER TABLE videos
            ADD CONSTRAINT videos_status_check
            CHECK (status IN (
                'pending',
                'processing',
                'awaiting_approval',
                'uploading',
                'ready',
                'error',
                'expired'
            ));
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS video_variants (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    video_id UUID NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    resolution TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    video_bitrate INTEGER NOT NULL,
    audio_bitrate INTEGER NOT NULL,
    segment_duration FLOAT NOT NULL DEFAULT 10.0,
    total_duration FLOAT,
    segment_count INTEGER,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS video_segments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    variant_id UUID NOT NULL REFERENCES video_variants(id) ON DELETE CASCADE,
    segment_index INTEGER NOT NULL,
    autonomi_address TEXT,
    autonomi_cost_atto TEXT,
    autonomi_payment_mode TEXT,
    duration FLOAT NOT NULL DEFAULT 10.0,
    byte_size BIGINT,
    local_path TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (variant_id, segment_index)
);

ALTER TABLE video_segments
    ADD COLUMN IF NOT EXISTS autonomi_cost_atto TEXT,
    ADD COLUMN IF NOT EXISTS autonomi_payment_mode TEXT,
    ADD COLUMN IF NOT EXISTS local_path TEXT;

ALTER TABLE video_segments
    ALTER COLUMN autonomi_address DROP NOT NULL;

CREATE TABLE IF NOT EXISTS video_jobs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    job_kind TEXT NOT NULL,
    video_id UUID REFERENCES videos(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    run_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

ALTER TABLE video_jobs
    ADD COLUMN IF NOT EXISTS job_kind TEXT,
    ADD COLUMN IF NOT EXISTS video_id UUID REFERENCES videos(id) ON DELETE CASCADE,
    ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'queued',
    ADD COLUMN IF NOT EXISTS attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS max_attempts INTEGER NOT NULL DEFAULT 3,
    ADD COLUMN IF NOT EXISTS lease_owner TEXT,
    ADD COLUMN IF NOT EXISTS lease_expires_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS run_after TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ADD COLUMN IF NOT EXISTS last_error TEXT,
    ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ DEFAULT NOW(),
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ DEFAULT NOW();

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'video_jobs_kind_check'
    ) THEN
        ALTER TABLE video_jobs
            ADD CONSTRAINT video_jobs_kind_check
            CHECK (job_kind IN ('process_video', 'upload_video', 'publish_catalog'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'video_jobs_status_check'
    ) THEN
        ALTER TABLE video_jobs
            ADD CONSTRAINT video_jobs_status_check
            CHECK (status IN ('queued', 'running', 'succeeded', 'failed'));
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
CREATE INDEX IF NOT EXISTS idx_videos_is_public ON videos(is_public);
CREATE INDEX IF NOT EXISTS idx_variants_video ON video_variants(video_id);
CREATE INDEX IF NOT EXISTS idx_segments_variant ON video_segments(variant_id);
CREATE INDEX IF NOT EXISTS idx_video_jobs_ready ON video_jobs(status, run_after);
CREATE INDEX IF NOT EXISTS idx_video_jobs_video ON video_jobs(video_id);
CREATE INDEX IF NOT EXISTS idx_video_jobs_lease ON video_jobs(status, lease_expires_at);

UPDATE videos
SET show_original_filename = FALSE
WHERE show_original_filename = TRUE;
