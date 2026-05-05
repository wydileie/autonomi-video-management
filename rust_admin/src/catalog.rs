use std::{
    fs,
    path::{Path as FsPath, PathBuf},
    sync::atomic::Ordering,
    time::{Duration as StdDuration, Instant},
};

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};
use uuid::Uuid;

use crate::{
    config::Config,
    db::{db_error, parse_video_uuid, set_current_catalog_address},
    errors::ApiError,
    models::{
        ManifestOriginalFile, ManifestSegment, ManifestVariant, PublicCatalogDocument,
        PublicCatalogVariant, PublicCatalogVideo, SegmentOut, VariantOut, VideoManifestDocument,
        VideoOut,
    },
    state::AppState,
    storage::store_json_public,
    CATALOG_CONTENT_TYPE, STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

pub(crate) fn read_catalog_state_value(config: &Config) -> Option<Value> {
    let raw = match fs::read_to_string(&config.catalog_state_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(
                path = %config.catalog_state_path.display(),
                "Could not read catalog state file: {err}"
            );
            return None;
        }
    };

    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Some(value),
        Err(err) => {
            let broken_path = catalog_state_broken_path(&config.catalog_state_path);
            match fs::rename(&config.catalog_state_path, &broken_path) {
                Ok(()) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Quarantined invalid catalog state file: {err}"
                ),
                Err(rename_err) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Invalid catalog state file could not be quarantined: {err}; rename failed: {rename_err}"
                ),
            }
            None
        }
    }
}

fn catalog_state_broken_path(path: &FsPath) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog.json");
    path.with_file_name(format!("{file_name}.broken"))
}

fn catalog_address_from_state(value: &Value) -> Option<String> {
    value
        .get("catalog_address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn read_catalog_address(config: &Config) -> Option<String> {
    read_catalog_state_value(config)
        .as_ref()
        .and_then(catalog_address_from_state)
        .or_else(|| config.catalog_bootstrap_address.clone())
}

fn read_catalog_snapshot(config: &Config) -> Option<(Value, Option<String>)> {
    let value = read_catalog_state_value(config)?;
    let mut catalog = value.get("catalog")?.clone();
    if !catalog.is_object() {
        return None;
    }
    if !catalog.get("videos").is_some_and(Value::is_array) {
        catalog["videos"] = json!([]);
    }
    Some((
        catalog,
        catalog_address_from_state(&value).or_else(|| config.catalog_bootstrap_address.clone()),
    ))
}

fn empty_catalog() -> Value {
    json!({
        "schema_version": 1,
        "content_type": CATALOG_CONTENT_TYPE,
        "updated_at": Utc::now().to_rfc3339(),
        "videos": [],
    })
}

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

pub(crate) async fn get_db_video(
    state: &AppState,
    video_id: &str,
    include_segments: bool,
) -> Result<VideoOut, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let row = sqlx::query(
        r#"
        SELECT id, title, original_filename, description, status, created_at,
               manifest_address, catalog_address, error_message, final_quote,
               final_quote_created_at, approval_expires_at,
               is_public, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               publish_when_ready
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    db_video_to_out(state, &row, include_segments).await
}

pub(crate) async fn db_video_to_out(
    state: &AppState,
    row: &sqlx::postgres::PgRow,
    include_segments: bool,
) -> Result<VideoOut, ApiError> {
    let video_id: Uuid = row.try_get("id").map_err(db_error)?;
    let variant_rows = sqlx::query(
        r#"
        SELECT id, resolution, width, height, total_duration, segment_count
        FROM video_variants WHERE video_id=$1 ORDER BY height DESC
        "#,
    )
    .bind(video_id)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut variants = Vec::with_capacity(variant_rows.len());
    for variant in variant_rows {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let mut segments = Vec::new();
        if include_segments {
            let segment_rows = sqlx::query(
                r#"
                SELECT segment_index, autonomi_address, duration
                FROM video_segments WHERE variant_id=$1 ORDER BY segment_index
                "#,
            )
            .bind(variant_id)
            .fetch_all(&state.pool)
            .await
            .map_err(db_error)?;
            segments = segment_rows
                .into_iter()
                .map(|segment| SegmentOut {
                    segment_index: segment.try_get("segment_index").unwrap_or_default(),
                    autonomi_address: segment.try_get("autonomi_address").ok().flatten(),
                    duration: segment.try_get("duration").unwrap_or_default(),
                })
                .collect();
        }
        variants.push(VariantOut {
            id: variant_id.to_string(),
            resolution: variant.try_get("resolution").unwrap_or_default(),
            width: variant.try_get("width").unwrap_or_default(),
            height: variant.try_get("height").unwrap_or_default(),
            total_duration: variant.try_get("total_duration").ok().flatten(),
            segment_count: variant.try_get("segment_count").ok().flatten(),
            segments,
        });
    }

    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(db_error)?;
    let final_quote_created_at: Option<DateTime<Utc>> =
        row.try_get("final_quote_created_at").ok().flatten();
    let approval_expires_at: Option<DateTime<Utc>> =
        row.try_get("approval_expires_at").ok().flatten();
    let catalog_address = row
        .try_get::<Option<String>, _>("catalog_address")
        .ok()
        .flatten()
        .or_else(|| read_catalog_address(&state.config));

    Ok(VideoOut {
        id: video_id.to_string(),
        title: row.try_get("title").unwrap_or_default(),
        original_filename: row.try_get("original_filename").ok().flatten(),
        description: row.try_get("description").ok().flatten(),
        status: row.try_get("status").unwrap_or_default(),
        created_at: created_at.to_rfc3339(),
        manifest_address: row.try_get("manifest_address").ok().flatten(),
        catalog_address,
        is_public: row.try_get("is_public").unwrap_or(false),
        show_original_filename: row.try_get("show_original_filename").unwrap_or(false),
        show_manifest_address: row.try_get("show_manifest_address").unwrap_or(false),
        upload_original: row.try_get("upload_original").unwrap_or(false),
        original_file_address: row.try_get("original_file_address").ok().flatten(),
        original_file_byte_size: row.try_get("original_file_byte_size").ok().flatten(),
        publish_when_ready: row.try_get("publish_when_ready").unwrap_or(false),
        error_message: row.try_get("error_message").ok().flatten(),
        final_quote: row.try_get("final_quote").ok().flatten(),
        final_quote_created_at: final_quote_created_at.map(|value| value.to_rfc3339()),
        approval_expires_at: approval_expires_at.map(|value| value.to_rfc3339()),
        variants,
    })
}

pub(crate) fn catalog_entry_to_video_out(entry: &Value, catalog_address: Option<&str>) -> VideoOut {
    let show_manifest_address = entry
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    VideoOut {
        id: string_field(entry, "id"),
        title: string_field(entry, "title"),
        original_filename: None,
        description: opt_string_field(entry, "description"),
        status: opt_string_field(entry, "status").unwrap_or_else(|| STATUS_READY.into()),
        created_at: string_field(entry, "created_at"),
        manifest_address: if show_manifest_address {
            opt_string_field(entry, "manifest_address")
        } else {
            None
        },
        catalog_address: catalog_address.map(str::to_string),
        is_public: true,
        show_original_filename: false,
        show_manifest_address,
        upload_original: false,
        original_file_address: None,
        original_file_byte_size: None,
        publish_when_ready: false,
        error_message: None,
        final_quote: None,
        final_quote_created_at: None,
        approval_expires_at: None,
        variants: entry
            .get("variants")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|variant| VariantOut {
                id: format!(
                    "{}:{}",
                    string_field(entry, "id"),
                    string_field(variant, "resolution")
                ),
                resolution: string_field(variant, "resolution"),
                width: int_field(variant, "width"),
                height: int_field(variant, "height"),
                total_duration: variant.get("total_duration").and_then(Value::as_f64),
                segment_count: variant
                    .get("segment_count")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                segments: vec![],
            })
            .collect(),
    }
}

pub(crate) fn manifest_to_video_out(
    state: &AppState,
    manifest: &Value,
    manifest_address: Option<&str>,
    public: bool,
) -> VideoOut {
    let show_original_filename = manifest
        .get("show_original_filename")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let show_manifest_address = manifest
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let original_file = manifest
        .get("original_file")
        .filter(|value| value.is_object());
    let video_id = string_field(manifest, "id");
    VideoOut {
        id: video_id.clone(),
        title: string_field(manifest, "title"),
        original_filename: if !public {
            opt_string_field(manifest, "original_filename")
        } else {
            None
        },
        description: opt_string_field(manifest, "description"),
        status: opt_string_field(manifest, "status").unwrap_or_else(|| STATUS_READY.into()),
        created_at: string_field(manifest, "created_at"),
        manifest_address: if !public || show_manifest_address {
            manifest_address
                .map(str::to_string)
                .or_else(|| opt_string_field(manifest, "manifest_address"))
        } else {
            None
        },
        catalog_address: if public {
            None
        } else {
            read_catalog_address(&state.config)
        },
        is_public: public,
        show_original_filename: if public {
            false
        } else {
            show_original_filename
        },
        show_manifest_address,
        upload_original: original_file.is_some(),
        original_file_address: if public {
            None
        } else {
            original_file
                .and_then(|value| value.get("autonomi_address"))
                .and_then(Value::as_str)
                .map(str::to_string)
        },
        original_file_byte_size: if public {
            None
        } else {
            original_file
                .and_then(|value| value.get("byte_size"))
                .and_then(Value::as_i64)
        },
        publish_when_ready: false,
        error_message: None,
        final_quote: None,
        final_quote_created_at: None,
        approval_expires_at: None,
        variants: manifest
            .get("variants")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|variant| VariantOut {
                id: format!("{video_id}:{}", string_field(variant, "resolution")),
                resolution: string_field(variant, "resolution"),
                width: int_field(variant, "width"),
                height: int_field(variant, "height"),
                total_duration: variant.get("total_duration").and_then(Value::as_f64),
                segment_count: variant
                    .get("segment_count")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                segments: if public {
                    vec![]
                } else {
                    variant
                        .get("segments")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .map(|segment| SegmentOut {
                            segment_index: int_field(segment, "segment_index"),
                            autonomi_address: opt_string_field(segment, "autonomi_address"),
                            duration: segment
                                .get("duration")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                        })
                        .collect()
                },
            })
            .collect(),
    }
}

pub(crate) fn apply_catalog_visibility(
    video: &mut VideoOut,
    entry: &Value,
    _manifest: &Value,
    manifest_address: &str,
) {
    let show_manifest_address = entry
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    video.show_original_filename = false;
    video.show_manifest_address = show_manifest_address;
    video.original_filename = None;
    video.manifest_address = if show_manifest_address {
        Some(manifest_address.to_string())
    } else {
        None
    };
    video.original_file_address = None;
    video.original_file_byte_size = None;
}

fn original_file_manifest_from_row(row: &sqlx::postgres::PgRow) -> Option<ManifestOriginalFile> {
    let address = row
        .try_get::<Option<String>, _>("original_file_address")
        .ok()
        .flatten()?;
    Some(ManifestOriginalFile {
        autonomi_address: address,
        byte_size: row
            .try_get::<Option<i64>, _>("original_file_byte_size")
            .ok()
            .flatten(),
        autonomi_cost_atto: row
            .try_get::<Option<String>, _>("original_file_autonomi_cost_atto")
            .ok()
            .flatten(),
        payment_mode: row
            .try_get::<Option<String>, _>("original_file_autonomi_payment_mode")
            .ok()
            .flatten(),
    })
}

async fn build_ready_manifest_from_db(
    state: &AppState,
    video_id: &str,
) -> Result<VideoManifestDocument, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at,
               show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               original_file_autonomi_cost_atto, original_file_autonomi_payment_mode
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let variants = sqlx::query(
        r#"
        SELECT id, resolution, width, height, video_bitrate, audio_bitrate,
               segment_duration, total_duration
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut manifest_variants = Vec::new();
    for variant in variants {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let uploaded_segments = sqlx::query(
            r#"
            SELECT segment_index, autonomi_address, duration, byte_size
            FROM video_segments
            WHERE variant_id=$1
            ORDER BY segment_index
            "#,
        )
        .bind(variant_id)
        .fetch_all(&state.pool)
        .await
        .map_err(db_error)?;
        if uploaded_segments.iter().any(|segment| {
            segment
                .try_get::<Option<String>, _>("autonomi_address")
                .ok()
                .flatten()
                .is_none()
        }) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Video has not finished uploading all segment addresses",
            ));
        }
        let segment_count = uploaded_segments.len();
        let segments = uploaded_segments
            .iter()
            .map(|segment| ManifestSegment {
                segment_index: segment
                    .try_get::<i32, _>("segment_index")
                    .unwrap_or_default(),
                autonomi_address: segment
                    .try_get::<Option<String>, _>("autonomi_address")
                    .ok()
                    .flatten(),
                duration: segment.try_get::<f64, _>("duration").unwrap_or_default(),
                byte_size: segment
                    .try_get::<Option<i64>, _>("byte_size")
                    .ok()
                    .flatten(),
            })
            .collect();
        manifest_variants.push(ManifestVariant {
            id: variant_id.to_string(),
            resolution: variant
                .try_get::<String, _>("resolution")
                .unwrap_or_default(),
            width: variant.try_get::<i32, _>("width").unwrap_or_default(),
            height: variant.try_get::<i32, _>("height").unwrap_or_default(),
            video_bitrate: variant
                .try_get::<i32, _>("video_bitrate")
                .unwrap_or_default(),
            audio_bitrate: variant
                .try_get::<i32, _>("audio_bitrate")
                .unwrap_or_default(),
            segment_duration: variant
                .try_get::<f64, _>("segment_duration")
                .unwrap_or_default(),
            total_duration: variant
                .try_get::<Option<f64>, _>("total_duration")
                .ok()
                .flatten(),
            segment_count,
            segments,
        });
    }

    Ok(VideoManifestDocument {
        schema_version: 1,
        content_type: VIDEO_MANIFEST_CONTENT_TYPE.to_string(),
        id: video_id.to_string(),
        title: video_row.try_get::<String, _>("title").unwrap_or_default(),
        original_filename: None,
        description: video_row
            .try_get::<Option<String>, _>("description")
            .ok()
            .flatten(),
        status: STATUS_READY.to_string(),
        created_at: video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        updated_at: Utc::now().to_rfc3339(),
        show_original_filename: false,
        show_manifest_address: video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        original_file: original_file_manifest_from_row(&video_row),
        variants: manifest_variants,
    })
}

async fn build_catalog_entry_from_db(
    state: &AppState,
    video_id: &str,
    manifest_address: String,
) -> Result<PublicCatalogVideo, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at,
               show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let variant_rows = sqlx::query(
        r#"
        SELECT resolution, width, height, total_duration, segment_count
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    Ok(PublicCatalogVideo {
        id: video_id.to_string(),
        title: video_row.try_get("title").unwrap_or_default(),
        original_filename: None,
        description: video_row.try_get("description").ok().flatten(),
        status: STATUS_READY.to_string(),
        created_at: video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        updated_at: Utc::now().to_rfc3339(),
        manifest_address,
        show_original_filename: false,
        show_manifest_address: video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        variants: variant_rows
            .iter()
            .map(|variant| PublicCatalogVariant {
                resolution: variant
                    .try_get::<String, _>("resolution")
                    .unwrap_or_default(),
                width: variant.try_get::<i32, _>("width").unwrap_or_default(),
                height: variant.try_get::<i32, _>("height").unwrap_or_default(),
                segment_count: variant
                    .try_get::<Option<i32>, _>("segment_count")
                    .ok()
                    .flatten()
                    .unwrap_or(0),
                total_duration: variant
                    .try_get::<Option<f64>, _>("total_duration")
                    .ok()
                    .flatten(),
            })
            .collect(),
    })
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

async fn build_public_catalog_from_db(state: &AppState) -> Result<PublicCatalogDocument, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, manifest_address
        FROM videos
        WHERE status=$1
          AND is_public=TRUE
          AND manifest_address IS NOT NULL
        ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST
        "#,
    )
    .bind(STATUS_READY)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut videos = Vec::with_capacity(rows.len());
    for row in rows {
        let video_id: Uuid = row.try_get("id").map_err(db_error)?;
        let Some(manifest_address) = row
            .try_get::<Option<String>, _>("manifest_address")
            .ok()
            .flatten()
        else {
            continue;
        };
        videos.push(
            build_catalog_entry_from_db(state, &video_id.to_string(), manifest_address).await?,
        );
    }

    Ok(PublicCatalogDocument {
        schema_version: 1,
        content_type: CATALOG_CONTENT_TYPE.to_string(),
        updated_at: Utc::now().to_rfc3339(),
        videos,
    })
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

pub(crate) fn write_catalog_state<T: Serialize + ?Sized>(
    config: &Config,
    address: Option<&str>,
    catalog: Option<&T>,
    publish_pending: bool,
) -> Result<(), ApiError> {
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not create catalog state directory: {err}"),
            )
        })?;
    }
    let tmp_path = config.catalog_state_path.with_extension("tmp");
    let mut payload = json!({
        "catalog_address": address.unwrap_or(""),
        "updated_at": Utc::now().to_rfc3339(),
        "publish_pending": publish_pending,
        "note": "Local catalog snapshot plus the latest network-hosted catalog address.",
    });
    if let Some(catalog) = catalog {
        payload["catalog"] = serde_json::to_value(catalog).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not encode catalog state: {err}"),
            )
        })?;
    }
    fs::write(
        &tmp_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not write catalog state: {err}"),
        )
    })?;
    fs::rename(&tmp_path, &config.catalog_state_path).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not update catalog state: {err}"),
        )
    })
}

fn string_field(value: &Value, key: &str) -> String {
    opt_string_field(value, key).unwrap_or_default()
}

fn opt_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_field(value: &Value, key: &str) -> i32 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default() as i32
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::SocketAddr,
        sync::{atomic::AtomicU64, Arc},
    };

    use axum::http::HeaderValue;
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::{Mutex, Semaphore};
    use uuid::Uuid;

    use super::*;
    use crate::{
        antd_client::AntdRestClient,
        metrics::AdminMetrics,
        models::{PublicCatalogDocument, PublicCatalogVariant, PublicCatalogVideo},
        state::AppState,
    };

    fn test_config(catalog_state_path: PathBuf) -> Config {
        Config {
            db_dsn: "postgresql://example".to_string(),
            antd_url: "http://127.0.0.1:0".to_string(),
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
            upload_temp_dir: std::env::temp_dir(),
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
            admin_job_max_attempts: 1,
            catalog_publish_job_max_attempts: 1,
        }
    }

    fn test_state(config: Config) -> AppState {
        let metrics = Arc::new(AdminMetrics::default());
        AppState {
            config: Arc::new(config),
            pool: PgPoolOptions::new()
                .connect_lazy("postgresql://postgres:postgres@localhost/postgres")
                .unwrap(),
            antd: AntdRestClient::new("http://127.0.0.1:9", 1.0, metrics.clone()).unwrap(),
            metrics,
            catalog_lock: Arc::new(Mutex::new(())),
            catalog_publish_lock: Arc::new(Mutex::new(())),
            catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
            upload_save_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    #[test]
    fn invalid_catalog_state_is_quarantined_to_broken_file() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.json");
        fs::write(&path, "not valid json").unwrap();
        let config = test_config(path.clone());

        assert!(read_catalog_state_value(&config).is_none());

        assert!(!path.exists());
        assert!(path.with_file_name("catalog.json.broken").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_catalog_state_persists_snapshot_and_pending_flag() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        let path = dir.join("catalog.json");
        let config = test_config(path.clone());
        let catalog = PublicCatalogDocument {
            schema_version: 1,
            content_type: CATALOG_CONTENT_TYPE.to_string(),
            updated_at: "2026-05-05T00:00:00Z".to_string(),
            videos: vec![PublicCatalogVideo {
                id: "video-1".to_string(),
                title: "Example".to_string(),
                original_filename: None,
                description: None,
                status: STATUS_READY.to_string(),
                created_at: "2026-05-05T00:00:00Z".to_string(),
                updated_at: "2026-05-05T00:00:01Z".to_string(),
                manifest_address: "manifest-address".to_string(),
                show_original_filename: false,
                show_manifest_address: false,
                variants: vec![PublicCatalogVariant {
                    resolution: "720p".to_string(),
                    width: 1280,
                    height: 720,
                    segment_count: 1,
                    total_duration: Some(6.0),
                }],
            }],
        };

        write_catalog_state(&config, Some("catalog-address"), Some(&catalog), true).unwrap();

        let payload: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(payload["catalog_address"], "catalog-address");
        assert_eq!(payload["publish_pending"], true);
        assert_eq!(payload["catalog"]["videos"][0]["id"], "video-1");
        assert!(!path.with_extension("tmp").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn catalog_entry_to_video_out_applies_public_visibility() {
        let entry = json!({
            "id": "video-1",
            "title": "Public Video",
            "description": "Visible summary",
            "status": STATUS_READY,
            "created_at": "2026-05-05T00:00:00Z",
            "manifest_address": "manifest-address",
            "show_manifest_address": false,
            "variants": [{
                "resolution": "720p",
                "width": 1280,
                "height": 720,
                "segment_count": 2,
                "total_duration": 12.5
            }]
        });

        let video = catalog_entry_to_video_out(&entry, Some("catalog-address"));

        assert_eq!(video.id, "video-1");
        assert_eq!(video.manifest_address, None);
        assert_eq!(video.catalog_address.as_deref(), Some("catalog-address"));
        assert!(video.is_public);
        assert!(!video.show_original_filename);
        assert_eq!(video.variants.len(), 1);
        assert_eq!(video.variants[0].segment_count, Some(2));
    }

    #[tokio::test]
    async fn manifest_to_video_out_hides_sensitive_fields_for_public_view() {
        let dir = std::env::temp_dir().join(format!("autvid_catalog_{}", Uuid::new_v4()));
        let mut config = test_config(dir.join("catalog.json"));
        config.catalog_bootstrap_address = Some("catalog-bootstrap".to_string());
        let state = test_state(config);
        let manifest = json!({
            "id": "video-1",
            "title": "Demo",
            "original_filename": "raw-source.mov",
            "description": "Internal upload",
            "status": STATUS_READY,
            "created_at": "2026-05-05T00:00:00Z",
            "show_original_filename": true,
            "show_manifest_address": false,
            "original_file": {
                "autonomi_address": "original-address",
                "byte_size": 12345
            },
            "variants": [{
                "resolution": "720p",
                "width": 1280,
                "height": 720,
                "segment_count": 1,
                "total_duration": 6.0,
                "segments": [{
                    "segment_index": 0,
                    "autonomi_address": "segment-address",
                    "duration": 6.0
                }]
            }]
        });

        let admin_view = manifest_to_video_out(&state, &manifest, Some("manifest-address"), false);
        assert_eq!(
            admin_view.original_filename.as_deref(),
            Some("raw-source.mov")
        );
        assert_eq!(
            admin_view.manifest_address.as_deref(),
            Some("manifest-address")
        );
        assert_eq!(
            admin_view.catalog_address.as_deref(),
            Some("catalog-bootstrap")
        );
        assert_eq!(
            admin_view.original_file_address.as_deref(),
            Some("original-address")
        );
        assert_eq!(admin_view.variants[0].segments.len(), 1);

        let public_view = manifest_to_video_out(&state, &manifest, Some("manifest-address"), true);
        assert_eq!(public_view.original_filename, None);
        assert_eq!(public_view.manifest_address, None);
        assert_eq!(public_view.catalog_address, None);
        assert_eq!(public_view.original_file_address, None);
        assert!(public_view.variants[0].segments.is_empty());
    }
}
