use axum::http::StatusCode;

use crate::{
    errors::ApiError,
    media::{
        enforce_upload_media_limits, estimate_transcoded_bytes, resolution_preset,
        supported_resolutions_error, target_dimensions_for_source, target_video_bitrate_kbps,
    },
    models::{
        QuoteValue, UploadQuoteOriginalOut, UploadQuoteOut, UploadQuoteRequest,
        UploadQuoteVariantOut,
    },
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

pub(crate) fn ceil_ratio(value: i64, numerator: i64, denominator: i64) -> i64 {
    if denominator <= 0 {
        return 0;
    }
    ((i128::from(value) * i128::from(numerator) + i128::from(denominator) - 1)
        / i128::from(denominator)) as i64
}

pub(crate) fn ceil_ratio_u128(value: u128, numerator: i64, denominator: i64) -> u128 {
    if value == 0 || numerator <= 0 || denominator <= 0 {
        return 0;
    }
    let numerator = numerator as u128;
    let denominator = denominator as u128;
    (value * numerator).div_ceil(denominator)
}

pub(crate) fn quote_sample_bytes(byte_size: i64, max_sample_bytes: usize) -> Option<i64> {
    if byte_size <= 0 {
        return None;
    }
    let max_sample_bytes = max_sample_bytes.max(MIN_ANTD_SELF_ENCRYPTION_BYTES) as i64;
    Some(
        byte_size
            .min(max_sample_bytes)
            .max(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64),
    )
}

pub(crate) async fn quote_data_size(
    state: &AppState,
    byte_size: i64,
    cache: &mut std::collections::HashMap<i64, QuoteValue>,
) -> Result<QuoteValue, ApiError> {
    if byte_size <= 0 {
        return Ok(QuoteValue {
            sampled: false,
            storage_cost_atto: 0,
            estimated_gas_cost_wei: 0,
            chunk_count: 0,
            payment_mode: state.config.antd_payment_mode.clone(),
        });
    }

    let quote_bytes = byte_size.max(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64);
    let sample_bytes = quote_sample_bytes(byte_size, state.config.upload_quote_max_sample_bytes)
        .unwrap_or(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64);
    if cache.get(&sample_bytes).is_none() {
        let estimate = state
            .antd
            .data_cost_for_size(sample_bytes as usize)
            .await
            .map_err(|err| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Could not get Autonomi price quote: {err}"),
                )
            })?;
        cache.insert(
            sample_bytes,
            QuoteValue {
                sampled: false,
                storage_cost_atto: parse_cost_u128(estimate.cost.as_deref()),
                estimated_gas_cost_wei: parse_cost_u128(estimate.estimated_gas_cost_wei.as_deref()),
                chunk_count: estimate.chunk_count.unwrap_or(0),
                payment_mode: estimate
                    .payment_mode
                    .unwrap_or_else(|| state.config.antd_payment_mode.clone()),
            },
        );
    }

    let quoted = cache.get(&sample_bytes).cloned().unwrap();
    if sample_bytes == quote_bytes {
        return Ok(quoted);
    }

    Ok(QuoteValue {
        sampled: true,
        storage_cost_atto: ceil_ratio_u128(quoted.storage_cost_atto, quote_bytes, sample_bytes),
        estimated_gas_cost_wei: ceil_ratio_u128(
            quoted.estimated_gas_cost_wei,
            quote_bytes,
            sample_bytes,
        ),
        chunk_count: ceil_ratio(quoted.chunk_count, quote_bytes, sample_bytes).max(1),
        payment_mode: quoted.payment_mode,
    })
}

pub(crate) fn parse_cost_u128(value: Option<&str>) -> u128 {
    value
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(0)
}

pub(crate) async fn build_upload_quote(
    state: &AppState,
    request: UploadQuoteRequest,
) -> Result<UploadQuoteOut, ApiError> {
    if request.duration_seconds <= 0.0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "duration_seconds must be greater than zero",
        ));
    }

    let source_dimensions = match (request.source_width, request.source_height) {
        (None, None) => None,
        (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
        (Some(_), Some(_)) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "source_width and source_height must be greater than zero",
            ))
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "source_width and source_height must be provided together",
            ))
        }
    };

    if let Some((width, height)) = source_dimensions {
        enforce_upload_media_limits(state, request.duration_seconds, width, height)?;
    } else if request.duration_seconds > state.config.upload_max_duration_seconds {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video duration exceeds upload limit",
        ));
    }
    if request.upload_original {
        match request.source_size_bytes {
            Some(size) if size > 0 => {}
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "source_size_bytes must be greater than zero when upload_original is true",
                ))
            }
            None => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "source_size_bytes must be provided when upload_original is true",
                ))
            }
        }
    }

    let selected: Vec<_> = request
        .resolutions
        .iter()
        .filter_map(|resolution| {
            resolution_preset(resolution).map(|preset| (resolution.clone(), preset))
        })
        .collect();
    if selected.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    }

    let mut quote_cache = std::collections::HashMap::new();
    let mut variants = Vec::new();
    let mut total_storage_cost = 0_u128;
    let mut total_gas_cost = 0_u128;
    let mut total_bytes = 0_i64;
    let mut total_segments = 0_i64;
    let mut any_sampled = false;
    let mut original_file = None;

    for (resolution, (preset_width, preset_height, video_kbps, audio_kbps)) in selected {
        let (width, height) =
            target_dimensions_for_source(preset_width, preset_height, source_dimensions);
        let video_kbps =
            target_video_bitrate_kbps(video_kbps, preset_width, preset_height, width, height);
        let full_segments =
            (request.duration_seconds / state.config.hls_segment_duration).floor() as i64;
        let mut remainder =
            request.duration_seconds - (full_segments as f64 * state.config.hls_segment_duration);
        if remainder < 0.01 {
            remainder = 0.0;
        }
        let segment_count = full_segments + if remainder > 0.0 { 1 } else { 0 };
        let full_segment_bytes = estimate_transcoded_bytes(
            state
                .config
                .hls_segment_duration
                .min(request.duration_seconds),
            video_kbps,
            audio_kbps,
            state.config.upload_quote_transcoded_overhead,
        );
        let full_quote = quote_data_size(state, full_segment_bytes, &mut quote_cache).await?;

        let mut variant_storage_cost = full_quote.storage_cost_atto * full_segments as u128;
        let mut variant_gas_cost = full_quote.estimated_gas_cost_wei * full_segments as u128;
        let mut variant_bytes = full_segment_bytes * full_segments;
        let mut variant_chunks = full_quote.chunk_count * full_segments;
        any_sampled = any_sampled || full_quote.sampled;

        if remainder > 0.0 {
            let final_segment_bytes = estimate_transcoded_bytes(
                remainder,
                video_kbps,
                audio_kbps,
                state.config.upload_quote_transcoded_overhead,
            );
            let final_quote = quote_data_size(state, final_segment_bytes, &mut quote_cache).await?;
            variant_storage_cost += final_quote.storage_cost_atto;
            variant_gas_cost += final_quote.estimated_gas_cost_wei;
            variant_bytes += final_segment_bytes;
            variant_chunks += final_quote.chunk_count;
            any_sampled = any_sampled || final_quote.sampled;
        }

        variants.push(UploadQuoteVariantOut {
            resolution,
            width,
            height,
            segment_count,
            estimated_bytes: variant_bytes,
            chunk_count: variant_chunks,
            storage_cost_atto: variant_storage_cost.to_string(),
            estimated_gas_cost_wei: variant_gas_cost.to_string(),
            payment_mode: full_quote.payment_mode,
        });
        total_storage_cost += variant_storage_cost;
        total_gas_cost += variant_gas_cost;
        total_bytes += variant_bytes;
        total_segments += segment_count;
    }

    if request.upload_original {
        let source_size_bytes = request.source_size_bytes.unwrap_or_default();
        let quote = quote_data_size(state, source_size_bytes, &mut quote_cache).await?;
        total_storage_cost += quote.storage_cost_atto;
        total_gas_cost += quote.estimated_gas_cost_wei;
        total_bytes += source_size_bytes;
        any_sampled = any_sampled || quote.sampled;
        original_file = Some(UploadQuoteOriginalOut {
            estimated_bytes: source_size_bytes,
            chunk_count: quote.chunk_count,
            storage_cost_atto: quote.storage_cost_atto.to_string(),
            estimated_gas_cost_wei: quote.estimated_gas_cost_wei.to_string(),
            payment_mode: quote.payment_mode,
        });
    }

    let manifest_bytes = 4096 + (variants.len() as i64 * 1024) + (total_segments * 220);
    let catalog_bytes = 2048 + (variants.len() as i64 * 512);
    let metadata_quote =
        quote_data_size(state, manifest_bytes + catalog_bytes, &mut quote_cache).await?;
    total_storage_cost += metadata_quote.storage_cost_atto;
    total_gas_cost += metadata_quote.estimated_gas_cost_wei;
    total_bytes += manifest_bytes + catalog_bytes;
    any_sampled = any_sampled || metadata_quote.sampled;

    Ok(UploadQuoteOut {
        duration_seconds: request.duration_seconds,
        segment_duration: state.config.hls_segment_duration,
        payment_mode: state.config.antd_payment_mode.clone(),
        estimated_bytes: total_bytes,
        segment_count: total_segments,
        storage_cost_atto: total_storage_cost.to_string(),
        estimated_gas_cost_wei: total_gas_cost.to_string(),
        metadata_bytes: manifest_bytes + catalog_bytes,
        sampled: any_sampled,
        original_file,
        variants,
    })
}

#[cfg(test)]
mod tests {
    use super::{ceil_ratio_u128, parse_cost_u128, quote_sample_bytes};

    #[test]
    fn parses_quote_costs_above_i64_max() {
        let ten_ant_atto = "10000000000000000000";
        assert_eq!(
            parse_cost_u128(Some(ten_ant_atto)),
            10_000_000_000_000_000_000_u128
        );
        assert_eq!(parse_cost_u128(Some("-1")), 0);
        assert_eq!(parse_cost_u128(Some("not-a-number")), 0);
    }

    #[test]
    fn scales_sampled_quote_costs_without_signed_overflow() {
        let value = 10_000_000_000_000_000_000_u128;
        assert_eq!(
            ceil_ratio_u128(value, 3, 2),
            15_000_000_000_000_000_000_u128
        );
    }

    #[test]
    fn quote_sample_bytes_respects_autonomi_minimum() {
        assert_eq!(quote_sample_bytes(0, 16), None);
        assert_eq!(quote_sample_bytes(1, 16), Some(3));
        assert_eq!(quote_sample_bytes(2, 16), Some(3));
        assert_eq!(quote_sample_bytes(10, 16), Some(10));
        assert_eq!(quote_sample_bytes(100, 16), Some(16));
        assert_eq!(quote_sample_bytes(100, 1), Some(3));
    }
}
