CREATE TABLE IF NOT EXISTS videos (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    manifest_address TEXT,
    catalog_address TEXT,
    all_catalog_address TEXT,
    error_message TEXT,
    job_dir TEXT,
    job_source_path TEXT,
    requested_resolutions TEXT,
    final_quote TEXT,
    final_quote_created_at TEXT,
    approval_expires_at TEXT,
    is_public INTEGER NOT NULL DEFAULT 0,
    show_original_filename INTEGER NOT NULL DEFAULT 0,
    show_manifest_address INTEGER NOT NULL DEFAULT 0,
    upload_original INTEGER NOT NULL DEFAULT 0,
    original_file_address TEXT,
    original_file_byte_size INTEGER,
    original_file_autonomi_cost_atto TEXT,
    original_file_autonomi_payment_mode TEXT,
    publish_when_ready INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    user_id TEXT,
    CHECK (status IN (
        'pending',
        'processing',
        'awaiting_approval',
        'uploading',
        'ready',
        'error',
        'expired'
    ))
);

CREATE TABLE IF NOT EXISTS video_variants (
    id TEXT PRIMARY KEY,
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    resolution TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    video_bitrate INTEGER NOT NULL,
    audio_bitrate INTEGER NOT NULL,
    segment_duration REAL NOT NULL DEFAULT 10.0,
    total_duration REAL,
    segment_count INTEGER,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE IF NOT EXISTS video_segments (
    id TEXT PRIMARY KEY,
    variant_id TEXT NOT NULL REFERENCES video_variants(id) ON DELETE CASCADE,
    segment_index INTEGER NOT NULL,
    autonomi_address TEXT,
    autonomi_cost_atto TEXT,
    autonomi_payment_mode TEXT,
    duration REAL NOT NULL DEFAULT 10.0,
    byte_size INTEGER,
    local_path TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (variant_id, segment_index)
);

CREATE TABLE IF NOT EXISTS video_jobs (
    id TEXT PRIMARY KEY,
    job_kind TEXT NOT NULL,
    video_id TEXT REFERENCES videos(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    lease_owner TEXT,
    lease_expires_at TEXT,
    run_after TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    CHECK (job_kind IN ('process_video', 'upload_video', 'publish_catalog')),
    CHECK (status IN ('queued', 'running', 'succeeded', 'failed'))
);

CREATE TABLE IF NOT EXISTS admin_refresh_sessions (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    last_used_at TEXT,
    replaced_by_session_id TEXT REFERENCES admin_refresh_sessions(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
CREATE INDEX IF NOT EXISTS idx_videos_is_public ON videos(is_public);
CREATE INDEX IF NOT EXISTS idx_variants_video ON video_variants(video_id);
CREATE INDEX IF NOT EXISTS idx_segments_variant ON video_segments(variant_id);
CREATE INDEX IF NOT EXISTS idx_video_jobs_ready ON video_jobs(status, run_after);
CREATE INDEX IF NOT EXISTS idx_video_jobs_video ON video_jobs(video_id);
CREATE INDEX IF NOT EXISTS idx_video_jobs_lease ON video_jobs(status, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_admin_refresh_sessions_valid
    ON admin_refresh_sessions(username, expires_at)
    WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_admin_refresh_sessions_expires_at
    ON admin_refresh_sessions(expires_at);

CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_video_kind
    ON video_jobs(job_kind, video_id)
    WHERE status IN ('queued', 'running') AND video_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_publish_catalog
    ON video_jobs(job_kind)
    WHERE status IN ('queued', 'running')
      AND video_id IS NULL
      AND job_kind = 'publish_catalog';
