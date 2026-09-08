use std::{path::PathBuf, sync::atomic::Ordering, time::Duration as StdDuration};

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use futures_util::FutureExt;
use sqlx::Row;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::{info, instrument, warn};
use uuid::Uuid;

use super::{
    recovery::{decode_requested_resolutions, recover_source_path},
    scheduling::job_retry_delay_seconds,
};
use crate::{
    catalog::publish_current_catalog_to_network,
    db::db_error,
    errors::ApiError,
    models::{EncodeSettings, JobKind, LeasedJob},
    pipeline::{process_video_inner, upload_approved_video_inner},
    state::AppState,
    JOB_STATUS_FAILED, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING, JOB_STATUS_SUCCEEDED, STATUS_ERROR,
    STATUS_PENDING, STATUS_PROCESSING, STATUS_UPLOADING,
};

pub(crate) fn start_job_workers(state: &AppState) -> Vec<(String, JoinHandle<()>)> {
    let mut handles = Vec::with_capacity(state.config.admin_job_workers);
    for worker_index in 0..state.config.admin_job_workers {
        let worker_id = format!("admin-{}-{worker_index}", Uuid::new_v4());
        handles.push((
            format!("job-worker-{worker_index}"),
            tokio::spawn(job_worker_loop(state.clone(), worker_id)),
        ));
    }
    info!(
        "Started {} durable admin job worker(s)",
        state.config.admin_job_workers
    );
    handles
}

async fn job_worker_loop(state: AppState, worker_id: String) {
    let mut notifications = state.job_notify_tx.subscribe();
    loop {
        if state.shutdown.is_cancelled() {
            info!("Worker {} stopping after shutdown signal", worker_id);
            break;
        }
        match acquire_next_job(&state, &worker_id).await {
            Ok(Some(job)) => {
                let kind = job.kind;
                let job_id = job.id;
                state.metrics.record_job_started();
                let result = tokio::select! {
                    result = run_leased_job(&state, &job).boxed() => result,
                    error = maintain_lease(&state, &job) => {
                        warn!("Worker {} lost job {} lease: {}", worker_id, job_id, error.detail);
                        continue;
                    },
                    _ = state.shutdown.cancelled() => {
                        if let Err(err) = mark_job_interrupted(&state, job_id, &worker_id).await {
                            warn!(
                                "Worker {} could not release {:?} job {} during shutdown: {}",
                                worker_id, kind, job_id, err.detail
                            );
                        }
                        info!("Worker {} released {:?} job {} during shutdown", worker_id, kind, job_id);
                        break;
                    }
                };
                match result {
                    Ok(()) => {
                        state.metrics.record_job_succeeded();
                        if let Err(err) = mark_job_succeeded(&state, &job).await {
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
                wait_for_job_signal(&state, &worker_id, &mut notifications).await;
            }
            Err(err) => {
                warn!("Worker {} could not lease a job: {}", worker_id, err.detail);
                wait_for_job_signal(&state, &worker_id, &mut notifications).await;
            }
        }
    }
}

async fn wait_for_job_signal(
    state: &AppState,
    worker_id: &str,
    notifications: &mut watch::Receiver<u64>,
) {
    let poll_delay = StdDuration::from_secs(state.config.admin_job_poll_interval_seconds);
    tokio::select! {
        _ = state.shutdown.cancelled() => {},
        _ = sleep(poll_delay) => {},
        changed = notifications.changed() => {
            if changed.is_err() {
                warn!("Worker {} job notification channel closed; falling back to polling", worker_id);
            }
        }
    }
}

pub(super) async fn acquire_next_job(
    state: &AppState,
    worker_id: &str,
) -> Result<Option<LeasedJob>, ApiError> {
    let now = Utc::now();
    let lease_expires_at = now + Duration::seconds(state.config.admin_job_lease_seconds);
    let row = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            attempts=attempts + 1,
            lease_owner=$2,
            lease_expires_at=$3,
            updated_at=$4
        WHERE id = (
            SELECT id
            FROM video_jobs
            WHERE (
                status=$5
                AND run_after <= $4
            ) OR (
                status=$1
                AND lease_expires_at IS NOT NULL
                AND lease_expires_at <= $4
            )
            ORDER BY run_after, created_at
            LIMIT 1
        )
        RETURNING id, job_kind, video_id, attempts, max_attempts, run_after
        "#,
    )
    .bind(JOB_STATUS_RUNNING)
    .bind(worker_id)
    .bind(lease_expires_at)
    .bind(now)
    .bind(JOB_STATUS_QUEUED)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = row.try_get("id").map_err(db_error)?;
    let run_after: DateTime<Utc> = row.try_get("run_after").unwrap_or(now);
    let pickup_latency_ms = now
        .signed_duration_since(run_after)
        .num_milliseconds()
        .max(0) as u64;
    state
        .metrics
        .record_job_pickup_latency(StdDuration::from_millis(pickup_latency_ms));
    let kind_raw: String = row.try_get("job_kind").map_err(db_error)?;
    let kind = JobKind::parse(&kind_raw).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unknown durable job kind {kind_raw:?}"),
        )
    })?;
    Ok(Some(LeasedJob {
        lease_owner: worker_id.to_owned(),
        id,
        kind,
        video_id: row.try_get("video_id").map_err(db_error)?,
        attempts: row.try_get("attempts").map_err(db_error)?,
        max_attempts: row.try_get("max_attempts").map_err(db_error)?,
    }))
}

#[instrument(skip(state, job), fields(job_id = %job.id, job_kind = ?job.kind, video_id = ?job.video_id))]
async fn run_leased_job(state: &AppState, job: &LeasedJob) -> Result<(), ApiError> {
    let mut state = state.clone();
    state.active_job = Some(crate::state::JobLease {
        id: job.id,
        owner: job.lease_owner.clone(),
        generation: job.attempts,
    });
    if let Some(key) = update_payment_lease(&state, job).await? {
        state.antd = state.antd.with_lease(&key);
    }
    let state = &state;
    match job.kind {
        JobKind::ProcessVideo => run_process_video_job(state, job.video_id).await,
        JobKind::UploadVideo => run_upload_video_job(state, job.video_id).await,
        JobKind::PublishCatalog => run_catalog_publish_job(state).await,
        JobKind::FinalizeCatalog => crate::catalog::payments::finalize(state).await,
        JobKind::QuoteVideo => run_quote_video_job(state, job.video_id).await,
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
        SELECT status, job_dir, job_source_path, requested_resolutions, encode_settings
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
    let encode_settings = decode_encode_settings(row.try_get("encode_settings").map_err(db_error)?);

    process_video_inner(
        state,
        &video_id,
        &source_path,
        &resolutions,
        &job_dir,
        true,
        &encode_settings,
    )
    .await
}

fn decode_encode_settings(value: Option<serde_json::Value>) -> EncodeSettings {
    value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
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

#[instrument(skip(state, job), fields(job_id = %job.id))]
pub(super) async fn mark_job_succeeded(state: &AppState, job: &LeasedJob) -> Result<(), ApiError> {
    let now = Utc::now();
    let mut tx = crate::db::begin_immediate(&state.pool).await?;
    let updated = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            last_error=NULL,
            updated_at=$2
        WHERE id=$3 AND status=$4 AND lease_owner=$5 AND attempts=$6 AND lease_expires_at > $2
        "#,
    )
    .bind(JOB_STATUS_SUCCEEDED)
    .bind(now)
    .bind(job.id)
    .bind(JOB_STATUS_RUNNING)
    .bind(&job.lease_owner)
    .bind(job.attempts)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    let mut cleanup_dir = None;
    if updated.rows_affected() == 1 && job.kind == JobKind::UploadVideo {
        cleanup_dir = sqlx::query_scalar::<_, Option<String>>(
            "SELECT job_dir FROM videos WHERE id=$1 AND status='ready'",
        )
        .bind(job.video_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .flatten();
        sqlx::query(
            "UPDATE videos SET job_dir=NULL,job_source_path=NULL WHERE id=$1 AND status='ready'",
        )
        .bind(job.video_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;
    if let Some(directory) = cleanup_dir {
        if let Ok(directory) = crate::media::assert_under(
            std::path::Path::new(&directory),
            &state.config.upload_temp_dir,
        ) {
            if let Err(error) = tokio::fs::remove_dir_all(&directory).await {
                warn!(path=%directory.display(), %error, "Could not remove completed local upload files");
            }
        }
    }

    Ok(())
}

#[instrument(skip(state, worker_id), fields(job_id = %job_id))]
pub(super) async fn mark_job_interrupted(
    state: &AppState,
    job_id: Uuid,
    worker_id: &str,
) -> Result<(), ApiError> {
    let now = Utc::now();
    sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=$2,
            last_error='Interrupted by graceful shutdown; job was requeued.',
            updated_at=$2
        WHERE id=$3 AND status=$4 AND lease_owner=$5
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(now)
    .bind(job_id)
    .bind(JOB_STATUS_RUNNING)
    .bind(worker_id)
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
    let mut detail = detail.to_owned();
    let approval = detail.contains("APPROVAL_REQUIRED") || detail.contains("approval_required:");
    let mut recovery = detail.contains("PAYMENT_RECOVERY_REQUIRED")
        || detail.contains("payment_recovery_required")
        || detail.contains("PARTIAL_UPLOAD")
        || detail.contains("partial_upload:")
        // A retry may already have completed paid objects under the original
        // approval even when its next preflight fails before another upload.
        || (approval && job.attempts > 1
            && matches!(job.kind, JobKind::UploadVideo | JobKind::FinalizeCatalog));
    let final_failure = job.attempts >= job.max_attempts || recovery || approval;
    if final_failure {
        let mut tx = crate::db::begin_immediate(&state.pool).await?;
        let now = Utc::now();
        let updated = sqlx::query(
            r#"
            UPDATE video_jobs
            SET status=$1,
                lease_owner=NULL,
                lease_expires_at=NULL,
                last_error=$2,
                updated_at=$3
            WHERE id=$4 AND status='running' AND lease_owner=$5 AND attempts=$6 AND lease_expires_at > $3
            "#,
        )
        .bind(JOB_STATUS_FAILED)
        .bind(&detail)
        .bind(now)
        .bind(job.id)
        .bind(&job.lease_owner)
        .bind(job.attempts)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if updated.rows_affected() == 1 {
            if approval && matches!(job.kind, JobKind::UploadVideo | JobKind::FinalizeCatalog) {
                let quote_id: Option<String> = sqlx::query_scalar("SELECT COALESCE(j.payment_quote_id,v.approved_quote_id) FROM video_jobs j LEFT JOIN videos v ON v.id=j.video_id WHERE j.id=$1")
                    .bind(job.id).fetch_one(&mut *tx).await.map_err(db_error)?;
                if let Some(quote_id) = quote_id {
                    let lease_key = format!("{}:{}:{}", job.id, job.lease_owner, job.attempts);
                    let client = state.antd.with_lease(&lease_key);
                    // Hold job ownership while the gateway atomically proves no
                    // reservation exists and prevents any later signing on this ID.
                    let unpaid = tokio::time::timeout(
                        StdDuration::from_secs(2),
                        client.cancel_unpaid_approval(&quote_id),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false);
                    recovery = !unpaid;
                    if unpaid {
                        detail = format!("approval_required: original approval safely cancelled before any transaction; regenerate the quote. {detail}");
                        sqlx::query("UPDATE video_jobs SET last_error=$1 WHERE id=$2")
                            .bind(&detail)
                            .bind(job.id)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_error)?;
                    }
                }
            }
            if job.kind == JobKind::FinalizeCatalog {
                let status = if approval && !recovery {
                    "approval_required"
                } else {
                    "payment_recovery_required"
                };
                sqlx::query("UPDATE catalog_approvals SET state=$1,error_message=$2 WHERE id=(SELECT payment_quote_id FROM video_jobs WHERE id=$3)")
                    .bind(status).bind(&detail).bind(job.id).execute(&mut *tx).await.map_err(db_error)?;
            }
            if let Some(video_id) = job.video_id {
                let approved: bool = sqlx::query_scalar(
                    "SELECT approved_quote_id IS NOT NULL FROM videos WHERE id=$1",
                )
                .bind(video_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
                let status =
                    if recovery || (approved && !approval && job.kind == JobKind::UploadVideo) {
                        "payment_recovery_required"
                    } else if approval {
                        "approval_required"
                    } else {
                        STATUS_ERROR
                    };
                sqlx::query(
                    "UPDATE videos SET status=$1,error_message=$2,updated_at=$3 WHERE id=$4",
                )
                .bind(status)
                .bind(&detail)
                .bind(now)
                .bind(video_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            }
        }
        tx.commit().await.map_err(db_error)?;
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
            updated_at=$5
        WHERE id=$4 AND status='running' AND lease_owner=$6 AND attempts=$7 AND lease_expires_at > $5
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(run_after)
    .bind(&detail)
    .bind(job.id)
    .bind(Utc::now())
    .bind(&job.lease_owner)
    .bind(job.attempts)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    info!(
        "Retrying {:?} job {} in {}s after attempt {}/{}",
        job.kind, job.id, delay_seconds, job.attempts, job.max_attempts
    );
    Ok(())
}

/// `attempts` increases on every claim of this immutable job ID and fences old owners.
pub(super) async fn renew_lease(state: &AppState, job: &LeasedJob) -> Result<bool, ApiError> {
    let now = Utc::now();
    Ok(sqlx::query("UPDATE video_jobs SET lease_expires_at=$1, updated_at=$2 WHERE id=$3 AND status='running' AND lease_owner=$4 AND attempts=$5 AND lease_expires_at > $2")
        .bind(now + Duration::seconds(state.config.admin_job_lease_seconds)).bind(now)
        .bind(job.id).bind(&job.lease_owner).bind(job.attempts)
        .execute(&state.pool).await.map_err(db_error)?.rows_affected() == 1)
}

fn maintain_lease<'a>(
    state: &'a AppState,
    job: &'a LeasedJob,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApiError> + Send + 'a>> {
    Box::pin(async move {
        let interval = StdDuration::from_millis(
            (state.config.admin_job_lease_seconds.max(1) as u64 * 1000 / 3).max(100),
        );
        loop {
            sleep(interval).await;
            match renew_lease(state, job).await {
                Ok(true) => {
                    if let Err(error) = update_payment_lease(state, job).await {
                        return error;
                    }
                }
                Ok(false) => {
                    return ApiError::new(StatusCode::CONFLICT, "Job lease ownership lost")
                }
                Err(error) => return error,
            }
        }
    })
}

async fn update_payment_lease(
    state: &AppState,
    job: &LeasedJob,
) -> Result<Option<String>, ApiError> {
    if !matches!(job.kind, JobKind::UploadVideo | JobKind::FinalizeCatalog) {
        return Ok(None);
    }
    let row = sqlx::query("SELECT COALESCE(j.payment_quote_id,v.approved_quote_id) AS approved_quote_id,j.lease_expires_at FROM video_jobs j LEFT JOIN videos v ON v.id=j.video_id WHERE j.id=$1 AND j.status='running' AND j.lease_owner=$2 AND j.attempts=$3 AND j.lease_expires_at>$4")
        .bind(job.id).bind(&job.lease_owner).bind(job.attempts).bind(Utc::now()).fetch_optional(&state.pool).await.map_err(db_error)?
        .ok_or_else(||ApiError::new(StatusCode::CONFLICT,"Job lease ownership lost before payment activation"))?;
    let Some(quote_id) = row
        .try_get::<Option<String>, _>("approved_quote_id")
        .map_err(db_error)?
    else {
        return Ok(None);
    };
    let lease = autvid_common::payments::PaymentLease {
        quote_id,
        job_id: job.id.to_string(),
        owner: job.lease_owner.clone(),
        generation: job.attempts,
        expires_at: row
            .try_get::<DateTime<Utc>, _>("lease_expires_at")
            .map_err(db_error)?
            .timestamp(),
    };
    state
        .antd
        .activate_payment_lease(&lease)
        .await
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    Ok(Some(lease.key()))
}

async fn run_quote_video_job(state: &AppState, id: Option<Uuid>) -> Result<(), ApiError> {
    let id = id
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "Quote job has no video"))?
        .to_string();
    let mut quote = crate::pipeline::build_final_upload_quote(state, &id).await?;
    let expiry = DateTime::from_timestamp(
        quote["approval"]["expires_at"]
            .as_i64()
            .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "Quote expiry missing"))?,
        0,
    )
    .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "Invalid expiry"))?;
    quote["approval_expires_at"] = serde_json::json!(expiry.to_rfc3339());
    quote["quote_created_at"] = serde_json::json!(Utc::now().to_rfc3339());
    crate::db::set_awaiting_approval(state, &id, quote, expiry).await
}
