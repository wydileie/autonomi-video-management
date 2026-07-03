use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteRow, Row};
use uuid::Uuid;

use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    models::{ManifestOriginalFile, ManifestSegment, ManifestVariant, VideoManifestDocument},
    state::AppState,
    STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

pub(crate) fn original_file_manifest_from_row(row: &SqliteRow) -> Option<ManifestOriginalFile> {
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

pub(crate) async fn build_ready_manifest_from_db(
    state: &AppState,
    video_id: &str,
) -> Result<VideoManifestDocument, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at, updated_at,
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
               video_codec, segment_container, segment_duration, total_duration
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
            video_codec: variant
                .try_get::<String, _>("video_codec")
                .unwrap_or_else(|_| "h264".to_string()),
            segment_container: variant
                .try_get::<String, _>("segment_container")
                .unwrap_or_else(|_| "mpegts".to_string()),
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
        updated_at: video_row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        show_original_filename: false,
        show_manifest_address: video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        original_file: original_file_manifest_from_row(&video_row),
        variants: manifest_variants,
    })
}
