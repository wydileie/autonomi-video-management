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
        apply_catalog_visibility, catalog_entry_to_video_out, load_catalog,
        load_json_from_autonomi, load_video_manifest_by_id, manifest_to_video_out,
    },
    db::db_error,
    errors::ApiError,
    models::VideoOut,
    state::AppState,
    STATUS_READY,
};
use uuid::Uuid;

pub(super) async fn get_catalog(
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

pub(super) async fn list_videos(
    State(state): State<AppState>,
) -> Result<Json<Vec<VideoOut>>, ApiError> {
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

pub(super) async fn get_video(
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

pub(super) async fn video_status(
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
