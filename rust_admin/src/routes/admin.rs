use std::fs;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::{
    auth::require_admin,
    catalog::{
        db_video_to_out, ensure_video_manifest_address, get_db_video, read_catalog_address,
        refresh_local_catalog_from_db,
    },
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    jobs::schedule_catalog_publish,
    models::{VideoOut, VideoPublicationUpdate, VideoVisibilityUpdate},
    state::AppState,
    STATUS_READY,
};

pub(super) async fn admin_list_videos(
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

pub(super) async fn admin_get_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

pub(super) async fn update_video_visibility(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoVisibilityUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let row = sqlx::query(
        r#"
        WITH previous AS (
            SELECT show_original_filename, show_manifest_address
            FROM videos
            WHERE id=$3
        ),
        updated AS (
            UPDATE videos
            SET show_original_filename=$1,
                show_manifest_address=$2,
                updated_at=CASE
                    WHEN show_original_filename IS DISTINCT FROM $1
                      OR show_manifest_address IS DISTINCT FROM $2
                    THEN NOW()
                    ELSE updated_at
                END
            WHERE id=$3
            RETURNING id, title, original_filename, description, status, created_at,
                      manifest_address, catalog_address, error_message, final_quote,
                      final_quote_created_at, approval_expires_at,
                      is_public, show_original_filename, show_manifest_address,
                      upload_original, original_file_address, original_file_byte_size,
                      publish_when_ready
        )
        SELECT updated.*,
               (previous.show_original_filename IS DISTINCT FROM $1
                OR previous.show_manifest_address IS DISTINCT FROM $2) AS visibility_changed
        FROM updated, previous
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
    let visibility_changed: bool = row.try_get("visibility_changed").unwrap_or(false);
    if visibility_changed && status == STATUS_READY && is_public {
        let epoch = refresh_local_catalog_from_db(&state, "visibility").await?;
        schedule_catalog_publish(&state, epoch, format!("visibility:{video_id}")).await?;
    }

    Ok(Json(db_video_to_out(&state, &row, true).await?))
}

pub(super) async fn update_video_publication(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoPublicationUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let row = sqlx::query("SELECT status, manifest_address, is_public FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let status: String = row.try_get("status").unwrap_or_default();
    let existing_manifest_address: Option<String> = row
        .try_get::<Option<String>, _>("manifest_address")
        .ok()
        .flatten();
    let was_public: bool = row.try_get("is_public").unwrap_or(false);
    if request.is_public {
        if status != STATUS_READY {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Only ready videos can be published",
            ));
        }
    }

    let manifest_address = if request.is_public {
        if let Some(address) = existing_manifest_address.clone() {
            Some(address)
        } else {
            Some(ensure_video_manifest_address(&state, &video_id).await?)
        }
    } else {
        None
    };

    let publication_changed = if request.is_public {
        !was_public || existing_manifest_address != manifest_address
    } else {
        was_public
    };

    let row = sqlx::query(
        r#"
        UPDATE videos
        SET is_public=$1,
            manifest_address=COALESCE($2, manifest_address),
            updated_at=CASE WHEN $3 THEN NOW() ELSE updated_at END
        WHERE id=$4
        RETURNING id, title, original_filename, description, status, created_at,
                  manifest_address, catalog_address, error_message, final_quote,
                  final_quote_created_at, approval_expires_at,
                  is_public, show_original_filename, show_manifest_address,
                  upload_original, original_file_address, original_file_byte_size,
                  publish_when_ready
        "#,
    )
    .bind(request.is_public)
    .bind(manifest_address.as_deref())
    .bind(publication_changed)
    .bind(video_uuid)
    .fetch_one(&state.pool)
    .await
    .map_err(db_error)?;

    let reason = if request.is_public {
        "publish"
    } else {
        "unpublish"
    };
    if publication_changed {
        let epoch = refresh_local_catalog_from_db(&state, reason).await?;
        schedule_catalog_publish(&state, epoch, format!("{reason}:{video_id}")).await?;
    }

    Ok(Json(db_video_to_out(&state, &row, true).await?))
}

pub(super) async fn delete_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let deleted =
        sqlx::query("DELETE FROM videos WHERE id=$1 RETURNING job_dir, status, is_public")
            .bind(video_uuid)
            .fetch_optional(&state.pool)
            .await
            .map_err(db_error)?;

    let deleted = deleted.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let status: String = deleted.try_get("status").unwrap_or_default();
    let was_public: bool = deleted.try_get("is_public").unwrap_or(false);
    if status == STATUS_READY && was_public {
        let epoch = refresh_local_catalog_from_db(&state, "delete").await?;
        schedule_catalog_publish(&state, epoch, format!("delete:{video_id}")).await?;
    }

    if let Ok(Some(job_dir)) = deleted.try_get::<Option<String>, _>("job_dir") {
        let _ = fs::remove_dir_all(job_dir);
    }
    let _ = fs::remove_dir_all(state.config.upload_temp_dir.join(&video_id));

    Ok(Json(json!({
        "deleted": video_id,
        "catalog_address": read_catalog_address(&state.config),
    })))
}
