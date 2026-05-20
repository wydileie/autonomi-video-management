mod manifest;
mod quote;
mod upload;

use std::{fs, path::Path as FsPath};

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::{QueryBuilder, Row, Sqlite};
use tracing::{info, instrument};
use uuid::Uuid;

pub(crate) use quote::build_final_upload_quote;
pub(crate) use upload::upload_approved_video_inner;

use crate::{
    db::{db_error, parse_video_uuid, set_awaiting_approval, set_status},
    errors::ApiError,
    media::{probe_duration, probe_video_dimensions, transcode_renditions},
    state::AppState,
    STATUS_PROCESSING,
};

#[instrument(
    skip(state, source_path, resolutions, job_dir),
    fields(video_id = %video_id, resolution_count = resolutions.len(), reset_existing = reset_existing)
)]
pub(crate) async fn process_video_inner(
    state: &AppState,
    video_id: &str,
    source_path: &FsPath,
    resolutions: &[String],
    job_dir: &FsPath,
    reset_existing: bool,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    if reset_existing {
        sqlx::query("DELETE FROM video_variants WHERE video_id=$1")
            .bind(video_uuid)
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        for resolution in resolutions {
            let _ = fs::remove_dir_all(job_dir.join(resolution));
        }
    }

    set_status(state, video_id, STATUS_PROCESSING, None).await?;
    let exists = sqlx::query("SELECT id FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .is_some();
    if !exists {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Video row {video_id} disappeared before processing"),
        ));
    }

    let total_duration = probe_duration(state, source_path).await?.unwrap_or(0.0);
    let source_dimensions = probe_video_dimensions(state, source_path).await?;
    let renditions = transcode_renditions(
        state,
        video_id,
        source_path,
        resolutions,
        job_dir,
        source_dimensions,
    )
    .await?;

    for rendition in renditions {
        let variant_id = Uuid::new_v4();
        let variant_row = sqlx::query(
            r#"
            INSERT INTO video_variants
                (id, video_id, resolution, width, height, video_bitrate, audio_bitrate,
                 segment_duration, total_duration, segment_count)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
            RETURNING id
        "#,
        )
        .bind(variant_id)
        .bind(video_uuid)
        .bind(&rendition.resolution)
        .bind(rendition.width)
        .bind(rendition.height)
        .bind(rendition.video_kbps * 1000)
        .bind(rendition.audio_kbps * 1000)
        .bind(state.config.hls_segment_duration)
        .bind(total_duration)
        .bind(rendition.segments.len() as i32)
        .fetch_one(&state.pool)
        .await
        .map_err(db_error)?;
        let variant_id: Uuid = variant_row.try_get("id").map_err(db_error)?;

        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            INSERT INTO video_segments
                (id, variant_id, segment_index, duration, byte_size, local_path)
            "#,
        );
        builder.push_values(rendition.segments.iter(), |mut row, segment| {
            row.push_bind(Uuid::new_v4())
                .push_bind(variant_id)
                .push_bind(segment.segment_index)
                .push_bind(segment.duration)
                .push_bind(segment.byte_size)
                .push_bind(segment.local_path.to_string_lossy().to_string());
        });
        builder.push(
            r#"
            ON CONFLICT (variant_id, segment_index) DO UPDATE
              SET duration=EXCLUDED.duration,
                  byte_size=EXCLUDED.byte_size,
                  local_path=EXCLUDED.local_path
            "#,
        );
        builder
            .build()
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
    }

    let mut final_quote = build_final_upload_quote(state, video_id).await?;
    let expires_at = Utc::now() + Duration::seconds(state.config.final_quote_approval_ttl_seconds);
    final_quote["approval_expires_at"] = json!(expires_at.to_rfc3339());
    final_quote["quote_created_at"] = json!(Utc::now().to_rfc3339());
    set_awaiting_approval(state, video_id, final_quote, expires_at).await?;
    info!(
        "Video {} is awaiting approval expires_at={}",
        video_id,
        expires_at.to_rfc3339()
    );
    Ok(())
}
