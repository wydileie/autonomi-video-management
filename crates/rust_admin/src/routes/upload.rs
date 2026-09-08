use std::{fs, path::Path as FsPath};

use axum::{
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::Utc;
use serde_json::Value;
use sqlx::Row;

use crate::{
    auth::{require_admin, require_csrf},
    catalog::get_db_video,
    db::{begin_immediate, db_error, parse_video_uuid, set_status},
    errors::ApiError,
    jobs::{fetch_job_dir, schedule_processing_job, schedule_upload_job},
    models::{UploadQuoteOut, UploadQuoteRequest, VideoOut},
    quote::build_upload_quote,
    state::AppState,
    upload::accept_upload,
    STATUS_AWAITING_APPROVAL, STATUS_ERROR,
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApprovalRequest {
    quote_id: String,
    max_storage_atto: String,
    max_gas_wei: String,
}

pub(super) async fn approve_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ApprovalRequest>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    require_csrf(&headers)?;
    crate::jobs::cleanup_expired_approvals(&state)
        .await
        .map_err(db_error)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let mut tx = begin_immediate(&state.pool).await?;
    let row = sqlx::query(
        "SELECT status, final_quote, job_dir, approved_quote_id FROM videos WHERE id=$1",
    )
    .bind(video_uuid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let status: String = row.try_get("status").map_err(db_error)?;
    if status == crate::STATUS_EXPIRED {
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
    let quote: Value = row
        .try_get::<Option<Value>, _>("final_quote")
        .map_err(db_error)?
        .unwrap_or(Value::Null);
    let plan: crate::pipeline::UploadPlan =
        serde_json::from_value(quote["plan"].clone()).map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "Regenerate this legacy quote before approval",
            )
        })?;
    if request.quote_id != plan.approval.quote_id
        || request.max_storage_atto != plan.approval.max_storage_atto
        || request.max_gas_wei != plan.approval.max_gas_wei
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Quote changed. Review the current storage and gas caps before approval.",
        ));
    }
    plan.approval
        .validate(Utc::now().timestamp(), &plan.approval.network)
        .map_err(|e| ApiError::new(StatusCode::CONFLICT, e.to_string()))?;
    let job_dir: String = row.try_get("job_dir").map_err(db_error)?;
    if !FsPath::new(&job_dir).exists() {
        return Err(ApiError::new(
            StatusCode::GONE,
            "Transcoded files are no longer available",
        ));
    }
    state
        .antd
        .approve_payment(&plan.approval)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
    sqlx::query("UPDATE videos SET status='uploading', approved_quote_id=$1,error_message=NULL,updated_at=$2 WHERE id=$3")
        .bind(&request.quote_id).bind(Utc::now()).bind(video_uuid).execute(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    schedule_upload_job(&state, &video_id).await?;
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

pub(super) async fn requote_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    require_csrf(&headers)?;
    let uuid = parse_video_uuid(&video_id)?;
    let changed=sqlx::query("UPDATE videos SET status='quoting',approved_quote_id=NULL,error_message=NULL,updated_at=$1 WHERE id=$2 AND status IN ('approval_required','awaiting_approval')")
        .bind(Utc::now()).bind(uuid).execute(&state.pool).await.map_err(db_error)?.rows_affected();
    if changed != 1 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Only unpaid or revised approvals can be requoted; uncertain payments require recovery",
        ));
    }
    crate::jobs::schedule_quote_job(&state, &video_id).await?;
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

/// Reuse the failed job's immutable identity and original approval. The gateway
/// replays completed results or retained paid storage material; uncertainty stays paused.
pub(super) async fn resume_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    require_csrf(&headers)?;
    let uuid = parse_video_uuid(&video_id)?;
    let mut tx = begin_immediate(&state.pool).await?;
    let changed = sqlx::query("UPDATE videos SET status='uploading',error_message=NULL,updated_at=$1 WHERE id=$2 AND status='payment_recovery_required' AND approved_quote_id IS NOT NULL")
        .bind(Utc::now()).bind(uuid).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
    if changed != 1 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "This video has no paused approved upload",
        ));
    }
    let resumed = sqlx::query("UPDATE video_jobs SET status='queued',run_after=$1,lease_owner=NULL,lease_expires_at=NULL,last_error=NULL,max_attempts=MAX(max_attempts,attempts+1),updated_at=$1 WHERE id=(SELECT id FROM video_jobs WHERE video_id=$2 AND job_kind='upload_video' AND status='failed' ORDER BY created_at DESC,id DESC LIMIT 1)")
        .bind(Utc::now()).bind(uuid).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
    if resumed != 1 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "The original upload job is unavailable; reconcile its payment journal",
        ));
    }
    tx.commit().await.map_err(db_error)?;
    state
        .job_notify_tx
        .send_modify(|epoch| *epoch = epoch.saturating_add(1));
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}
