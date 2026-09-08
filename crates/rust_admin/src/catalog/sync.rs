use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::Row;
use tracing::{error, info, instrument};

use super::{
    db_document::build_catalog_on_connection,
    state_file::{
        empty_catalog, read_all_catalog_address, read_catalog_address, read_catalog_snapshot,
        write_catalog_state,
    },
};
use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    state::AppState,
};

pub(crate) async fn load_catalog(state: &AppState) -> Result<(Value, Option<String>), ApiError> {
    if let Some(snapshot) = read_catalog_snapshot(&state.config) {
        return Ok(snapshot);
    }

    let Some(address) = read_catalog_address(&state.config) else {
        return Ok((empty_catalog(), None));
    };

    match load_json_from_autonomi(state, &address).await {
        Ok(mut catalog) => {
            if !catalog.get("videos").is_some_and(Value::is_array) {
                catalog["videos"] = json!([]);
            }
            Ok((catalog, Some(address)))
        }
        Err(err) => {
            error!("Could not load Autonomi catalog {}: {:?}", address, err);
            Ok((empty_catalog(), Some(address)))
        }
    }
}

pub(crate) async fn load_json_from_autonomi(
    state: &AppState,
    address: &str,
) -> Result<Value, ApiError> {
    let data = state
        .antd
        .data_get_public(address)
        .await
        .map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()))?;
    serde_json::from_slice(&data).map_err(|err| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("invalid JSON from Autonomi: {err}"),
        )
    })
}

pub(crate) async fn load_video_manifest_by_id(
    state: &AppState,
    video_id: &str,
) -> Result<Option<(Value, String)>, ApiError> {
    let (catalog, _) = load_catalog(state).await?;
    let Some(manifest_address) = catalog
        .get("videos")
        .and_then(Value::as_array)
        .and_then(|videos| {
            videos
                .iter()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(video_id))
        })
        .and_then(|entry| entry.get("manifest_address").and_then(Value::as_str))
    else {
        return Ok(None);
    };

    let manifest = load_json_from_autonomi(state, manifest_address).await?;
    Ok(Some((manifest, manifest_address.to_string())))
}

pub(crate) async fn ensure_video_manifest_address(
    state: &AppState,
    video_id: &str,
) -> Result<String, ApiError> {
    let existing_manifest_address = sqlx::query("SELECT manifest_address FROM videos WHERE id=$1")
        .bind(parse_video_uuid(video_id)?)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?
        .try_get::<Option<String>, _>("manifest_address")
        .ok()
        .flatten();

    if let Some(address) = existing_manifest_address {
        return Ok(address);
    }

    Err(ApiError::new(StatusCode::CONFLICT,
        "This legacy video has no stored manifest. Regenerate and approve its content quote before publication."))
}

#[instrument(skip(state), fields(reason = %reason))]
pub(crate) async fn refresh_local_catalog_from_db(
    state: &AppState,
    reason: &str,
) -> Result<u64, ApiError> {
    let _guard = state.catalog_lock.lock().await;
    let mut tx = crate::db::begin_immediate(&state.pool).await?;
    let all_catalog = build_catalog_on_connection(&mut tx).await?;
    let mut catalog = all_catalog.clone();
    catalog.catalog_kind = "published".into();
    catalog.videos.retain(|video| video.is_public);
    let video_count = catalog.videos.len();
    let all_video_count = all_catalog.videos.len();
    let epoch = state.catalog_publish_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let catalog_address = read_catalog_address(&state.config);
    let all_catalog_address = read_all_catalog_address(&state.config);
    sqlx::query("INSERT INTO application_state(key,value) VALUES('catalog_snapshot',$1) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(json!({"published_address":catalog_address,"all_address":all_catalog_address,"published":catalog,"all":all_catalog,"publish_pending":true}).to_string())
        .execute(&mut *tx).await.map_err(db_error)?;
    write_catalog_state(
        &state.config,
        catalog_address.as_deref(),
        all_catalog_address.as_deref(),
        Some(&catalog),
        Some(&all_catalog),
        true,
    )?;
    tx.commit().await.map_err(db_error)?;
    info!(
        "Queued local catalog update epoch={} reason={} published_videos={} all_videos={}",
        epoch, reason, video_count, all_video_count
    );
    Ok(epoch)
}

pub(crate) async fn publish_current_catalog_to_network(
    state: &AppState,
    _epoch: u64,
    _reason: &str,
) -> Result<(), ApiError> {
    super::payments::prepare(state).await
}
