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
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};
use uuid::Uuid;

use crate::{
    auth::{auth_me, login, logout, require_admin},
    catalog::{
        apply_catalog_visibility, catalog_entry_to_video_out, db_video_to_out,
        ensure_video_manifest_address, get_db_video, load_catalog, load_json_from_autonomi,
        load_video_manifest_by_id, manifest_to_video_out, read_catalog_address,
        refresh_local_catalog_from_db,
    },
    config::{cors_layer, Config},
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
    MIN_ANTD_SELF_ENCRYPTION_BYTES, STATUS_AWAITING_APPROVAL, STATUS_ERROR, STATUS_READY,
};

pub(crate) fn router(config: &Config, state: AppState) -> anyhow::Result<Router> {
    let service_metrics = state.metrics.clone();
    Ok(Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(get_catalog))
        .route("/videos/upload/quote", post(quote_video_upload))
        .route("/videos/upload", post(upload_video))
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
        .unwrap_or(&Vec::new())
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
