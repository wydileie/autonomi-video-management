use std::{
    fs,
    path::{Path as FsPath, PathBuf},
    sync::atomic::Ordering,
    time::Duration as StdDuration,
};

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::Row;
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::{
    catalog::{publish_current_catalog_to_network, read_catalog_state_value},
    db::{db_error, parse_video_uuid, set_status},
    errors::ApiError,
    media::resolution_preset,
    models::{JobKind, LeasedJob},
    pipeline::{process_video_inner, upload_approved_video_inner},
    state::AppState,
    upload::parse_resolutions,
    JOB_KIND_PUBLISH_CATALOG, JOB_STATUS_FAILED, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING,
    JOB_STATUS_SUCCEEDED, STATUS_ERROR, STATUS_PENDING, STATUS_PROCESSING, STATUS_UPLOADING,
};

pub(crate) async fn schedule_processing_job(
    state: &AppState,
    video_id: &str,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    enqueue_video_job(state, JobKind::ProcessVideo, video_uuid).await
}

pub(crate) async fn schedule_upload_job(state: &AppState, video_id: &str) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    enqueue_video_job(state, JobKind::UploadVideo, video_uuid).await
}

pub(crate) fn start_job_workers(state: &AppState) {
    for worker_index in 0..state.config.admin_job_workers {
        let worker_id = format!("admin-{}-{worker_index}", Uuid::new_v4());
        tokio::spawn(job_worker_loop(state.clone(), worker_id));
    }
    info!(
        "Started {} durable admin job worker(s)",
        state.config.admin_job_workers
    );
}

async fn job_worker_loop(state: AppState, worker_id: String) {
    loop {
        match acquire_next_job(&state, &worker_id).await {
            Ok(Some(job)) => {
                let kind = job.kind;
                let job_id = job.id;
                state.metrics.record_job_started();
                let result = run_leased_job(&state, &job).await;
                match result {
                    Ok(()) => {
                        state.metrics.record_job_succeeded();
                        if let Err(err) = mark_job_succeeded(&state, job_id).await {
                            warn!(
                                "Worker {} could not mark {:?} job {} succeeded: {}",
                                worker_id, kind, job_id, err.detail
                            );
                        }
                    }
                    Err(err) => {
                        state.metrics.record_job_failed();
                        let detail = err.detail;
                        warn!(
                            "Worker {} {:?} job {} failed on attempt {}/{}: {}",
                            worker_id, kind, job_id, job.attempts, job.max_attempts, detail
                        );
                        if let Err(mark_err) = mark_job_failed(&state, &job, &detail).await {
                            warn!(
                                "Worker {} could not persist {:?} job {} failure: {}",
                                worker_id, kind, job_id, mark_err.detail
                            );
                        }
                    }
                }
            }
            Ok(None) => {
                sleep(StdDuration::from_secs(
                    state.config.admin_job_poll_interval_seconds,
                ))
                .await;
            }
            Err(err) => {
                warn!("Worker {} could not lease a job: {}", worker_id, err.detail);
                sleep(StdDuration::from_secs(
                    state.config.admin_job_poll_interval_seconds,
                ))
                .await;
            }
        }
    }
}

async fn enqueue_video_job(
    state: &AppState,
    kind: JobKind,
    video_id: Uuid,
) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, $2, $3, $4, NOW()
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$1
              AND video_id=$2
              AND status IN ($3, $5)
        )
        "#,
    )
    .bind(kind.as_str())
    .bind(video_id)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.admin_job_max_attempts)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable {:?} job for video {}", kind, video_id);
    } else {
        info!(
            "Durable {:?} job for video {} is already queued or running",
            kind, video_id
        );
    }
    Ok(())
}

async fn enqueue_catalog_publish_job(state: &AppState) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, NULL::uuid, $2, $3, NOW()
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$1
              AND status=$2
        )
        "#,
    )
    .bind(JOB_KIND_PUBLISH_CATALOG)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.catalog_publish_job_max_attempts)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable catalog publish job");
    } else {
        info!("Durable catalog publish job is already queued");
    }
    Ok(())
}

async fn acquire_next_job(
    state: &AppState,
    worker_id: &str,
) -> Result<Option<LeasedJob>, ApiError> {
    let row = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            attempts=attempts + 1,
            lease_owner=$2,
            lease_expires_at=NOW() + ($3::bigint * INTERVAL '1 second'),
            updated_at=NOW()
        WHERE id = (
            SELECT id
            FROM video_jobs
            WHERE (
                status=$4
                AND run_after <= NOW()
            ) OR (
                status=$1
                AND lease_expires_at IS NOT NULL
                AND lease_expires_at <= NOW()
            )
            ORDER BY run_after, created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        RETURNING id, job_kind, video_id, attempts, max_attempts
        "#,
    )
    .bind(JOB_STATUS_RUNNING)
    .bind(worker_id)
    .bind(state.config.admin_job_lease_seconds)
    .bind(JOB_STATUS_QUEUED)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = row.try_get("id").map_err(db_error)?;
    let kind_raw: String = row.try_get("job_kind").map_err(db_error)?;
    let kind = JobKind::parse(&kind_raw).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unknown durable job kind {kind_raw:?}"),
        )
    })?;
    Ok(Some(LeasedJob {
        id,
        kind,
        video_id: row.try_get("video_id").map_err(db_error)?,
        attempts: row.try_get("attempts").map_err(db_error)?,
        max_attempts: row.try_get("max_attempts").map_err(db_error)?,
    }))
}

#[instrument(skip(state, job), fields(job_id = %job.id, job_kind = ?job.kind, video_id = ?job.video_id))]
async fn run_leased_job(state: &AppState, job: &LeasedJob) -> Result<(), ApiError> {
    match job.kind {
        JobKind::ProcessVideo => run_process_video_job(state, job.video_id).await,
        JobKind::UploadVideo => run_upload_video_job(state, job.video_id).await,
        JobKind::PublishCatalog => run_catalog_publish_job(state).await,
    }
}

#[instrument(skip(state), fields(video_id = ?video_uuid))]
async fn run_process_video_job(state: &AppState, video_uuid: Option<Uuid>) -> Result<(), ApiError> {
    let video_uuid = video_uuid.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Process video job is missing video_id",
        )
    })?;
    let video_id = video_uuid.to_string();
    let row = sqlx::query(
        r#"
        SELECT status, job_dir, job_source_path, requested_resolutions
        FROM videos
        WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        info!("Skipping process job for deleted video {}", video_id);
        return Ok(());
    };
    let status: String = row.try_get("status").map_err(db_error)?;
    if !matches!(status.as_str(), STATUS_PENDING | STATUS_PROCESSING) {
        info!(
            "Skipping process job for video {} because status is {}",
            video_id, status
        );
        return Ok(());
    }

    let job_dir = row
        .try_get::<Option<String>, _>("job_dir")
        .map_err(db_error)?
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Processing job is missing job_dir",
            )
        })?;
    let resolutions =
        decode_requested_resolutions(row.try_get("requested_resolutions").map_err(db_error)?);
    let job_source_path: Option<String> = row.try_get("job_source_path").map_err(db_error)?;
    let source_path =
        recover_source_path(Some(&job_dir), job_source_path.as_deref()).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Processing job is missing its source file",
            )
        })?;
    if resolutions.is_empty() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Processing job has no supported requested resolutions",
        ));
    }

    process_video_inner(state, &video_id, &source_path, &resolutions, &job_dir, true).await
}

#[instrument(skip(state), fields(video_id = ?video_uuid))]
async fn run_upload_video_job(state: &AppState, video_uuid: Option<Uuid>) -> Result<(), ApiError> {
    let video_uuid = video_uuid.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Upload video job is missing video_id",
        )
    })?;
    let video_id = video_uuid.to_string();
    let row = sqlx::query("SELECT status FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    let Some(row) = row else {
        info!("Skipping upload job for deleted video {}", video_id);
        return Ok(());
    };
    let status: String = row.try_get("status").map_err(db_error)?;
    if status != STATUS_UPLOADING {
        info!(
            "Skipping upload job for video {} because status is {}",
            video_id, status
        );
        return Ok(());
    }

    upload_approved_video_inner(state, &video_id).await
}

#[instrument(skip(state), fields(catalog_publish_epoch = state.catalog_publish_epoch.load(Ordering::SeqCst)))]
async fn run_catalog_publish_job(state: &AppState) -> Result<(), ApiError> {
    let epoch = state.catalog_publish_epoch.load(Ordering::SeqCst);
    publish_current_catalog_to_network(state, epoch, "durable-job").await
}

#[instrument(skip(state), fields(job_id = %job_id))]
async fn mark_job_succeeded(state: &AppState, job_id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            last_error=NULL,
            updated_at=NOW()
        WHERE id=$2
        "#,
    )
    .bind(JOB_STATUS_SUCCEEDED)
    .bind(job_id)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

#[instrument(skip(state, job, detail), fields(job_id = %job.id, job_kind = ?job.kind, video_id = ?job.video_id))]
async fn mark_job_failed(state: &AppState, job: &LeasedJob, detail: &str) -> Result<(), ApiError> {
    let final_failure = job.attempts >= job.max_attempts;
    if final_failure {
        sqlx::query(
            r#"
            UPDATE video_jobs
            SET status=$1,
                lease_owner=NULL,
                lease_expires_at=NULL,
                last_error=$2,
                updated_at=NOW()
            WHERE id=$3
            "#,
        )
        .bind(JOB_STATUS_FAILED)
        .bind(detail)
        .bind(job.id)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;
        handle_final_job_failure(state, job, detail).await;
        return Ok(());
    }

    let delay_seconds = job_retry_delay_seconds(job.attempts);
    let run_after = Utc::now() + Duration::seconds(delay_seconds);
    sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=$2,
            last_error=$3,
            updated_at=NOW()
        WHERE id=$4
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(run_after)
    .bind(detail)
    .bind(job.id)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    info!(
        "Retrying {:?} job {} in {}s after attempt {}/{}",
        job.kind, job.id, delay_seconds, job.attempts, job.max_attempts
    );
    Ok(())
}

async fn handle_final_job_failure(state: &AppState, job: &LeasedJob, detail: &str) {
    let Some(video_uuid) = job.video_id else {
        error!(
            "{:?} job {} failed permanently after {} attempt(s): {}",
            job.kind, job.id, job.attempts, detail
        );
        return;
    };
    let video_id = video_uuid.to_string();
    error!(
        "{:?} job {} for video {} failed permanently after {} attempt(s): {}",
        job.kind, job.id, video_id, job.attempts, detail
    );
    if matches!(job.kind, JobKind::ProcessVideo | JobKind::UploadVideo) {
        let _ = set_status(state, &video_id, STATUS_ERROR, Some(detail)).await;
        if let Ok(Some(job_dir)) = fetch_job_dir(state, &video_id).await {
            let _ = fs::remove_dir_all(job_dir);
        }
    }
}

pub(crate) fn job_retry_delay_seconds(attempts: i32) -> i64 {
    let exponent = attempts.saturating_sub(1).min(5) as u32;
    (30_i64 * 2_i64.pow(exponent)).min(15 * 60)
}

pub(crate) async fn schedule_catalog_publish(
    state: &AppState,
    epoch: u64,
    reason: impl Into<String>,
) -> Result<(), ApiError> {
    let reason = reason.into();
    enqueue_catalog_publish_job(state).await?;
    info!(
        "Queued durable catalog publish epoch={} reason={}",
        epoch, reason
    );
    Ok(())
}

pub(crate) async fn fetch_job_dir(
    state: &AppState,
    video_id: &str,
) -> Result<Option<String>, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let row = sqlx::query("SELECT job_dir FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    Ok(row.and_then(|row| row.try_get::<Option<String>, _>("job_dir").ok().flatten()))
}

pub(crate) async fn cleanup_expired_approvals(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"
        UPDATE videos
        SET status='expired',
            error_message='Final quote approval window expired; local files were deleted.',
            updated_at=NOW()
        WHERE status='awaiting_approval'
          AND approval_expires_at IS NOT NULL
          AND approval_expires_at <= NOW()
        RETURNING id, job_dir
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for row in rows {
        if let Ok(Some(job_dir)) = row.try_get::<Option<String>, _>("job_dir") {
            let _ = fs::remove_dir_all(job_dir);
        }
        if let Ok(video_id) = row.try_get::<Uuid, _>("id") {
            info!(
                "Expired awaiting approval video {} and removed local files",
                video_id
            );
        }
    }
    Ok(())
}

pub(crate) async fn approval_cleanup_loop(state: AppState) {
    loop {
        sleep(StdDuration::from_secs(
            state.config.approval_cleanup_interval_seconds,
        ))
        .await;
        if let Err(err) = cleanup_expired_approvals(&state).await {
            warn!("Approval cleanup failed: {}", err);
        }
    }
}

pub(crate) async fn recover_interrupted_jobs(state: AppState) -> anyhow::Result<()> {
    let reset_jobs = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=NOW(),
            updated_at=NOW()
        WHERE status=$2
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await?;
    if reset_jobs.rows_affected() > 0 {
        info!(
            "Requeued {} running durable job(s) from a previous admin process",
            reset_jobs.rows_affected()
        );
    }

    let rows = sqlx::query(
        r#"
        SELECT id, status, job_dir, job_source_path, requested_resolutions
        FROM videos
        WHERE status IN ('pending', 'processing', 'uploading')
        ORDER BY created_at
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut recovered_processing = 0;
    let mut recovered_uploads = 0;
    for row in rows {
        let video_id: Uuid = row.try_get("id")?;
        let video_id = video_id.to_string();
        let status: String = row.try_get("status")?;
        let job_dir = row
            .try_get::<Option<String>, _>("job_dir")?
            .map(PathBuf::from);

        if matches!(status.as_str(), STATUS_PENDING | STATUS_PROCESSING) {
            let resolutions = decode_requested_resolutions(
                row.try_get::<Option<Value>, _>("requested_resolutions")?,
            );
            let source_path = recover_source_path(
                job_dir.as_deref(),
                row.try_get::<Option<String>, _>("job_source_path")?
                    .as_deref(),
            );
            if job_dir.as_ref().is_none_or(|path| !path.exists())
                || source_path.is_none()
                || resolutions.is_empty()
            {
                let _ = set_status(
                    &state,
                    &video_id,
                    STATUS_ERROR,
                    Some(
                        "Interrupted processing job could not be recovered because its source file or requested resolutions were missing.",
                    ),
                )
                .await;
                warn!("Could not recover interrupted processing job {}", video_id);
                continue;
            }

            schedule_processing_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_processing += 1;
        } else if status == STATUS_UPLOADING {
            schedule_upload_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_uploads += 1;
        }
    }

    if recovered_processing > 0 || recovered_uploads > 0 {
        info!(
            "Recovered interrupted jobs: processing={} uploading={}",
            recovered_processing, recovered_uploads
        );
    }
    if read_catalog_state_value(&state.config)
        .and_then(|value| value.get("publish_pending").and_then(Value::as_bool))
        .unwrap_or(false)
    {
        enqueue_catalog_publish_job(&state)
            .await
            .map_err(|err| anyhow::anyhow!(err.detail))?;
        info!("Recovered pending catalog publish from local catalog state");
    }
    Ok(())
}

fn decode_requested_resolutions(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .filter(|item| resolution_preset(item).is_some())
            .collect(),
        Some(Value::String(value)) => parse_resolutions(&value),
        _ => vec![],
    }
}

fn recover_source_path(job_dir: Option<&FsPath>, job_source_path: Option<&str>) -> Option<PathBuf> {
    if let Some(source_path) = job_source_path
        .map(PathBuf::from)
        .filter(|path| path.exists())
    {
        return Some(source_path);
    }

    let job_dir = job_dir?;
    let mut matches = fs::read_dir(job_dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("original_"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use serde_json::json;
    use uuid::Uuid;

    use super::{decode_requested_resolutions, job_retry_delay_seconds, recover_source_path};

    #[test]
    fn durable_job_retry_backoff_caps() {
        assert_eq!(job_retry_delay_seconds(1), 30);
        assert_eq!(job_retry_delay_seconds(2), 60);
        assert_eq!(job_retry_delay_seconds(6), 900);
        assert_eq!(job_retry_delay_seconds(20), 900);
    }

    #[test]
    fn recovery_resolution_decode_accepts_arrays_and_legacy_strings() {
        assert_eq!(
            decode_requested_resolutions(Some(json!(["1080p", "720p", "bogus"]))),
            vec!["1080p".to_string(), "720p".to_string()]
        );
        assert_eq!(
            decode_requested_resolutions(Some(json!("480p,360p,unknown"))),
            vec!["480p".to_string(), "360p".to_string()]
        );
        assert!(decode_requested_resolutions(Some(json!({ "bad": true }))).is_empty());
    }

    #[test]
    fn recovery_source_path_prefers_existing_explicit_path_then_original_file() {
        let dir = std::env::temp_dir().join(format!("autvid_job_recover_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let explicit = dir.join("custom-source.mp4");
        let fallback = dir.join("original_upload.mp4");
        fs::write(&explicit, b"explicit").unwrap();
        fs::write(&fallback, b"fallback").unwrap();

        assert_eq!(
            recover_source_path(Some(&dir), Some(explicit.to_str().unwrap())),
            Some(explicit.clone())
        );

        assert_eq!(
            recover_source_path(Some(&dir), Some("/definitely/missing/source.mp4")),
            Some(fallback.clone())
        );

        assert_eq!(
            recover_source_path(Some(&PathBuf::from("/definitely/missing/job-dir")), None),
            None
        );

        let _ = fs::remove_dir_all(dir);
    }
}
