-- SQLite requires table replacement to expand CHECK constraints. ensure_schema
-- disables FK enforcement only on its dedicated migration connection, then
-- checks all references and reenables enforcement before serving requests.
CREATE TABLE videos_new (
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
    encode_settings TEXT,
    CHECK (status IN (
        'pending',
        'processing',
        'awaiting_approval',
        'uploading',
        'ready',
        'error',
        'expired',
        'quoting',
        'approval_required',
        'payment_recovery_required'
    ))
);
INSERT INTO videos_new SELECT * FROM videos;
DROP TABLE videos;
ALTER TABLE videos_new RENAME TO videos;
CREATE TABLE video_jobs_new (
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
    CHECK (job_kind IN ('process_video', 'upload_video', 'publish_catalog', 'quote_video', 'finalize_catalog')),
    CHECK (status IN ('queued', 'running', 'succeeded', 'failed'))
);
INSERT INTO video_jobs_new SELECT * FROM video_jobs;
DROP TABLE video_jobs;
ALTER TABLE video_jobs_new RENAME TO video_jobs;
CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
CREATE INDEX IF NOT EXISTS idx_videos_is_public ON videos(is_public);
CREATE INDEX IF NOT EXISTS idx_video_jobs_ready ON video_jobs(status, run_after);
CREATE INDEX IF NOT EXISTS idx_video_jobs_video ON video_jobs(video_id);
CREATE INDEX IF NOT EXISTS idx_video_jobs_lease ON video_jobs(status, lease_expires_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_video_kind
    ON video_jobs(job_kind, video_id)
    WHERE status IN ('queued', 'running') AND video_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_publish_catalog
    ON video_jobs(job_kind)
    WHERE status IN ('queued', 'running')
      AND video_id IS NULL
      AND job_kind = 'publish_catalog';

-- Preserve all local media and public addresses. Legacy pending approvals cannot
-- authorize the new signer; they must be regenerated from the actual content.
ALTER TABLE videos ADD COLUMN approved_quote_id TEXT;
CREATE TABLE application_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
UPDATE videos SET status='approval_required', error_message='Regenerate the legacy quote to approve content, storage and gas caps.'
WHERE status='awaiting_approval';
ALTER TABLE video_jobs ADD COLUMN payment_quote_id TEXT;
CREATE TABLE catalog_approvals (id TEXT PRIMARY KEY, plan TEXT NOT NULL, state TEXT NOT NULL, error_message TEXT, created_at TEXT NOT NULL);
CREATE UNIQUE INDEX idx_video_jobs_active_finalize_catalog ON video_jobs(job_kind)
WHERE status IN ('queued','running') AND job_kind='finalize_catalog';
CREATE TABLE catalog_generation (singleton INTEGER PRIMARY KEY CHECK(singleton=1), value INTEGER NOT NULL);
INSERT INTO catalog_generation VALUES(1,0);
CREATE TRIGGER catalog_video_insert AFTER INSERT ON videos WHEN NEW.status='ready'
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
CREATE TRIGGER catalog_video_delete AFTER DELETE ON videos WHEN OLD.status='ready'
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
CREATE TRIGGER catalog_video_update AFTER UPDATE OF status,title,description,updated_at,is_public,show_manifest_address,manifest_address ON videos
WHEN OLD.status='ready' OR NEW.status='ready'
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
CREATE TRIGGER catalog_variant_update AFTER UPDATE ON video_variants
WHEN EXISTS(SELECT 1 FROM videos WHERE id=NEW.video_id AND status='ready')
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
CREATE TRIGGER catalog_variant_insert AFTER INSERT ON video_variants
WHEN EXISTS(SELECT 1 FROM videos WHERE id=NEW.video_id AND status='ready')
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
CREATE TRIGGER catalog_variant_delete AFTER DELETE ON video_variants
WHEN EXISTS(SELECT 1 FROM videos WHERE id=OLD.video_id AND status='ready')
BEGIN UPDATE catalog_generation SET value=value+1 WHERE singleton=1; END;
