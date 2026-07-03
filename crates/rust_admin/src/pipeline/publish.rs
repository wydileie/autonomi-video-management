use std::{fs, path::PathBuf, sync::Arc, time::Instant};

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use sqlx::Row;
use tokio::{fs as tokio_fs, sync::Semaphore, task::JoinSet};
use tracing::{info, instrument};
use uuid::Uuid;

use super::*;

use crate::{
    antd_client::is_missing_file_upload_endpoint,
    catalog::{read_catalog_address, refresh_local_catalog_from_db},
    db::{db_error, parse_video_uuid, set_publication, set_ready},
    errors::ApiError,
    jobs::schedule_catalog_publish,
    media::assert_under,
    models::{ManifestSegment, ManifestVariant, VideoManifestDocument},
    state::AppState,
    storage::{put_public_verified_inner, store_json_public},
    MIN_ANTD_SELF_ENCRYPTION_BYTES, STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

#[instrument(skip(state), fields(video_id = %video_id))]
pub(crate) async fn upload_approved_video_inner(
    state: &AppState,
    video_id: &str,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at, job_dir,
               job_source_path, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               original_file_autonomi_cost_atto, original_file_autonomi_payment_mode,
               publish_when_ready
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Video row {video_id} disappeared before upload"),
        )
    })?;

    let job_dir: Option<String> = video_row.try_get("job_dir").ok().flatten();
    let upload_original = video_row.try_get("upload_original").unwrap_or(false);
    let publish_when_ready = video_row.try_get("publish_when_ready").unwrap_or(false);
    let original_file = if upload_original {
        upload_original_file_if_needed(state, video_uuid, video_id, &video_row).await?
    } else {
        None
    };
    let manifest_created_at = video_row
        .try_get::<DateTime<Utc>, _>("created_at")
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|_| Utc::now().to_rfc3339());
    let show_manifest_address = video_row
        .try_get::<bool, _>("show_manifest_address")
        .unwrap_or(false);

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

    struct SegmentUploadInput {
        segment_index: i32,
        local_path: PathBuf,
        label: String,
    }
    struct SegmentUploadResult {
        segment_index: i32,
        address: String,
        cost: Option<String>,
        payment_mode: String,
        byte_size: i64,
    }

    let mut manifest_variants = Vec::new();
    for variant in variants {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let resolution = variant
            .try_get::<String, _>("resolution")
            .unwrap_or_default();
        let segment_rows = sqlx::query(
            r#"
            SELECT segment_index, local_path, duration, byte_size, autonomi_address
            FROM video_segments
            WHERE variant_id=$1
            ORDER BY segment_index
            "#,
        )
        .bind(variant_id)
        .fetch_all(&state.pool)
        .await
        .map_err(db_error)?;
        if segment_rows.is_empty() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "No segments found for {}",
                    variant
                        .try_get::<String, _>("resolution")
                        .unwrap_or_default()
                ),
            ));
        }

        info!(
            "Uploading {} approved segments for {}/{} with payment_mode={} concurrency={}",
            segment_rows.len(),
            video_id,
            resolution,
            state.config.antd_payment_mode,
            state.config.antd_upload_concurrency
        );
        let mut upload_inputs = Vec::new();
        for segment in &segment_rows {
            let existing_address: Option<String> =
                segment.try_get("autonomi_address").ok().flatten();
            if existing_address.is_some() {
                continue;
            }
            let local_path: Option<String> = segment.try_get("local_path").ok().flatten();
            let path = local_path.as_deref().map(PathBuf::from).ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Transcoded segment is missing from disk",
                )
            })?;
            let path = assert_under(&path, &state.config.upload_temp_dir)?;
            if !path.exists() {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Transcoded segment is missing from disk: {}",
                        path.display()
                    ),
                ));
            }
            let segment_index = segment
                .try_get::<i32, _>("segment_index")
                .unwrap_or_default();
            upload_inputs.push(SegmentUploadInput {
                segment_index,
                local_path: path,
                label: format!("{}/{}/segment-{segment_index:05}", video_id, resolution),
            });
        }

        let upload_started = Instant::now();
        let semaphore = Arc::new(Semaphore::new(state.config.antd_upload_concurrency));
        let mut jobs = JoinSet::new();
        for input in upload_inputs {
            let antd = state.antd.clone();
            let semaphore = semaphore.clone();
            let payment_mode = state.config.antd_payment_mode.clone();
            let upload_verify = state.config.antd_upload_verify;
            let upload_retries = state.config.antd_upload_retries;
            let direct_upload_max_bytes = state.config.antd_direct_upload_max_bytes;
            jobs.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|err| err.to_string())?;
                let metadata = tokio_fs::metadata(&input.local_path)
                    .await
                    .map_err(|err| format!("Could not inspect transcoded segment: {err}"))?;
                let byte_size = metadata.len() as i64;
                if metadata.len() < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
                    return Err(format!(
                        "Transcoded segment is too small to store on Autonomi: {} has {} bytes",
                        input.label, byte_size
                    ));
                }
                match antd
                    .file_put_public(
                        &input.local_path,
                        &payment_mode,
                        upload_verify,
                        upload_retries,
                    )
                    .await
                {
                    Ok(result) => Ok::<SegmentUploadResult, String>(SegmentUploadResult {
                        segment_index: input.segment_index,
                        address: result.address,
                        cost: Some(result.storage_cost_atto),
                        payment_mode: result.payment_mode_used,
                        byte_size: result.byte_size as i64,
                    }),
                    Err(err) if is_missing_file_upload_endpoint(&err) => {
                        if metadata.len() as usize > direct_upload_max_bytes {
                            return Err(format!(
                                "Autonomi file upload endpoint is unavailable and legacy JSON upload for {} would exceed ANTD_DIRECT_UPLOAD_MAX_BYTES ({})",
                                input.label,
                                direct_upload_max_bytes
                            ));
                        }
                        let data = tokio_fs::read(&input.local_path)
                            .await
                            .map_err(|err| format!("Could not read transcoded segment: {err}"))?;
                        let result = put_public_verified_inner(
                            antd,
                            payment_mode.clone(),
                            upload_verify,
                            upload_retries,
                            data,
                            input.label,
                        )
                        .await?;
                        Ok(SegmentUploadResult {
                            segment_index: input.segment_index,
                            address: result.address,
                            cost: result.cost,
                            payment_mode,
                            byte_size,
                        })
                    }
                    Err(err) => Err(format!(
                        "Autonomi file upload failed for {}: {err}",
                        input.label
                    )),
                }
            });
        }

        let mut uploaded_results = Vec::new();
        while let Some(joined) = jobs.join_next().await {
            let result = joined.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Segment upload task failed: {err}"),
                )
            })?;
            uploaded_results.push(result.map_err(|err| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Autonomi segment upload failed: {err}"),
                )
            })?);
        }
        uploaded_results.sort_by_key(|result| result.segment_index);
        if !uploaded_results.is_empty() {
            info!(
                "Uploaded {} segments for {}/{} in {:.2}s",
                uploaded_results.len(),
                video_id,
                resolution,
                upload_started.elapsed().as_secs_f64()
            );
        }

        for result in uploaded_results {
            sqlx::query(
                r#"
                UPDATE video_segments
                SET autonomi_address=$1,
                    autonomi_cost_atto=$2,
                    autonomi_payment_mode=$3,
                    byte_size=$4
                WHERE variant_id=$5 AND segment_index=$6
                "#,
            )
            .bind(&result.address)
            .bind(result.cost.as_deref())
            .bind(&result.payment_mode)
            .bind(result.byte_size)
            .bind(variant_id)
            .bind(result.segment_index)
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        }

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

    let manifest = VideoManifestDocument {
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
        created_at: manifest_created_at,
        updated_at: Utc::now().to_rfc3339(),
        show_original_filename: false,
        show_manifest_address,
        original_file,
        variants: manifest_variants,
    };
    let manifest_address = store_json_public(state, &manifest).await?;
    let catalog_address = read_catalog_address(&state.config);
    set_ready(
        state,
        video_id,
        &manifest_address,
        catalog_address.as_deref(),
    )
    .await?;
    let mut is_public = false;
    if publish_when_ready {
        set_publication(
            state,
            video_id,
            true,
            Some(&manifest_address),
            catalog_address.as_deref(),
        )
        .await?;
        let epoch = refresh_local_catalog_from_db(state, "auto-publish").await?;
        schedule_catalog_publish(state, epoch, format!("auto-publish:{video_id}")).await?;
        is_public = true;
    }
    if let Some(job_dir) = job_dir {
        let _ = fs::remove_dir_all(job_dir);
    }
    info!(
        "Video {} is ready manifest={} catalog={:?} public={}",
        video_id, manifest_address, catalog_address, is_public
    );
    Ok(())
}
