use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{errors::ApiError, state::AppState, STATUS_READY};

pub(crate) async fn ensure_schema(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

pub(crate) async fn set_status(
    state: &AppState,
    video_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status=$1, error_message=$2, updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(status)
    .bind(error_message)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn set_awaiting_approval(
    state: &AppState,
    video_id: &str,
    final_quote: Value,
    expires_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status='awaiting_approval',
            final_quote=$1::jsonb,
            final_quote_created_at=NOW(),
            approval_expires_at=$2,
            error_message=NULL,
            updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(final_quote)
    .bind(expires_at)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn set_ready(
    state: &AppState,
    video_id: &str,
    manifest_address: &str,
    catalog_address: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status='ready',
            manifest_address=$1,
            catalog_address=$2,
            is_public=FALSE,
            error_message=NULL,
            job_dir=NULL,
            job_source_path=NULL,
            approval_expires_at=NULL,
            updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(manifest_address)
    .bind(catalog_address)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn set_publication(
    state: &AppState,
    video_id: &str,
    is_public: bool,
    manifest_address: Option<&str>,
    catalog_address: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET is_public=$1,
            manifest_address=COALESCE($2, manifest_address),
            catalog_address=COALESCE($3, catalog_address),
            updated_at=NOW()
        WHERE id=$4
        "#,
    )
    .bind(is_public)
    .bind(manifest_address)
    .bind(catalog_address)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) async fn set_current_catalog_address(
    state: &AppState,
    catalog_address: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE videos
        SET catalog_address=$1
        WHERE status=$2
        "#,
    )
    .bind(catalog_address)
    .bind(STATUS_READY)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(crate) fn db_error(err: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

pub(crate) fn parse_video_uuid(video_id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(video_id).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))
}
