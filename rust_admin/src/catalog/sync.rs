use std::{
    sync::atomic::Ordering,
    time::{Duration as StdDuration, Instant},
};

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::time::sleep;
use tracing::{error, info, instrument};

use super::{
    db_document::{build_public_catalog_from_db, build_ready_manifest_from_db},
    state_file::{empty_catalog, read_catalog_address, read_catalog_snapshot, write_catalog_state},
};
use crate::{
    db::{db_error, parse_video_uuid, set_current_catalog_address},
    errors::ApiError,
    state::AppState,
    storage::store_json_public,
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

    let manifest = build_ready_manifest_from_db(state, video_id).await?;
    let manifest_address = store_json_public(state, &manifest).await?;
    sqlx::query("UPDATE videos SET manifest_address=$1, updated_at=NOW() WHERE id=$2")
        .bind(&manifest_address)
        .bind(parse_video_uuid(video_id)?)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;
    Ok(manifest_address)
}

#[instrument(skip(state), fields(reason = %reason))]
pub(crate) async fn refresh_local_catalog_from_db(
    state: &AppState,
    reason: &str,
) -> Result<u64, ApiError> {
    let catalog = build_public_catalog_from_db(state).await?;
    let video_count = catalog.videos.len();
    let _guard = state.catalog_lock.lock().await;
    let epoch = state.catalog_publish_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let catalog_address = read_catalog_address(&state.config);
    write_catalog_state(
        &state.config,
        catalog_address.as_deref(),
        Some(&catalog),
        true,
    )?;
    info!(
        "Queued local catalog update epoch={} reason={} videos={}",
        epoch, reason, video_count
    );
    Ok(epoch)
}

#[instrument(skip(state), fields(catalog_publish_epoch = epoch, reason = %reason))]
pub(crate) async fn publish_current_catalog_to_network(
    state: &AppState,
    epoch: u64,
    reason: &str,
) -> Result<(), ApiError> {
    sleep(StdDuration::from_millis(250)).await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Skipping stale catalog publish epoch={} reason={}",
            epoch, reason
        );
        return Ok(());
    }

    let _publish_guard = state.catalog_publish_lock.lock().await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Skipping stale catalog publish epoch={} reason={}",
            epoch, reason
        );
        return Ok(());
    }

    let catalog = build_public_catalog_from_db(state).await?;
    let video_count = catalog.videos.len();
    let start = Instant::now();
    let catalog_address = store_json_public(state, &catalog).await?;

    let _state_guard = state.catalog_lock.lock().await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Discarding stale catalog publish result epoch={} reason={} address={}",
            epoch, reason, catalog_address
        );
        return Ok(());
    }

    write_catalog_state(&state.config, Some(&catalog_address), Some(&catalog), false)?;
    set_current_catalog_address(state, &catalog_address).await?;
    info!(
        "Published catalog epoch={} reason={} videos={} address={} in {:.2}s",
        epoch,
        reason,
        video_count,
        catalog_address,
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
