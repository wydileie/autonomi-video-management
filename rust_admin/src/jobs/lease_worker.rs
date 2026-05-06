use std::{fs, path::PathBuf, sync::atomic::Ordering, time::Duration as StdDuration};

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use sqlx::Row;
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use super::{
    recovery::{decode_requested_resolutions, recover_source_path},
    scheduling::{fetch_job_dir, job_retry_delay_seconds},
};
use crate::{
    catalog::publish_current_catalog_to_network,
    db::{db_error, set_status},
    errors::ApiError,
    models::{JobKind, LeasedJob},
    pipeline::{process_video_inner, upload_approved_video_inner},
    state::AppState,
    JOB_STATUS_FAILED, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING, JOB_STATUS_SUCCEEDED, STATUS_ERROR,
    STATUS_PENDING, STATUS_PROCESSING, STATUS_UPLOADING,
};

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

pub(super) async fn acquire_next_job(
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
pub(super) async fn mark_job_failed(
    state: &AppState,
    job: &LeasedJob,
    detail: &str,
) -> Result<(), ApiError> {
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
