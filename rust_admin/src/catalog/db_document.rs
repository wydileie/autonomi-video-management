use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::state_file::read_catalog_address;
use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    models::{
        ManifestOriginalFile, ManifestSegment, ManifestVariant, PublicCatalogDocument,
        PublicCatalogVariant, PublicCatalogVideo, SegmentOut, VariantOut, VideoManifestDocument,
        VideoOut,
    },
    state::AppState,
    CATALOG_CONTENT_TYPE, STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

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

pub(super) async fn build_ready_manifest_from_db(
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

pub(super) async fn build_public_catalog_from_db(
    state: &AppState,
) -> Result<PublicCatalogDocument, ApiError> {
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

fn string_field(value: &Value, key: &str) -> String {
    opt_string_field(value, key).unwrap_or_default()
}

fn opt_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_field(value: &Value, key: &str) -> i32 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default() as i32
}
