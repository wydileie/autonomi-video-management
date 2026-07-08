use std::time::Duration as StdDuration;

use axum::{
    extract::DefaultBodyLimit,
    http::{Request, Response, StatusCode},
    routing::{get, patch, post},
    Router,
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};

use crate::{
    auth::{auth_me, login, logout, refresh},
    config::{cors_layer, duration_from_secs_f64, Config},
    state::AppState,
};

mod admin;
mod health;
mod public;
mod upload;

pub(crate) fn router(config: &Config, state: AppState) -> anyhow::Result<Router> {
    let service_metrics = state.metrics.clone();
    let default_timeout = TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        duration_from_secs_f64(config.admin_request_timeout_seconds),
    );
    let upload_timeout = TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        duration_from_secs_f64(config.admin_upload_request_timeout_seconds),
    );
    Ok(Router::new()
        .route("/livez", get(health::livez))
        .route("/health", get(health::health))
        .route("/metrics", get(health::metrics))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(public::get_catalog))
        .route("/videos/upload/quote", post(upload::quote_video_upload))
        .route("/videos", get(public::list_videos))
        .route("/admin/catalogs", get(admin::admin_get_catalogs))
        .route(
            "/admin/catalogs/publish",
            post(admin::admin_publish_catalogs),
        )
        .route("/admin/videos", get(admin::admin_list_videos))
        .route(
            "/videos/{video_id}",
            get(public::get_video).delete(admin::delete_video),
        )
        .route(
            "/admin/videos/{video_id}",
            get(admin::admin_get_video).delete(admin::delete_video),
        )
        .route("/videos/{video_id}/status", get(public::video_status))
        .route("/videos/{video_id}/approve", post(upload::approve_video))
        .route(
            "/admin/videos/{video_id}/approve",
            post(upload::approve_video),
        )
        .route(
            "/admin/videos/{video_id}/visibility",
            patch(admin::update_video_visibility),
        )
        .route(
            "/admin/videos/{video_id}/publication",
            patch(admin::update_video_publication),
        )
        .route_layer(default_timeout)
        .route(
            "/videos/upload",
            post(upload::upload_video).layer(upload_timeout),
        )
        .layer(DefaultBodyLimit::disable())
        .layer(cors_layer(config)?)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("");
                    info_span!(
                        "http_request",
                        service = "rust_admin",
                        request_id = %request_id,
                        method = %request.method(),
                        uri = %request.uri(),
                        version = ?request.version(),
                    )
                })
                .on_response(
                    move |response: &Response<_>, latency: StdDuration, _span: &Span| {
                        service_metrics
                            .http
                            .record_request(response.status().as_u16(), latency);
                        info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "request completed"
                        );
                    },
                ),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state))
}

#[cfg(all(test, feature = "db-tests"))]
mod db_tests {
    #![allow(clippy::unwrap_used)]
    use std::{
        fs,
        net::SocketAddr,
        path::{Path, PathBuf},
        sync::{atomic::AtomicU64, Arc},
    };

    use axum::http::{HeaderValue, StatusCode};
    use chrono::{Duration, Utc};
    use serde_json::{json, Value};
    use sqlx::{
        sqlite::{SqliteConnectOptions, SqlitePoolOptions},
        Row, SqlitePool,
    };
    use tokio::{net::TcpListener, sync::Mutex, sync::Semaphore};
    use uuid::Uuid;

    use super::router;
    use crate::{
        antd_client::AntdRestClient, config::Config, db::ensure_schema, metrics::AdminMetrics,
        state::AppState, JOB_KIND_PUBLISH_CATALOG, JOB_STATUS_QUEUED, STATUS_AWAITING_APPROVAL,
        STATUS_EXPIRED, STATUS_READY,
    };

    struct TestDb {
        pool: SqlitePool,
        db_path: PathBuf,
    }

    impl TestDb {
        async fn new() -> Self {
            let db_path = std::env::temp_dir()
                .join(format!("autvid_test_{}.sqlite3", Uuid::new_v4().simple()));
            let connect_options = SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true)
                .foreign_keys(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(connect_options)
                .await
                .expect("connect test database");
            ensure_schema(&pool).await.expect("run migrations");
            Self { pool, db_path }
        }

        async fn cleanup(self) {
            self.pool.close().await;
            for path in [
                self.db_path.clone(),
                self.db_path.with_extension("sqlite3-wal"),
                self.db_path.with_extension("sqlite3-shm"),
            ] {
                let _ = fs::remove_file(path);
            }
        }
    }

    fn test_config(catalog_state_path: PathBuf, upload_temp_dir: PathBuf) -> Config {
        Config {
            db_path: catalog_state_path.with_file_name("autvid.sqlite3"),
            antd_url: "http://127.0.0.1:9".to_string(),
            antd_internal_token: None,
            antd_payment_mode: "auto".to_string(),
            antd_metadata_payment_mode: "merkle".to_string(),
            admin_username: "admin".to_string(),
            admin_password: "password".to_string(),
            admin_auth_secret: "secret".to_string(),
            admin_auth_ttl_hours: 12,
            admin_auth_cookie_secure: false,
            catalog_state_path,
            catalog_bootstrap_address: None,
            all_catalog_bootstrap_address: None,
            cors_allowed_origins: vec![HeaderValue::from_static("http://localhost")],
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            admin_db_min_connections: 1,
            admin_db_max_connections: 5,
            admin_db_connect_timeout_seconds: 5.0,
            admin_request_timeout_seconds: 120.0,
            admin_upload_request_timeout_seconds: 3600.0,
            admin_shutdown_grace_seconds: 1.0,
            upload_temp_dir,
            upload_max_file_bytes: 20 * 1024 * 1024,
            upload_min_free_bytes: 0,
            upload_max_concurrent_saves: 1,
            upload_read_idle_timeout_seconds: 30.0,
            upload_ffprobe_timeout_seconds: 30.0,
            ffmpeg_bin: "ffmpeg".into(),
            ffprobe_bin: "ffprobe".into(),
            hls_segment_duration: 1.0,
            ffmpeg_threads: 1,
            ffmpeg_filter_threads: 1,
            ffmpeg_max_parallel_renditions: 1,
            ffmpeg_rendition_timeout_seconds: 3600.0,
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

    fn test_state(pool: SqlitePool, root_dir: &Path) -> AppState {
        let metrics = Arc::new(AdminMetrics::default());
        AppState {
            config: Arc::new(test_config(
                root_dir.join("catalog.json"),
                root_dir.join("processing"),
            )),
            pool,
            antd: AntdRestClient::new("http://127.0.0.1:9", 1.0, metrics.clone(), None).unwrap(),
            metrics,
            catalog_lock: Arc::new(Mutex::new(())),
            catalog_publish_lock: Arc::new(Mutex::new(())),
            catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
            upload_save_semaphore: Arc::new(Semaphore::new(1)),
            shutdown: tokio_util::sync::CancellationToken::new(),
            job_notify_tx: tokio::sync::watch::channel(0).0,
        }
    }

    async fn spawn_admin(state: AppState) -> String {
        let config = state.config.clone();
        let app = router(&config, state).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn response_set_cookies(response: &reqwest::Response) -> Vec<String> {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect()
    }

    fn find_cookie_pair(set_cookies: &[String], name: &str) -> String {
        let prefix = format!("{name}=");
        set_cookies
            .iter()
            .find(|cookie| cookie.starts_with(&prefix))
            .and_then(|cookie| cookie.split(';').next())
            .expect("expected cookie")
            .to_string()
    }

    fn cookie_value(cookie_pair: &str) -> String {
        cookie_pair
            .split_once('=')
            .map(|(_, value)| value.to_string())
            .unwrap_or_default()
    }

    struct TestAuth {
        cookie_header: String,
        csrf_token: String,
    }

    async fn login(client: &reqwest::Client, base_url: &str) -> TestAuth {
        let response = client
            .post(format!("{base_url}/auth/login"))
            .json(&json!({ "username": "admin", "password": "password" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let cookies = response_set_cookies(&response);
        let auth_cookie = find_cookie_pair(&cookies, "autvid_admin");
        let csrf_cookie = find_cookie_pair(&cookies, "autvid_csrf");
        TestAuth {
            cookie_header: format!("{auth_cookie}; {csrf_cookie}"),
            csrf_token: cookie_value(&csrf_cookie),
        }
    }

    #[tokio::test]
    async fn db_auth_refresh_rotates_and_logout_revokes_refresh_session() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();

        let login_response = client
            .post(format!("{base_url}/auth/login"))
            .json(&json!({ "username": "admin", "password": "password" }))
            .send()
            .await
            .unwrap();
        assert_eq!(login_response.status().as_u16(), StatusCode::OK.as_u16());
        let login_cookies = response_set_cookies(&login_response);
        assert!(login_cookies.iter().any(|cookie| {
            cookie.starts_with("autvid_admin=")
                && cookie.contains("HttpOnly")
                && cookie.contains("SameSite=Lax")
        }));
        assert!(login_cookies.iter().any(|cookie| {
            cookie.starts_with("autvid_admin_refresh=")
                && cookie.contains("HttpOnly")
                && cookie.contains("Path=/api/auth")
        }));
        assert!(login_cookies.iter().any(|cookie| {
            cookie.starts_with("autvid_csrf=")
                && !cookie.contains("HttpOnly")
                && cookie.contains("Path=/")
        }));
        let auth_cookie = find_cookie_pair(&login_cookies, "autvid_admin");
        let csrf_cookie = find_cookie_pair(&login_cookies, "autvid_csrf");
        let refresh_cookie = find_cookie_pair(&login_cookies, "autvid_admin_refresh");
        let login_body: Value = login_response.json().await.unwrap();
        assert!(login_body["access_token"].is_null());
        assert!(login_body["token_type"].is_null());
        assert!(login_body["refresh_token_expires_at"].as_str().is_some());

        let me: Value = client
            .get(format!("{base_url}/auth/me"))
            .header(
                reqwest::header::COOKIE,
                format!("{auth_cookie}; {csrf_cookie}"),
            )
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["username"], "admin");

        let active_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > $1",
        )
        .bind(Utc::now())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(active_sessions, 1);

        let refresh_response = client
            .post(format!("{base_url}/auth/refresh"))
            .header(reqwest::header::COOKIE, &refresh_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(refresh_response.status().as_u16(), StatusCode::OK.as_u16());
        let refresh_cookies = response_set_cookies(&refresh_response);
        let rotated_refresh_cookie = find_cookie_pair(&refresh_cookies, "autvid_admin_refresh");
        let rotated_csrf_cookie = find_cookie_pair(&refresh_cookies, "autvid_csrf");
        let rotated_csrf_token = cookie_value(&rotated_csrf_cookie);
        assert_ne!(rotated_refresh_cookie, refresh_cookie);
        let refresh_body: Value = refresh_response.json().await.unwrap();
        assert!(refresh_body["token_type"].is_null());
        assert!(refresh_body["access_token"].is_null());

        let row = sqlx::query(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN revoked_at IS NULL THEN 1 ELSE 0 END), 0) AS active,
                COALESCE(SUM(CASE WHEN revoked_at IS NOT NULL THEN 1 ELSE 0 END), 0) AS revoked
            FROM admin_refresh_sessions
            "#,
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.try_get::<i64, _>("active").unwrap(), 1);
        assert_eq!(row.try_get::<i64, _>("revoked").unwrap(), 1);

        let reused_refresh = client
            .post(format!("{base_url}/auth/refresh"))
            .header(reqwest::header::COOKIE, &refresh_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(
            reused_refresh.status().as_u16(),
            StatusCode::UNAUTHORIZED.as_u16()
        );

        let logout_response = client
            .post(format!("{base_url}/auth/logout"))
            .header(
                reqwest::header::COOKIE,
                format!("{rotated_refresh_cookie}; {rotated_csrf_cookie}"),
            )
            .header("x-csrf-token", rotated_csrf_token)
            .send()
            .await
            .unwrap();
        assert_eq!(
            logout_response.status().as_u16(),
            StatusCode::NO_CONTENT.as_u16()
        );
        let logout_cookies = response_set_cookies(&logout_response);
        assert!(logout_cookies
            .iter()
            .any(|cookie| cookie.starts_with("autvid_admin=") && cookie.contains("Max-Age=0")));
        assert!(logout_cookies.iter().any(|cookie| {
            cookie.starts_with("autvid_admin_refresh=") && cookie.contains("Max-Age=0")
        }));
        assert!(logout_cookies
            .iter()
            .any(|cookie| cookie.starts_with("autvid_csrf=") && cookie.contains("Max-Age=0")));

        let active_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > $1",
        )
        .bind(Utc::now())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(active_sessions, 0);

        let after_logout_refresh = client
            .post(format!("{base_url}/auth/refresh"))
            .header(reqwest::header::COOKIE, &rotated_refresh_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(
            after_logout_refresh.status().as_u16(),
            StatusCode::UNAUTHORIZED.as_u16()
        );

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_auth_refresh_rejects_expired_session() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();

        let login_response = client
            .post(format!("{base_url}/auth/login"))
            .json(&json!({ "username": "admin", "password": "password" }))
            .send()
            .await
            .unwrap();
        assert_eq!(login_response.status().as_u16(), StatusCode::OK.as_u16());
        let refresh_cookie = find_cookie_pair(
            &response_set_cookies(&login_response),
            "autvid_admin_refresh",
        );
        sqlx::query("UPDATE admin_refresh_sessions SET expires_at=$1")
            .bind(Utc::now() - Duration::seconds(1))
            .execute(&state.pool)
            .await
            .unwrap();

        let refresh_response = client
            .post(format!("{base_url}/auth/refresh"))
            .header(reqwest::header::COOKIE, refresh_cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(
            refresh_response.status().as_u16(),
            StatusCode::UNAUTHORIZED.as_u16()
        );

        let active_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > $1",
        )
        .bind(Utc::now())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(active_sessions, 0);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    async fn insert_ready_video(pool: &SqlitePool, root_dir: &Path) -> Uuid {
        let video_id = Uuid::new_v4();
        let job_dir = root_dir.join(video_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        sqlx::query(
            r#"
            INSERT INTO videos
                (id, title, original_filename, status, manifest_address, job_dir)
            VALUES ($1, 'Ready DB Test', 'source.mp4', $2, 'manifest-address', $3)
            "#,
        )
        .bind(video_id)
        .bind(STATUS_READY)
        .bind(job_dir.to_string_lossy().as_ref())
        .execute(pool)
        .await
        .unwrap();
        video_id
    }

    async fn insert_expired_approval_video(pool: &SqlitePool, root_dir: &Path) -> (Uuid, PathBuf) {
        let video_id = Uuid::new_v4();
        let job_dir = root_dir.join(video_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        sqlx::query(
            r#"
            INSERT INTO videos
                (id, title, original_filename, status, job_dir, final_quote, approval_expires_at)
            VALUES ($1, 'Expired DB Test', 'source.mp4', $2, $3, $4, $5)
            "#,
        )
        .bind(video_id)
        .bind(STATUS_AWAITING_APPROVAL)
        .bind(job_dir.to_string_lossy().as_ref())
        .bind(json!({ "quote_type": "final" }))
        .bind(Utc::now() - Duration::seconds(10))
        .execute(pool)
        .await
        .unwrap();
        (video_id, job_dir)
    }

    async fn insert_approval_video_without_final_quote(pool: &SqlitePool, root_dir: &Path) -> Uuid {
        let video_id = Uuid::new_v4();
        let job_dir = root_dir.join(video_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        sqlx::query(
            r#"
            INSERT INTO videos
                (id, title, original_filename, status, job_dir, approval_expires_at)
            VALUES ($1, 'Missing Quote DB Test', 'source.mp4', $2, $3, $4)
            "#,
        )
        .bind(video_id)
        .bind(STATUS_AWAITING_APPROVAL)
        .bind(job_dir.to_string_lossy().as_ref())
        .bind(Utc::now() + Duration::seconds(600))
        .execute(pool)
        .await
        .unwrap();
        video_id
    }

    #[tokio::test]
    async fn db_approval_route_expires_old_final_quotes() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let (video_id, job_dir) = insert_expired_approval_video(&state.pool, &root_dir).await;
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = login(&client, &base_url).await;

        let response = client
            .post(format!("{base_url}/admin/videos/{video_id}/approve"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), StatusCode::GONE.as_u16());

        let row = sqlx::query("SELECT status, error_message FROM videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.try_get::<String, _>("status").unwrap(), STATUS_EXPIRED);
        assert!(row
            .try_get::<String, _>("error_message")
            .unwrap()
            .contains("approval window expired"));
        assert!(!job_dir.exists());

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_approval_route_rejects_missing_final_quote() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_approval_video_without_final_quote(&state.pool, &root_dir).await;
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = login(&client, &base_url).await;

        let response = client
            .post(format!("{base_url}/admin/videos/{video_id}/approve"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), StatusCode::CONFLICT.as_u16());

        let status: String = sqlx::query_scalar("SELECT status FROM videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, STATUS_AWAITING_APPROVAL);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_manual_catalog_publish_queues_job_without_network_publish() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        fs::create_dir_all(&root_dir).unwrap();
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_ready_video(&state.pool, &root_dir).await;
        sqlx::query("UPDATE videos SET is_public=TRUE WHERE id=$1")
            .bind(video_id)
            .execute(&state.pool)
            .await
            .unwrap();

        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = login(&client, &base_url).await;

        let body: Value = client
            .post(format!("{base_url}/admin/catalogs/publish"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            body["published_catalog"]["videos"][0]["id"],
            video_id.to_string()
        );
        assert_eq!(body["all_catalog"]["videos"][0]["id"], video_id.to_string());

        let publish_jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE job_kind=$1 AND status=$2")
                .bind(JOB_KIND_PUBLISH_CATALOG)
                .bind(JOB_STATUS_QUEUED)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(publish_jobs, 1);

        let catalog_state: Value =
            serde_json::from_str(&fs::read_to_string(&state.config.catalog_state_path).unwrap())
                .unwrap();
        assert_eq!(catalog_state["publish_pending"], true);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    #[tokio::test]
    async fn db_publication_and_delete_routes_update_catalog_and_jobs() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        fs::create_dir_all(&root_dir).unwrap();
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_ready_video(&state.pool, &root_dir).await;
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let auth = login(&client, &base_url).await;

        let published: Value = client
            .patch(format!("{base_url}/admin/videos/{video_id}/publication"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .json(&json!({ "is_public": true }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(published["is_public"], true);

        let public_videos: Value = client
            .get(format!("{base_url}/videos"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(public_videos[0]["id"], video_id.to_string());

        let catalog_state: Value =
            serde_json::from_str(&fs::read_to_string(&state.config.catalog_state_path).unwrap())
                .unwrap();
        assert_eq!(catalog_state["publish_pending"], true);
        assert_eq!(
            catalog_state["catalog"]["videos"][0]["id"],
            video_id.to_string()
        );

        let publish_jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE job_kind=$1 AND status=$2")
                .bind(JOB_KIND_PUBLISH_CATALOG)
                .bind(JOB_STATUS_QUEUED)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(publish_jobs, 1);

        let republished: Value = client
            .patch(format!("{base_url}/admin/videos/{video_id}/publication"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .json(&json!({ "is_public": true }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(republished["is_public"], true);

        let publish_jobs_after_noop: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM video_jobs WHERE job_kind=$1 AND status=$2")
                .bind(JOB_KIND_PUBLISH_CATALOG)
                .bind(JOB_STATUS_QUEUED)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(publish_jobs_after_noop, 1);

        let unpublished: Value = client
            .patch(format!("{base_url}/admin/videos/{video_id}/publication"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .json(&json!({ "is_public": false }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(unpublished["is_public"], false);

        let deleted: Value = client
            .delete(format!("{base_url}/admin/videos/{video_id}"))
            .header(reqwest::header::COOKIE, &auth.cookie_header)
            .header("x-csrf-token", &auth.csrf_token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(deleted["deleted"], video_id.to_string());

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }
}
