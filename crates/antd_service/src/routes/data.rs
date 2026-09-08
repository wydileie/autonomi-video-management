use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::error::ApiError;
use crate::state::{AppState, CostCacheKey};

use super::shared::{decode_base64, parse_payment_mode};

#[derive(Deserialize)]
pub(super) struct DataRequest {
    data: String,
    #[serde(default)]
    payment_mode: Option<String>,
}

pub(crate) type DataCostResponse = crate::payments::ContentQuote;

#[derive(Serialize)]
pub(super) struct DataGetResponse {
    data: String,
}

pub(super) async fn data_cost(
    State(state): State<AppState>,
    Json(request): Json<DataRequest>,
) -> Result<Json<DataCostResponse>, ApiError> {
    let data = decode_base64(&request.data)?;
    let raw_payment_mode = request.payment_mode.as_deref().unwrap_or("auto");
    let mode = parse_payment_mode(raw_payment_mode)?;
    let payment_mode = raw_payment_mode.trim().to_ascii_lowercase();
    let cache_key = cost_cache_key(&data, &payment_mode);
    if let Some(cached) = state.cost_cache.get(&cache_key) {
        return Ok(Json(cached));
    }

    let file = NamedTempFile::new_in(&state.upload_temp_dir)?;
    tokio::fs::write(file.path(), data).await?;
    let response = state
        .payments
        .quote(&state.client, file.path(), mode)
        .await
        .map_err(|e| ApiError::from_autonomi_message(e.to_string()))?;
    state.cost_cache.insert(cache_key, response.clone());
    Ok(Json(response))
}

pub(super) async fn data_put_public(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DataRequest>,
) -> Result<Json<crate::payments::UploadReceipt>, ApiError> {
    let approval = crate::payments::approval_header(&headers)?;
    let lease = crate::payments::lease_header(&headers)?;
    let data = decode_base64(&request.data)?;
    let size = data.len() as u64;
    let sha256 = hex::encode(Sha256::digest(&data));
    let mode = parse_payment_mode(request.payment_mode.as_deref().unwrap_or("auto"))?;
    let file = NamedTempFile::new_in(&state.upload_temp_dir)?;
    tokio::fs::write(file.path(), data).await?;
    state
        .payments
        .upload(
            state.client.clone(),
            crate::payments::UploadRequest {
                file,
                approval,
                lease_key: lease,
                sha256,
                size,
                mode,
                verify: true,
            },
        )
        .await
        .map(Json)
        .map_err(crate::payments::payment_error)
}

pub(super) async fn data_get_public(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DataGetResponse>, ApiError> {
    let content = fetch_public_bytes(&state, &address).await?;
    Ok(public_data_json_response(&content))
}

pub(super) async fn data_get_public_raw(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let content = fetch_public_bytes(&state, &address).await?;
    Ok(public_data_raw_response(content))
}

async fn fetch_public_bytes(state: &AppState, address: &str) -> Result<Vec<u8>, ApiError> {
    let _permit = state
        .download_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "download capacity exhausted",
            )
        })?;
    let (data_map, _) = super::download::root_map(state, address, 32 * 1024 * 1024).await?;
    state
        .client
        .data_download(&data_map)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|err| ApiError::from_autonomi_message(err.to_string()))
}

fn public_data_json_response(content: &[u8]) -> Json<DataGetResponse> {
    Json(DataGetResponse {
        data: BASE64.encode(content),
    })
}

fn public_data_raw_response(
    content: Vec<u8>,
) -> ([(header::HeaderName, &'static str); 1], Vec<u8>) {
    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        content,
    )
}

fn cost_cache_key(data: &[u8], payment_mode: &str) -> CostCacheKey {
    let mut hasher = Sha256::new();
    hasher.update(data);
    CostCacheKey {
        sha256: hasher.finalize().into(),
        byte_len: data.len(),
        payment_mode: payment_mode.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use axum::body::to_bytes;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn raw_public_data_response_matches_json_payload_bytes() {
        let payload = b"autvid raw bytes \x00\x01\x02".to_vec();
        let json = public_data_json_response(&payload);
        let raw = public_data_raw_response(payload.clone()).into_response();

        assert_eq!(raw.status(), StatusCode::OK);
        assert_eq!(
            raw.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        let raw_body = to_bytes(raw.into_body(), usize::MAX).await.unwrap();
        assert_eq!(raw_body.as_ref(), payload.as_slice());
        assert_eq!(BASE64.decode(&json.0.data).unwrap(), payload);
    }
}
