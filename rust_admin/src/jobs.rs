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

#[cfg(all(test, feature = "db-tests"))]
mod db_tests {
    use std::{
        fs,
        net::SocketAddr,
        path::PathBuf,
        sync::{atomic::AtomicU64, Arc},
    };

    use axum::http::HeaderValue;
    use serde_json::json;
    use sqlx::{postgres::PgPoolOptions, PgPool, Row};
    use tokio::sync::{Mutex, Semaphore};
    use uuid::Uuid;

    use super::{
        acquire_next_job, mark_job_failed, recover_interrupted_jobs, schedule_processing_job,
        schedule_upload_job,
    };
    use crate::{
        antd_client::AntdRestClient, config::Config, db::ensure_schema, metrics::AdminMetrics,
        state::AppState, JOB_KIND_PROCESS_VIDEO, JOB_KIND_PUBLISH_CATALOG, JOB_KIND_UPLOAD_VIDEO,
        JOB_STATUS_FAILED, JOB_STATUS_QUEUED, JOB_STATUS_RUNNING, STATUS_ERROR, STATUS_PENDING,
    };

    struct TestDb {
        pool: PgPool,
        maintenance_url: String,
        database_name: String,
    }

    impl TestDb {
        async fn new() -> Self {
            let maintenance_url = std::env::var("TEST_DATABASE_URL")
                .expect("TEST_DATABASE_URL must be set for db-tests");
            let database_name = format!("autvid_test_{}", Uuid::new_v4().simple());
            let test_url = database_url_for_name(&maintenance_url, &database_name);
            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&maintenance_url)
                .await
                .expect("connect maintenance database");
            sqlx::query(&format!(r#"CREATE DATABASE "{database_name}""#))
                .execute(&admin_pool)
                .await
                .expect("create test database");
            admin_pool.close().await;

            let pool = PgPoolOptions::new()
                .max_connections(5)
                .connect(&test_url)
                .await
                .expect("connect test database");
            ensure_schema(&pool).await.expect("run migrations");
            Self {
                pool,
                maintenance_url,
                database_name,
            }
        }

        async fn cleanup(self) {
            self.pool.close().await;
            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&self.maintenance_url)
                .await
                .expect("connect maintenance database for cleanup");
            let _ = sqlx::query(
                r#"
                SELECT pg_terminate_backend(pid)
                FROM pg_stat_activity
                WHERE datname=$1 AND pid <> pg_backend_pid()
                "#,
            )
            .bind(&self.database_name)
            .execute(&admin_pool)
            .await;
            let _ = sqlx::query(&format!(
                r#"DROP DATABASE IF EXISTS "{}""#,
                self.database_name
            ))
            .execute(&admin_pool)
            .await;
            admin_pool.close().await;
        }
    }

    fn database_url_for_name(base_url: &str, database_name: &str) -> String {
        let (without_query, query) = base_url
            .split_once('?')
            .map(|(base, query)| (base, Some(query)))
            .unwrap_or((base_url, None));
        let slash_index = without_query
            .rfind('/')
            .expect("database URL must include a database path");
        let prefix = &without_query[..slash_index + 1];
        match query {
            Some(query) => format!("{prefix}{database_name}?{query}"),
            None => format!("{prefix}{database_name}"),
        }
    }

    fn test_config(catalog_state_path: PathBuf, upload_temp_dir: PathBuf) -> Config {
        Config {
            db_dsn: "postgresql://example".to_string(),
            antd_url: "http://127.0.0.1:9".to_string(),
            antd_payment_mode: "auto".to_string(),
            antd_metadata_payment_mode: "merkle".to_string(),
            admin_username: "admin".to_string(),
            admin_password: "password".to_string(),
            admin_auth_secret: "secret".to_string(),
            admin_auth_ttl_hours: 12,
            admin_auth_cookie_secure: false,
            catalog_state_path,
            catalog_bootstrap_address: None,
            cors_allowed_origins: vec![HeaderValue::from_static("http://localhost")],
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            admin_request_timeout_seconds: 120.0,
            admin_upload_request_timeout_seconds: 3600.0,
            upload_temp_dir,
            upload_max_file_bytes: 20 * 1024 * 1024,
            upload_min_free_bytes: 0,
            upload_max_concurrent_saves: 1,
            upload_ffprobe_timeout_seconds: 30.0,
            hls_segment_duration: 1.0,
            ffmpeg_threads: 1,
            ffmpeg_filter_threads: 1,
            ffmpeg_max_parallel_renditions: 1,
            upload_max_duration_seconds: 3600.0,
            upload_max_source_pixels: 1920 * 1080,
            upload_max_source_long_edge: 1920,
            upload_quote_transcoded_overhead: 1.08,
            upload_quote_max_sample_bytes: 1024,
            final_quote_approval_ttl_seconds: 3600,
            approval_cleanup_interval_seconds: 300,
            antd_upload_verify: false,
            antd_upload_retries: 1,
            antd_upload_timeout_seconds: 30.0,
            antd_quote_concurrency: 1,
            antd_upload_concurrency: 1,
            antd_approve_on_startup: false,
            antd_require_cost_ready: false,
            antd_direct_upload_max_bytes: 1024,
            admin_job_workers: 1,
            admin_job_poll_interval_seconds: 1,
            admin_job_lease_seconds: 60,
            admin_job_max_attempts: 2,
            catalog_publish_job_max_attempts: 2,
        }
    }

    fn test_state(pool: PgPool, root_dir: &std::path::Path) -> AppState {
        let metrics = Arc::new(AdminMetrics::default());
        AppState {
            config: Arc::new(test_config(
                root_dir.join("catalog.json"),
                root_dir.join("processing"),
            )),
            pool,
            antd: AntdRestClient::new("http://127.0.0.1:9", 1.0, metrics.clone()).unwrap(),
            metrics,
            catalog_lock: Arc::new(Mutex::new(())),
            catalog_publish_lock: Arc::new(Mutex::new(())),
            catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
            upload_save_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    async fn insert_video(pool: &PgPool, status: &str, root_dir: &std::path::Path) -> Uuid {
        let video_id = Uuid::new_v4();
        let job_dir = root_dir.join(video_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        let source_path = job_dir.join("original_smoke.mp4");
        fs::write(&source_path, b"source").unwrap();
        sqlx::query(
            r#"
            INSERT INTO videos
                (id, title, original_filename, status, job_dir, job_source_path, requested_resolutions)
            VALUES ($1, 'DB Test', 'source.mp4', $2, $3, $4, $5::jsonb)
            "#,
        )
        .bind(video_id)
        .bind(status)
        .bind(job_dir.to_string_lossy().as_ref())
        .bind(source_path.to_string_lossy().as_ref())
        .bind(json!(["360p"]))
        .execute(pool)
        .await
        .unwrap();
        video_id
    }

    #[tokio::test]
    async fn db_job_queue_dedupes_and_leases_without_double_claiming() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_jobs_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_video(&state.pool, STATUS_PENDING, &root_dir).await;
        let video_id_text = video_id.to_string();

        schedule_processing_job(&state, &video_id_text)
            .await
            .unwrap();
        schedule_processing_job(&state, &video_id_text)
            .await
            .unwrap();
        schedule_upload_job(&state, &video_id_text).await.unwrap();
        schedule_upload_job(&state, &video_id_text).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE video_id=$1")
            .bind(video_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 2);

        let first = acquire_next_job(&state, "worker-a").await.unwrap().unwrap();
        let second = acquire_next_job(&state, "worker-b").await.unwrap().unwrap();
        let third = acquire_next_job(&state, "worker-c").await.unwrap();
        assert_ne!(first.id, second.id);
        assert!(third.is_none());

        let running_rows = sqlx::query(
            r#"
            SELECT job_kind, lease_owner, attempts
            FROM video_jobs
            WHERE status=$1
            ORDER BY job_kind
            "#,
        )
        .bind(JOB_STATUS_RUNNING)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        let owners = running_rows
            .iter()
            .map(|row| row.try_get::<String, _>("lease_owner").unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        let kinds = running_rows
            .iter()
            .map(|row| row.try_get::<String, _>("job_kind").unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            owners,
            ["worker-a".to_string(), "worker-b".to_string()].into()
        );
        assert_eq!(
            kinds,
            [
                JOB_KIND_PROCESS_VIDEO.to_string(),
                JOB_KIND_UPLOAD_VIDEO.to_string()
            ]
            .into()
        );
        assert!(running_rows
            .iter()
            .all(|row| row.try_get::<i32, _>("attempts").unwrap() == 1));

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_expired_lease_can_be_reclaimed() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_jobs_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_video(&state.pool, STATUS_PENDING, &root_dir).await;

        schedule_processing_job(&state, &video_id.to_string())
            .await
            .unwrap();
        let first = acquire_next_job(&state, "worker-a").await.unwrap().unwrap();
        sqlx::query(
            r#"
            UPDATE video_jobs
            SET lease_expires_at=NOW() - INTERVAL '1 second'
            WHERE id=$1
            "#,
        )
        .bind(first.id)
        .execute(&state.pool)
        .await
        .unwrap();

        let reclaimed = acquire_next_job(&state, "worker-b").await.unwrap().unwrap();
        assert_eq!(reclaimed.id, first.id);
        assert_eq!(reclaimed.attempts, 2);

        let owner: String = sqlx::query_scalar("SELECT lease_owner FROM video_jobs WHERE id=$1")
            .bind(first.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(owner, "worker-b");

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_failed_jobs_retry_then_mark_video_terminal() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_jobs_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_video(&state.pool, STATUS_PENDING, &root_dir).await;

        schedule_processing_job(&state, &video_id.to_string())
            .await
            .unwrap();
        let first = acquire_next_job(&state, "worker-a").await.unwrap().unwrap();
        assert_eq!(first.max_attempts, 2);
        mark_job_failed(&state, &first, "temporary failure")
            .await
            .unwrap();

        let retry =
            sqlx::query("SELECT status, last_error, lease_owner FROM video_jobs WHERE id=$1")
                .bind(first.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            retry.try_get::<String, _>("status").unwrap(),
            JOB_STATUS_QUEUED
        );
        assert_eq!(
            retry.try_get::<String, _>("last_error").unwrap(),
            "temporary failure"
        );
        assert!(retry
            .try_get::<Option<String>, _>("lease_owner")
            .unwrap()
            .is_none());

        sqlx::query("UPDATE video_jobs SET run_after=NOW() WHERE id=$1")
            .bind(first.id)
            .execute(&state.pool)
            .await
            .unwrap();
        let second = acquire_next_job(&state, "worker-b").await.unwrap().unwrap();
        assert_eq!(second.attempts, 2);
        mark_job_failed(&state, &second, "permanent failure")
            .await
            .unwrap();

        let final_status: String = sqlx::query_scalar("SELECT status FROM video_jobs WHERE id=$1")
            .bind(first.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(final_status, JOB_STATUS_FAILED);
        let video = sqlx::query("SELECT status, error_message FROM videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(video.try_get::<String, _>("status").unwrap(), STATUS_ERROR);
        assert_eq!(
            video.try_get::<String, _>("error_message").unwrap(),
            "permanent failure"
        );

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_recovery_requeues_running_jobs_and_pending_catalog_publish() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_jobs_{}", Uuid::new_v4()));
        fs::create_dir_all(&root_dir).unwrap();
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_video(&state.pool, STATUS_PENDING, &root_dir).await;
        fs::write(
            &state.config.catalog_state_path,
            serde_json::to_string(&json!({
                "catalog_address": "",
                "publish_pending": true,
                "catalog": { "videos": [] }
            }))
            .unwrap(),
        )
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO video_jobs (job_kind, status, attempts, max_attempts, lease_owner, lease_expires_at)
            VALUES ($1, $2, 1, 2, 'old-worker', NOW() + INTERVAL '1 hour')
            "#,
        )
        .bind(JOB_KIND_PUBLISH_CATALOG)
        .bind(JOB_STATUS_RUNNING)
        .execute(&state.pool)
        .await
        .unwrap();

        recover_interrupted_jobs(state.clone()).await.unwrap();

        let running_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE status=$1")
                .bind(JOB_STATUS_RUNNING)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(running_count, 0);
        let process_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM video_jobs WHERE job_kind=$1 AND video_id=$2 AND status=$3",
        )
        .bind(JOB_KIND_PROCESS_VIDEO)
        .bind(video_id)
        .bind(JOB_STATUS_QUEUED)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(process_count, 1);
        let catalog_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE job_kind=$1 AND status=$2")
                .bind(JOB_KIND_PUBLISH_CATALOG)
                .bind(JOB_STATUS_QUEUED)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(catalog_count, 1);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }
}
