use std::{
    collections::HashMap,
    fs,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Instant,
};

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use sqlx::Row;
use tokio::{fs as tokio_fs, sync::Semaphore, task::JoinSet};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    antd_client::is_missing_file_upload_endpoint,
    catalog::{read_catalog_address, refresh_local_catalog_from_db},
    db::{
        db_error, parse_video_uuid, set_awaiting_approval, set_publication, set_ready, set_status,
    },
    errors::ApiError,
    jobs::schedule_catalog_publish,
    media::{probe_duration, probe_video_dimensions, transcode_renditions},
    models::QuoteValue,
    quote::{parse_cost_u128, quote_data_size},
    state::AppState,
    storage::{put_public_verified_inner, put_public_verified_with_mode, store_json_public},
    MIN_ANTD_SELF_ENCRYPTION_BYTES, STATUS_PROCESSING, STATUS_READY, VIDEO_MANIFEST_CONTENT_TYPE,
};

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

    let total_duration = probe_duration(source_path).await?.unwrap_or(0.0);
    let source_dimensions = probe_video_dimensions(source_path).await?;
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
        let variant_row = sqlx::query(
            r#"
            INSERT INTO video_variants
                (video_id, resolution, width, height, video_bitrate, audio_bitrate,
                 segment_duration, total_duration, segment_count)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            RETURNING id
        "#,
        )
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

        for segment in rendition.segments {
            sqlx::query(
                r#"
                INSERT INTO video_segments
                    (variant_id, segment_index, duration, byte_size, local_path)
                VALUES ($1,$2,$3,$4,$5)
                ON CONFLICT (variant_id, segment_index) DO UPDATE
                  SET duration=EXCLUDED.duration,
                      byte_size=EXCLUDED.byte_size,
                      local_path=EXCLUDED.local_path
            "#,
            )
            .bind(variant_id)
            .bind(segment.segment_index)
            .bind(segment.duration)
            .bind(segment.byte_size)
            .bind(segment.local_path.to_string_lossy().as_ref())
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        }
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

pub(crate) async fn build_final_upload_quote(
    state: &AppState,
    video_id: &str,
) -> Result<Value, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    #[derive(Default)]
    struct FinalVariantQuote {
        resolution: String,
        width: i32,
        height: i32,
        segment_count: i64,
        estimated_bytes: i64,
        actual_bytes: i64,
        chunk_count: i64,
        storage_cost_atto: u128,
        estimated_gas_cost_wei: u128,
        payment_mode: String,
    }
    struct FinalSegmentQuoteInput {
        order: usize,
        variant_id: Uuid,
        resolution: String,
        segment_index: i32,
        width: i32,
        height: i32,
        total_duration: Option<f64>,
        local_path: PathBuf,
    }
    struct FinalSegmentQuoteResult {
        order: usize,
        variant_id: Uuid,
        resolution: String,
        width: i32,
        height: i32,
        total_duration: Option<f64>,
        byte_size: i64,
        storage_cost: u128,
        gas_cost: u128,
        chunk_count: i64,
        payment_mode: String,
    }

    let video_row = sqlx::query(
        r#"
        SELECT upload_original, job_source_path
        FROM videos
        WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let upload_original = video_row.try_get("upload_original").unwrap_or(false);
    let original_source_path: Option<PathBuf> = video_row
        .try_get::<Option<String>, _>("job_source_path")
        .ok()
        .flatten()
        .map(PathBuf::from);

    let rows = sqlx::query(
        r#"
        SELECT v.id AS variant_id, v.resolution, v.width, v.height, v.total_duration,
               s.segment_index, s.local_path, s.byte_size
        FROM video_variants v
        JOIN video_segments s ON s.variant_id = v.id
        WHERE v.video_id=$1
        ORDER BY v.height DESC, s.segment_index
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    if rows.is_empty() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No transcoded segments were found for final quote",
        ));
    }

    let mut inputs = Vec::with_capacity(rows.len());
    for (order, row) in rows.iter().enumerate() {
        let local_path: Option<String> = row.try_get("local_path").ok().flatten();
        let path = local_path.as_deref().map(PathBuf::from).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Transcoded segment is missing from disk",
            )
        })?;
        if !path.exists() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Transcoded segment is missing from disk: {}",
                    path.display()
                ),
            ));
        }
        inputs.push(FinalSegmentQuoteInput {
            order,
            variant_id: row.try_get("variant_id").map_err(db_error)?,
            resolution: row.try_get("resolution").unwrap_or_default(),
            segment_index: row.try_get("segment_index").unwrap_or_default(),
            width: row.try_get("width").unwrap_or_default(),
            height: row.try_get("height").unwrap_or_default(),
            total_duration: row
                .try_get::<Option<f64>, _>("total_duration")
                .ok()
                .flatten(),
            local_path: path,
        });
    }

    let quote_started = Instant::now();
    let semaphore = Arc::new(Semaphore::new(state.config.antd_quote_concurrency));
    let mut jobs = JoinSet::new();
    for input in inputs {
        let antd = state.antd.clone();
        let semaphore = semaphore.clone();
        let default_payment_mode = state.config.antd_payment_mode.clone();
        jobs.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|err| err.to_string())?;
            let metadata = tokio_fs::metadata(&input.local_path)
                .await
                .map_err(|err| format!("Could not inspect transcoded segment: {err}"))?;
            let byte_size = metadata.len();
            if byte_size < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
                return Err(format!(
                    "Transcoded segment is too small to store on Autonomi: {}/{}/segment-{:05} has {} bytes",
                    input.resolution,
                    input.variant_id,
                    input.segment_index,
                    byte_size
                ));
            }
            let estimate = antd
                .data_cost_for_size(byte_size as usize)
                .await
                .map_err(|err| {
                    format!(
                        "Could not get final Autonomi price quote for {}/segment-{:05} ({} bytes): {err}",
                        input.resolution, input.segment_index, byte_size
                    )
                })?;
            Ok::<FinalSegmentQuoteResult, String>(FinalSegmentQuoteResult {
                order: input.order,
                variant_id: input.variant_id,
                resolution: input.resolution,
                width: input.width,
                height: input.height,
                total_duration: input.total_duration,
                byte_size: byte_size as i64,
                storage_cost: parse_cost_u128(estimate.cost.as_deref()),
                gas_cost: parse_cost_u128(estimate.estimated_gas_cost_wei.as_deref()),
                chunk_count: estimate.chunk_count.unwrap_or(0),
                payment_mode: estimate.payment_mode.unwrap_or(default_payment_mode),
            })
        });
    }

    let mut results = Vec::with_capacity(rows.len());
    while let Some(joined) = jobs.join_next().await {
        let result = joined.map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Final quote task failed: {err}"),
            )
        })?;
        results.push(result.map_err(|err| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Could not get final Autonomi price quote: {err}"),
            )
        })?);
    }
    results.sort_by_key(|result| result.order);
    info!(
        "Final quote for {} checked {} segments in {:.2}s with concurrency={}",
        video_id,
        results.len(),
        quote_started.elapsed().as_secs_f64(),
        state.config.antd_quote_concurrency
    );

    let mut variants = Vec::<FinalVariantQuote>::new();
    let mut variant_indexes = HashMap::<String, usize>::new();
    let mut quote_cache = HashMap::<i64, QuoteValue>::new();
    let mut total_storage_cost = 0_u128;
    let mut total_gas_cost = 0_u128;
    let mut total_bytes = 0_i64;
    let mut total_chunks = 0_i64;
    let mut max_duration = 0.0_f64;
    let mut original_file_quote = None;

    for result in results {
        let variant_id = result.variant_id;
        let variant_key = variant_id.to_string();
        let index = *variant_indexes.entry(variant_key).or_insert_with(|| {
            variants.push(FinalVariantQuote {
                resolution: result.resolution.clone(),
                width: result.width,
                height: result.height,
                payment_mode: result.payment_mode.clone(),
                ..FinalVariantQuote::default()
            });
            variants.len() - 1
        });
        let variant = &mut variants[index];
        variant.segment_count += 1;
        variant.estimated_bytes += result.byte_size;
        variant.actual_bytes += result.byte_size;
        variant.chunk_count += result.chunk_count;
        variant.storage_cost_atto += result.storage_cost;
        variant.estimated_gas_cost_wei += result.gas_cost;

        total_storage_cost += result.storage_cost;
        total_gas_cost += result.gas_cost;
        total_bytes += result.byte_size;
        total_chunks += result.chunk_count;
        if let Some(duration) = result.total_duration {
            max_duration = max_duration.max(duration);
        }
    }
    let actual_transcoded_bytes = total_bytes;

    if upload_original {
        let path = original_source_path.ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Original source file is missing from disk",
            )
        })?;
        if !path.exists() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Original source file is missing from disk: {}",
                    path.display()
                ),
            ));
        }
        let metadata = tokio_fs::metadata(&path).await.map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not inspect original source file: {err}"),
            )
        })?;
        let byte_size = metadata.len() as i64;
        let quote = quote_data_size(state, byte_size, &mut quote_cache)
            .await
            .map_err(|err| {
                ApiError::new(
                    err.status,
                    format!(
                        "Could not get final Autonomi price quote for original file: {}",
                        err.detail
                    ),
                )
            })?;
        let storage_cost = quote.storage_cost_atto;
        let gas_cost = quote.estimated_gas_cost_wei;
        let chunk_count = quote.chunk_count;
        let payment_mode = quote.payment_mode;
        total_storage_cost += storage_cost;
        total_gas_cost += gas_cost;
        total_bytes += byte_size;
        total_chunks += chunk_count;
        original_file_quote = Some(json!({
            "byte_size": byte_size,
            "chunk_count": chunk_count,
            "storage_cost_atto": storage_cost.to_string(),
            "estimated_gas_cost_wei": gas_cost.to_string(),
            "payment_mode": payment_mode,
        }));
    }

    let manifest_bytes = 4096 + (variants.len() as i64 * 1024) + (rows.len() as i64 * 220);
    let catalog_bytes = 2048 + (variants.len() as i64 * 512);
    let metadata_quote =
        quote_data_size(state, manifest_bytes + catalog_bytes, &mut quote_cache).await?;

    total_storage_cost += metadata_quote.storage_cost_atto;
    total_gas_cost += metadata_quote.estimated_gas_cost_wei;
    total_bytes += manifest_bytes + catalog_bytes;
    total_chunks += metadata_quote.chunk_count;

    let variant_values = variants
        .into_iter()
        .map(|variant| {
            json!({
                "resolution": variant.resolution,
                "width": variant.width,
                "height": variant.height,
                "segment_count": variant.segment_count,
                "estimated_bytes": variant.estimated_bytes,
                "actual_bytes": variant.actual_bytes,
                "chunk_count": variant.chunk_count,
                "storage_cost_atto": variant.storage_cost_atto.to_string(),
                "estimated_gas_cost_wei": variant.estimated_gas_cost_wei.to_string(),
                "payment_mode": variant.payment_mode,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "quote_type": "final",
        "duration_seconds": max_duration,
        "segment_duration": state.config.hls_segment_duration,
        "payment_mode": state.config.antd_payment_mode.clone(),
        "estimated_bytes": total_bytes,
        "actual_media_bytes": total_bytes - (manifest_bytes + catalog_bytes),
        "actual_transcoded_bytes": actual_transcoded_bytes,
        "segment_count": rows.len(),
        "chunk_count": total_chunks,
        "storage_cost_atto": total_storage_cost.to_string(),
        "estimated_gas_cost_wei": total_gas_cost.to_string(),
        "metadata_bytes": manifest_bytes + catalog_bytes,
        "original_file": original_file_quote,
        "sampled": metadata_quote.sampled,
        "approval_ttl_seconds": state.config.final_quote_approval_ttl_seconds,
        "variants": variant_values,
    }))
}

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
    let mut manifest = json!({
        "schema_version": 1,
        "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
        "id": video_id,
        "title": video_row.try_get::<String, _>("title").unwrap_or_default(),
        "original_filename": Value::Null,
        "description": video_row.try_get::<Option<String>, _>("description").ok().flatten(),
        "status": STATUS_READY,
        "created_at": video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        "updated_at": Utc::now().to_rfc3339(),
        "show_original_filename": false,
        "show_manifest_address": video_row.try_get::<bool, _>("show_manifest_address").unwrap_or(false),
        "original_file": original_file.unwrap_or(Value::Null),
        "variants": [],
    });

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
                    .file_put_public(&input.local_path, &payment_mode, upload_verify)
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

        manifest_variants.push(json!({
            "id": variant_id.to_string(),
            "resolution": variant.try_get::<String, _>("resolution").unwrap_or_default(),
            "width": variant.try_get::<i32, _>("width").unwrap_or_default(),
            "height": variant.try_get::<i32, _>("height").unwrap_or_default(),
            "video_bitrate": variant.try_get::<i32, _>("video_bitrate").unwrap_or_default(),
            "audio_bitrate": variant.try_get::<i32, _>("audio_bitrate").unwrap_or_default(),
            "segment_duration": variant.try_get::<f64, _>("segment_duration").unwrap_or_default(),
            "total_duration": variant.try_get::<Option<f64>, _>("total_duration").ok().flatten(),
            "segment_count": uploaded_segments.len(),
            "segments": uploaded_segments
                .iter()
                .map(|segment| {
                    json!({
                        "segment_index": segment.try_get::<i32, _>("segment_index").unwrap_or_default(),
                        "autonomi_address": segment.try_get::<Option<String>, _>("autonomi_address").ok().flatten(),
                        "duration": segment.try_get::<f64, _>("duration").unwrap_or_default(),
                        "byte_size": segment.try_get::<Option<i64>, _>("byte_size").ok().flatten(),
                    })
                })
                .collect::<Vec<_>>(),
        }));
    }

    manifest["updated_at"] = json!(Utc::now().to_rfc3339());
    manifest["variants"] = json!(manifest_variants);
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

async fn upload_original_file_if_needed(
    state: &AppState,
    video_uuid: Uuid,
    video_id: &str,
    video_row: &sqlx::postgres::PgRow,
) -> Result<Option<Value>, ApiError> {
    if let Some(address) = video_row
        .try_get::<Option<String>, _>("original_file_address")
        .ok()
        .flatten()
    {
        return Ok(Some(json!({
            "autonomi_address": address,
            "byte_size": video_row
                .try_get::<Option<i64>, _>("original_file_byte_size")
                .ok()
                .flatten(),
            "autonomi_cost_atto": video_row
                .try_get::<Option<String>, _>("original_file_autonomi_cost_atto")
                .ok()
                .flatten(),
            "payment_mode": video_row
                .try_get::<Option<String>, _>("original_file_autonomi_payment_mode")
                .ok()
                .flatten(),
        })));
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
                warn!(
                    label = %upload_label,
                    byte_size = metadata.len(),
                    direct_upload_max_bytes = state.config.antd_direct_upload_max_bytes,
                    "Skipping optional original source upload because the Autonomi file upload endpoint is unavailable and legacy JSON upload would exceed the configured cap"
                );
                return Ok(None);
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
            updated_at=NOW()
        WHERE id=$5
        "#,
    )
    .bind(&address)
    .bind(byte_size)
    .bind(cost.as_deref())
    .bind(&payment_mode)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    Ok(Some(json!({
        "autonomi_address": address,
        "byte_size": byte_size,
        "autonomi_cost_atto": cost,
        "payment_mode": payment_mode,
    })))
}
