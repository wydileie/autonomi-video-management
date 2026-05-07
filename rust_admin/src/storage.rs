use std::time::Duration as StdDuration;

use axum::http::StatusCode;
use serde::Serialize;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    antd_client::{is_retryable_antd_error, jitter_duration, AntdDataPutResponse, AntdRestClient},
    errors::ApiError,
    state::AppState,
    upload::format_bytes,
};

pub(crate) async fn store_json_public<T: Serialize + ?Sized>(
    state: &AppState,
    payload: &T,
) -> Result<String, ApiError> {
    let data = serde_json::to_vec(payload).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not encode JSON document: {err}"),
        )
    })?;
    let result = put_public_verified_with_mode(
        state,
        &data,
        "json document",
        &state.config.antd_metadata_payment_mode,
    )
    .await?;
    Ok(result.address)
}

pub(crate) async fn put_public_verified_with_mode(
    state: &AppState,
    data: &[u8],
    label: &str,
    payment_mode: &str,
) -> Result<AntdDataPutResponse, ApiError> {
    if data.len() > state.config.antd_direct_upload_max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Direct JSON upload for {label} is {} but ANTD_DIRECT_UPLOAD_MAX_BYTES is {}; media uploads must use the streaming file endpoint",
                format_bytes(data.len() as u64),
                format_bytes(state.config.antd_direct_upload_max_bytes as u64)
            ),
        ));
    }
    put_public_verified_inner(
        state.antd.clone(),
        payment_mode.to_string(),
        state.config.antd_upload_verify,
        state.config.antd_upload_retries,
        data.to_vec(),
        label.to_string(),
    )
    .await
    .map_err(|err| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err))
}

pub(crate) async fn put_public_verified_inner(
    antd: AntdRestClient,
    payment_mode: String,
    upload_verify: bool,
    upload_retries: usize,
    data: Vec<u8>,
    label: String,
) -> Result<AntdDataPutResponse, String> {
    let mut last_error = None;
    for attempt in 1..=upload_retries {
        info!(
            "Uploading {} ({} bytes), attempt {}/{}",
            label,
            data.len(),
            attempt,
            upload_retries
        );
        let result = antd.data_put_public(&data, &payment_mode).await;
        match result {
            Ok(result) => {
                if upload_verify {
                    match antd.data_get_public(&result.address).await {
                        Ok(retrieved) if retrieved == data.as_slice() => return Ok(result),
                        Ok(retrieved) => {
                            last_error = Some(format!(
                                "Autonomi verification mismatch for {label}: stored {} bytes, retrieved {} bytes",
                                data.len(),
                                retrieved.len()
                            ));
                        }
                        Err(err) => last_error = Some(err.to_string()),
                    }
                } else {
                    return Ok(result);
                }
            }
            Err(err) => {
                let retryable = is_retryable_antd_error(&err);
                last_error = Some(err.to_string());
                if !retryable {
                    break;
                }
            }
        }

        if attempt < upload_retries {
            antd.record_upload_retry();
            let delay = jitter_duration(StdDuration::from_secs(
                2_u64.pow((attempt - 1).min(3) as u32),
            ));
            warn!(
                "Autonomi upload verification failed for {} on attempt {}/{}: {}; retrying in {}ms",
                label,
                attempt,
                upload_retries,
                last_error.as_deref().unwrap_or("unknown error"),
                delay.as_millis()
            );
            sleep(delay).await;
        }
    }

    Err(format!(
        "Autonomi upload failed verification for {} after {} attempt(s): {}",
        label,
        upload_retries,
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}
