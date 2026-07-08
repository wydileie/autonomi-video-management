use chrono::Utc;
use tracing::info;
use uuid::Uuid;

use sqlx::Row;

use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    models::JobKind,
    state::AppState,
    JOB_KIND_PUBLISH_CATALOG, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING,
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

async fn enqueue_video_job(
    state: &AppState,
    kind: JobKind,
    video_id: Uuid,
) -> Result<(), ApiError> {
    let job_id = Uuid::new_v4();
    let now = Utc::now();
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (id, job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, $2, $3, $4, $5, $6
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$2
              AND video_id=$3
              AND status IN ($4, $7)
        )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(job_id)
    .bind(kind.as_str())
    .bind(video_id)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.admin_job_max_attempts)
    .bind(now)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable {:?} job for video {}", kind, video_id);
        notify_job_workers(state, &format!("{}:{video_id}", kind.as_str())).await;
    } else {
        info!(
            "Durable {:?} job for video {} is already queued or running",
            kind, video_id
        );
    }
    Ok(())
}

pub(super) async fn enqueue_catalog_publish_job(state: &AppState) -> Result<(), ApiError> {
    let job_id = Uuid::new_v4();
    let now = Utc::now();
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (id, job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, $2, NULL, $3, $4, $5
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$2
              AND status=$3
        )
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(job_id)
    .bind(JOB_KIND_PUBLISH_CATALOG)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.catalog_publish_job_max_attempts)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable catalog publish job");
        notify_job_workers(state, JOB_KIND_PUBLISH_CATALOG).await;
    } else {
        info!("Durable catalog publish job is already queued");
    }
    Ok(())
}

async fn notify_job_workers(state: &AppState, payload: &str) {
    let next_epoch = state.job_notify_tx.borrow().saturating_add(1);
    let _ = state.job_notify_tx.send(next_epoch);
    info!("Notified durable job workers: {}", payload);
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
