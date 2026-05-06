use std::{fs, path::Path as FsPath, time::Duration as StdDuration};

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, HeaderMap, Request, Response, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::Row;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};
use uuid::Uuid;

use crate::{
    auth::{auth_me, login, logout, refresh, require_admin},
    catalog::{
        apply_catalog_visibility, catalog_entry_to_video_out, db_video_to_out,
        ensure_video_manifest_address, get_db_video, load_catalog, load_json_from_autonomi,
        load_video_manifest_by_id, manifest_to_video_out, read_catalog_address,
        refresh_local_catalog_from_db,
    },
    config::{cors_layer, duration_from_secs_f64, Config},
    db::{db_error, parse_video_uuid, set_publication, set_status},
    errors::ApiError,
    jobs::{
        cleanup_expired_approvals, fetch_job_dir, schedule_catalog_publish,
        schedule_processing_job, schedule_upload_job,
    },
    models::{
        AutonomiHealth, HealthResponse, PostgresHealth, UploadQuoteOut, UploadQuoteRequest,
        VideoOut, VideoPublicationUpdate, VideoVisibilityUpdate,
    },
    quote::build_upload_quote,
    state::AppState,
    upload::accept_upload,
    MIN_ANTD_SELF_ENCRYPTION_BYTES, STATUS_AWAITING_APPROVAL, STATUS_ERROR, STATUS_EXPIRED,
    STATUS_READY,
};

pub(crate) fn router(config: &Config, state: AppState) -> anyhow::Result<Router> {
    let service_metrics = state.metrics.clone();
    let default_timeout =
        TimeoutLayer::new(duration_from_secs_f64(config.admin_request_timeout_seconds));
    let upload_timeout = TimeoutLayer::new(duration_from_secs_f64(
        config.admin_upload_request_timeout_seconds,
    ));
    Ok(Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(get_catalog))
        .route("/videos/upload/quote", post(quote_video_upload))
        .route("/videos", get(list_videos))
        .route("/admin/videos", get(admin_list_videos))
        .route("/videos/:video_id", get(get_video).delete(delete_video))
        .route(
            "/admin/videos/:video_id",
            get(admin_get_video).delete(delete_video),
        )
        .route("/videos/:video_id/status", get(video_status))
        .route("/videos/:video_id/approve", post(approve_video))
        .route("/admin/videos/:video_id/approve", post(approve_video))
        .route(
            "/admin/videos/:video_id/visibility",
            patch(update_video_visibility),
        )
        .route(
            "/admin/videos/:video_id/publication",
            patch(update_video_publication),
        )
        .route_layer(default_timeout)
        .route("/videos/upload", post(upload_video).layer(upload_timeout))
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

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus(),
    )
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let autonomi = match state.antd.health().await {
        Ok(status) => AutonomiHealth {
            ok: status.status.eq_ignore_ascii_case("ok"),
            network: status.network,
            error: None,
        },
        Err(err) => AutonomiHealth {
            ok: false,
            network: None,
            error: Some(err.to_string()),
        },
    };
    let postgres = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => PostgresHealth {
            ok: true,
            error: None,
        },
        Err(err) => PostgresHealth {
            ok: false,
            error: Some(err.to_string()),
        },
    };
    let write_ready = if state.config.antd_require_cost_ready {
        state
            .antd
            .data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await
            .is_ok()
    } else {
        autonomi.ok
    };
    let ok = autonomi.ok && postgres.ok && write_ready;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(HealthResponse {
            ok,
            autonomi,
            postgres,
            write_ready,
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            role: "primary_admin",
        }),
    )
        .into_response()
}

async fn get_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers)?;
    let (catalog, catalog_address) = load_catalog(&state).await?;
    Ok(Json(json!({
        "catalog_address": catalog_address,
        "catalog": catalog,
    })))
}

async fn quote_video_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UploadQuoteRequest>,
) -> Result<Json<UploadQuoteOut>, ApiError> {
    require_admin(&state, &headers)?;
    build_upload_quote(&state, request).await.map(Json)
}

async fn list_videos(State(state): State<AppState>) -> Result<Json<Vec<VideoOut>>, ApiError> {
    let (catalog, catalog_address) = load_catalog(&state).await?;
    let videos = catalog
        .get("videos")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|entry| {
            entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(STATUS_READY)
                == STATUS_READY
        })
        .map(|entry| catalog_entry_to_video_out(entry, catalog_address.as_deref()))
        .collect();
    Ok(Json(videos))
}

async fn admin_list_videos(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<VideoOut>>, ApiError> {
    require_admin(&state, &headers)?;
    let rows = sqlx::query(
        r#"
        SELECT id, title, original_filename, description, status, created_at,
               manifest_address, catalog_address, error_message, final_quote,
               final_quote_created_at, approval_expires_at,
               is_public, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               publish_when_ready
        FROM videos
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut videos = Vec::with_capacity(rows.len());
    for row in rows {
        videos.push(db_video_to_out(&state, &row, false).await?);
    }
    Ok(Json(videos))
}

async fn get_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
) -> Result<Json<VideoOut>, ApiError> {
    let (catalog, _) = load_catalog(&state).await?;
    let entry = catalog
        .get("videos")
        .and_then(Value::as_array)
        .and_then(|videos| {
            videos
                .iter()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(video_id.as_str()))
        })
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let manifest_address = entry
        .get("manifest_address")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let manifest = load_json_from_autonomi(&state, manifest_address).await?;
    let mut video = manifest_to_video_out(&state, &manifest, Some(manifest_address), true);
    apply_catalog_visibility(&mut video, entry, &manifest, manifest_address);
    Ok(Json(video))
}

async fn admin_get_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn video_status(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let video_uuid = Uuid::parse_str(&video_id).ok();
    let row = sqlx::query(
        r#"
        SELECT status, manifest_address, catalog_address, error_message,
               show_manifest_address
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    if let Some(row) = row {
        let show_manifest_address = row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false);
        let manifest_address = if show_manifest_address {
            row.try_get::<Option<String>, _>("manifest_address")
                .ok()
                .flatten()
        } else {
            None
        };
        return Ok(Json(json!({
            "video_id": video_id,
            "status": row.try_get::<String, _>("status").unwrap_or_default(),
            "manifest_address": manifest_address,
            "catalog_address": null,
            "error_message": row.try_get::<Option<String>, _>("error_message").ok().flatten(),
        })));
    }

    let loaded = load_video_manifest_by_id(&state, &video_id).await?;
    let (manifest, manifest_address) =
        loaded.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let show_manifest_address = manifest
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Json(json!({
        "video_id": video_id,
        "status": STATUS_READY,
        "manifest_address": if show_manifest_address { Some(manifest_address) } else { None },
        "catalog_address": null,
    })))
}

async fn update_video_visibility(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoVisibilityUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let row = sqlx::query(
        r#"
        UPDATE videos
        SET show_original_filename=$1,
            show_manifest_address=$2,
            updated_at=NOW()
        WHERE id=$3
        RETURNING status, is_public
        "#,
    )
    .bind(false)
    .bind(request.show_manifest_address)
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let row = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let status: String = row.try_get("status").unwrap_or_default();
    let is_public: bool = row.try_get("is_public").unwrap_or(false);
    if status == STATUS_READY && is_public {
        let epoch = refresh_local_catalog_from_db(&state, "visibility").await?;
        schedule_catalog_publish(&state, epoch, format!("visibility:{video_id}")).await?;
    }

    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn upload_video(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<VideoOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    let accepted = accept_upload(&state, &headers, multipart, &username).await?;
    if let Err(err) = schedule_processing_job(&state, &accepted.video_id).await {
        let _ = set_status(&state, &accepted.video_id, STATUS_ERROR, Some(&err.detail)).await;
        if let Ok(Some(job_dir)) = fetch_job_dir(&state, &accepted.video_id).await {
            let _ = fs::remove_dir_all(job_dir);
        }
        return Err(err);
    }
    Ok(Json(accepted.video))
}

async fn approve_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    cleanup_expired_approvals(&state).await.map_err(db_error)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let mut expired_job_dir = None;
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let row = sqlx::query(
        r#"
        SELECT status, approval_expires_at, job_dir
        FROM videos
        WHERE id=$1
        FOR UPDATE
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let status: String = row.try_get("status").unwrap_or_default();
    if status == STATUS_EXPIRED {
        return Err(ApiError::new(
            StatusCode::GONE,
            "Final quote approval window has expired",
        ));
    }
    if status != STATUS_AWAITING_APPROVAL {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("Video is {status}, not awaiting approval"),
        ));
    }

    let job_dir: Option<String> = row.try_get("job_dir").ok().flatten();
    let approval_expires_at: Option<DateTime<Utc>> =
        row.try_get("approval_expires_at").ok().flatten();
    if approval_expires_at.is_some_and(|expires_at| expires_at <= Utc::now()) {
        expired_job_dir = job_dir.clone();
        sqlx::query(
            r#"
            UPDATE videos
            SET status='expired',
                error_message='Final quote approval window expired; local files were deleted.',
                updated_at=NOW()
            WHERE id=$1
            "#,
        )
        .bind(video_uuid)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    } else if job_dir
        .as_deref()
        .map(|path| !FsPath::new(path).exists())
        .unwrap_or(true)
    {
        return Err(ApiError::new(
            StatusCode::GONE,
            "Transcoded files are no longer available",
        ));
    } else {
        sqlx::query(
            r#"
            UPDATE videos
            SET status='uploading', error_message=NULL, updated_at=NOW()
            WHERE id=$1
            "#,
        )
        .bind(video_uuid)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;

    if let Some(path) = expired_job_dir {
        let _ = fs::remove_dir_all(path);
        return Err(ApiError::new(
            StatusCode::GONE,
            "Final quote approval window has expired",
        ));
    }

    if let Err(err) = schedule_upload_job(&state, &video_id).await {
        let _ = set_status(&state, &video_id, STATUS_ERROR, Some(&err.detail)).await;
        return Err(err);
    }
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn update_video_publication(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoPublicationUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let row = sqlx::query("SELECT status, manifest_address FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    if request.is_public {
        let status: String = row.try_get("status").unwrap_or_default();
        if status != STATUS_READY {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Only ready videos can be published",
            ));
        }
        let manifest_address = if let Some(address) = row
            .try_get::<Option<String>, _>("manifest_address")
            .ok()
            .flatten()
        {
            address
        } else {
            ensure_video_manifest_address(&state, &video_id).await?
        };
        set_publication(&state, &video_id, true, Some(&manifest_address), None).await?;
    } else {
        set_publication(&state, &video_id, false, None, None).await?;
    }

    let reason = if request.is_public {
        "publish"
    } else {
        "unpublish"
    };
    let epoch = refresh_local_catalog_from_db(&state, reason).await?;
    schedule_catalog_publish(&state, epoch, format!("{reason}:{video_id}")).await?;

    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn delete_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let job_dir_row = sqlx::query("SELECT job_dir FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    let result = sqlx::query("DELETE FROM videos WHERE id=$1")
        .bind(video_uuid)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;

    if result.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Video not found"));
    }

    let epoch = refresh_local_catalog_from_db(&state, "delete").await?;
    schedule_catalog_publish(&state, epoch, format!("delete:{video_id}")).await?;

    if let Some(row) = job_dir_row {
        if let Ok(Some(job_dir)) = row.try_get::<Option<String>, _>("job_dir") {
            let _ = fs::remove_dir_all(job_dir);
        }
    }
    let _ = fs::remove_dir_all(state.config.upload_temp_dir.join(&video_id));

    Ok(Json(json!({
        "deleted": video_id,
        "catalog_address": read_catalog_address(&state.config),
    })))
}

#[cfg(all(test, feature = "db-tests"))]
mod db_tests {
    use std::{
        fs,
        net::SocketAddr,
        path::{Path, PathBuf},
        sync::{atomic::AtomicU64, Arc},
    };

    use axum::http::{HeaderValue, StatusCode};
    use chrono::{Duration, Utc};
    use serde_json::{json, Value};
    use sqlx::{postgres::PgPoolOptions, PgPool, Row};
    use tokio::{net::TcpListener, sync::Mutex, sync::Semaphore};
    use uuid::Uuid;

    use super::router;
    use crate::{
        antd_client::AntdRestClient, config::Config, db::ensure_schema, metrics::AdminMetrics,
        state::AppState, JOB_KIND_PUBLISH_CATALOG, JOB_STATUS_QUEUED, STATUS_AWAITING_APPROVAL,
        STATUS_EXPIRED, STATUS_READY,
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

    fn test_state(pool: PgPool, root_dir: &Path) -> AppState {
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

    async fn login(client: &reqwest::Client, base_url: &str) -> String {
        let response: Value = client
            .post(format!("{base_url}/auth/login"))
            .json(&json!({ "username": "admin", "password": "password" }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        response["access_token"].as_str().unwrap().to_string()
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
        let refresh_cookie = find_cookie_pair(&login_cookies, "autvid_admin_refresh");
        let login_body: Value = login_response.json().await.unwrap();
        let access_token = login_body["access_token"].as_str().unwrap();
        assert_eq!(login_body["token_type"], "bearer");
        assert!(login_body["refresh_token_expires_at"].as_str().is_some());

        let me: Value = client
            .get(format!("{base_url}/auth/me"))
            .bearer_auth(access_token)
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
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > NOW()",
        )
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
        assert_ne!(rotated_refresh_cookie, refresh_cookie);
        let refresh_body: Value = refresh_response.json().await.unwrap();
        assert_eq!(refresh_body["token_type"], "bearer");
        assert!(refresh_body["access_token"].as_str().is_some());

        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE revoked_at IS NULL) AS active,
                COUNT(*) FILTER (WHERE revoked_at IS NOT NULL) AS revoked
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
            .header(reqwest::header::COOKIE, &rotated_refresh_cookie)
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

        let active_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > NOW()",
        )
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
        sqlx::query("UPDATE admin_refresh_sessions SET expires_at=NOW() - INTERVAL '1 second'")
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
            "SELECT COUNT(*) FROM admin_refresh_sessions WHERE revoked_at IS NULL AND expires_at > NOW()",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(active_sessions, 0);

        let _ = fs::remove_dir_all(root_dir);
        db.cleanup().await;
    }

    async fn insert_ready_video(pool: &PgPool, root_dir: &Path) -> Uuid {
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

    async fn insert_expired_approval_video(pool: &PgPool, root_dir: &Path) -> (Uuid, PathBuf) {
        let video_id = Uuid::new_v4();
        let job_dir = root_dir.join(video_id.to_string());
        fs::create_dir_all(&job_dir).unwrap();
        sqlx::query(
            r#"
            INSERT INTO videos
                (id, title, original_filename, status, job_dir, final_quote, approval_expires_at)
            VALUES ($1, 'Expired DB Test', 'source.mp4', $2, $3, $4::jsonb, $5)
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

    #[tokio::test]
    async fn db_approval_route_expires_old_final_quotes() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        let state = test_state(db.pool.clone(), &root_dir);
        let (video_id, job_dir) = insert_expired_approval_video(&state.pool, &root_dir).await;
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let token = login(&client, &base_url).await;

        let response = client
            .post(format!("{base_url}/admin/videos/{video_id}/approve"))
            .bearer_auth(token)
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
    async fn db_publication_and_delete_routes_update_catalog_and_jobs() {
        let db = TestDb::new().await;
        let root_dir = std::env::temp_dir().join(format!("autvid_db_routes_{}", Uuid::new_v4()));
        fs::create_dir_all(&root_dir).unwrap();
        let state = test_state(db.pool.clone(), &root_dir);
        let video_id = insert_ready_video(&state.pool, &root_dir).await;
        let base_url = spawn_admin(state.clone()).await;
        let client = reqwest::Client::new();
        let token = login(&client, &base_url).await;

        let published: Value = client
            .patch(format!("{base_url}/admin/videos/{video_id}/publication"))
            .bearer_auth(&token)
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

        let unpublished: Value = client
            .patch(format!("{base_url}/admin/videos/{video_id}/publication"))
            .bearer_auth(&token)
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
            .bearer_auth(&token)
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
