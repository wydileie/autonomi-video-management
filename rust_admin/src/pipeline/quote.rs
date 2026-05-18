use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use axum::http::StatusCode;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::{fs as tokio_fs, sync::Semaphore, task::JoinSet};
use tracing::{info, instrument};
use uuid::Uuid;

use crate::{
    db::{db_error, parse_video_uuid},
    errors::ApiError,
    media::assert_under,
    models::QuoteValue,
    quote::{parse_cost_u128, quote_data_size},
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

#[instrument(skip(state), fields(video_id = %video_id))]
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
