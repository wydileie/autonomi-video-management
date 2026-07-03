use std::path::PathBuf;

use axum::http::StatusCode;
use chrono::Utc;
use sqlx::Row;
use tokio::fs as tokio_fs;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    antd_client::is_missing_file_upload_endpoint, db::db_error, errors::ApiError,
    media::assert_under, models::ManifestOriginalFile, state::AppState,
    storage::put_public_verified_with_mode,
};

#[instrument(skip(state, video_row), fields(video_id = %video_id, video_uuid = %video_uuid))]
pub(crate) async fn upload_original_file_if_needed(
    state: &AppState,
    video_uuid: Uuid,
    video_id: &str,
    video_row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ManifestOriginalFile>, ApiError> {
    if let Some(address) = video_row
        .try_get::<Option<String>, _>("original_file_address")
        .ok()
        .flatten()
    {
        return Ok(Some(ManifestOriginalFile {
            autonomi_address: address,
            byte_size: video_row
                .try_get::<Option<i64>, _>("original_file_byte_size")
                .ok()
                .flatten(),
            autonomi_cost_atto: video_row
                .try_get::<Option<String>, _>("original_file_autonomi_cost_atto")
                .ok()
                .flatten(),
            payment_mode: video_row
                .try_get::<Option<String>, _>("original_file_autonomi_payment_mode")
                .ok()
                .flatten(),
        }));
    }

    let source_path = video_row
        .try_get::<Option<String>, _>("job_source_path")
        .ok()
        .flatten()
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Original source file is missing from disk",
            )
        })?;
    let source_path = assert_under(&source_path, &state.config.upload_temp_dir)?;
    if !source_path.exists() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Original source file is missing from disk: {}",
                source_path.display()
            ),
        ));
    }

    let metadata = tokio_fs::metadata(&source_path).await.map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not inspect original source file: {err}"),
        )
    })?;
    let byte_size = metadata.len() as i64;
    let filename = video_row
        .try_get::<String, _>("original_filename")
        .unwrap_or_else(|_| "source".to_string());
    let upload_label = format!("{video_id}/original/{filename}");
    let file_result = state
        .antd
        .file_put_public(
            &source_path,
            &state.config.antd_payment_mode,
            state.config.antd_upload_verify,
            state.config.antd_upload_retries,
        )
        .await;
    let (address, cost, payment_mode) = match file_result {
        Ok(result) => (
            result.address,
            Some(result.storage_cost_atto),
            result.payment_mode_used,
        ),
        Err(err) if is_missing_file_upload_endpoint(&err) => {
            if metadata.len() as usize > state.config.antd_direct_upload_max_bytes {
                return Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!(
                        "Autonomi file upload endpoint is unavailable and legacy JSON upload for original source {upload_label} would exceed ANTD_DIRECT_UPLOAD_MAX_BYTES ({})",
                        state.config.antd_direct_upload_max_bytes
                    ),
                ));
            }
            let data = tokio_fs::read(&source_path).await.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not read original source file: {err}"),
                )
            })?;
            let result = put_public_verified_with_mode(
                state,
                &data,
                &upload_label,
                &state.config.antd_payment_mode,
            )
            .await?;
            (
                result.address,
                result.cost,
                state.config.antd_payment_mode.clone(),
            )
        }
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Autonomi original source upload failed for {upload_label}: {err}"),
            ));
        }
    };
    sqlx::query(
        r#"
        UPDATE videos
        SET original_file_address=$1,
            original_file_byte_size=$2,
            original_file_autonomi_cost_atto=$3,
            original_file_autonomi_payment_mode=$4,
            updated_at=$5
        WHERE id=$6
        "#,
    )
    .bind(&address)
    .bind(byte_size)
    .bind(cost.as_deref())
    .bind(&payment_mode)
    .bind(Utc::now())
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    Ok(Some(ManifestOriginalFile {
        autonomi_address: address,
        byte_size: Some(byte_size),
        autonomi_cost_atto: cost,
        payment_mode: Some(payment_mode),
    }))
}
