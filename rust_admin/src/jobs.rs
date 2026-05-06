mod cleanup;
mod lease_worker;
mod recovery;
mod scheduling;

pub(crate) use cleanup::{approval_cleanup_loop, cleanup_expired_approvals};
pub(crate) use lease_worker::start_job_workers;
pub(crate) use recovery::recover_interrupted_jobs;
pub(crate) use scheduling::{
    fetch_job_dir, schedule_catalog_publish, schedule_processing_job, schedule_upload_job,
};

#[cfg(all(test, feature = "db-tests"))]
use lease_worker::{acquire_next_job, mark_job_failed};
#[cfg(test)]
use recovery::{decode_requested_resolutions, recover_source_path};
#[cfg(test)]
use scheduling::job_retry_delay_seconds;

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
