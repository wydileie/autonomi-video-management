use std::{fs, path::Path as FsPath};

use axum::{
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;

use crate::{
    auth::{require_admin, require_csrf},
    catalog::get_db_video,
    db::{db_error, parse_video_uuid, set_status},
    errors::ApiError,
    jobs::{
        cleanup_expired_approvals, fetch_job_dir, schedule_processing_job, schedule_upload_job,
    },
    models::{UploadQuoteOut, UploadQuoteRequest, VideoOut},
    quote::build_upload_quote,
    state::AppState,
    upload::accept_upload,
    STATUS_AWAITING_APPROVAL, STATUS_ERROR, STATUS_EXPIRED,
};

pub(super) async fn quote_video_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UploadQuoteRequest>,
) -> Result<Json<UploadQuoteOut>, ApiError> {
    require_admin(&state, &headers)?;
    require_csrf(&headers)?;
    build_upload_quote(&state, request).await.map(Json)
}

pub(super) async fn upload_video(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<VideoOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    require_csrf(&headers)?;
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

pub(super) async fn approve_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    require_csrf(&headers)?;
    cleanup_expired_approvals(&state).await.map_err(db_error)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let mut expired_job_dir = None;
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let row = sqlx::query(
        r#"
        SELECT status, approval_expires_at, job_dir, final_quote
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
    let final_quote: Option<Value> = row.try_get("final_quote").ok().flatten();
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
    } else if final_quote.is_none() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Final quote is missing; wait for processing to finish",
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
