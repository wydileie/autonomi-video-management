CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_video_kind
    ON video_jobs(job_kind, video_id)
    WHERE status IN ('queued', 'running') AND video_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_video_jobs_active_publish_catalog
    ON video_jobs(job_kind)
    WHERE status IN ('queued', 'running')
      AND video_id IS NULL
      AND job_kind = 'publish_catalog';
