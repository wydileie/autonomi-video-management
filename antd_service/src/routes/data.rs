use axum::extract::{Path, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::io::Write;
use tempfile::NamedTempFile;

use crate::error::ApiError;
use crate::state::AppState;

use super::shared::{
    decode_base64, format_payment_mode, hex_to_address, parse_payment_mode, resolve_data_map,
};

#[derive(Deserialize)]
pub(super) struct DataRequest {
    data: String,
    #[serde(default)]
    payment_mode: Option<String>,
}

#[derive(Serialize)]
pub(super) struct DataCostResponse {
    cost: String,
    file_size: u64,
    chunk_count: usize,
    estimated_gas_cost_wei: String,
    payment_mode: String,
}

#[derive(Serialize)]
pub(super) struct DataPutResponse {
    address: String,
    chunks_stored: usize,
    payment_mode_used: String,
}

#[derive(Serialize)]
pub(super) struct DataGetResponse {
    data: String,
}

pub(super) async fn data_cost(
    State(state): State<AppState>,
    Json(request): Json<DataRequest>,
) -> Result<Json<DataCostResponse>, ApiError> {
    let data = decode_base64(&request.data)?;
    let mode = parse_payment_mode(request.payment_mode.as_deref().unwrap_or("auto"))?;

    let mut file = NamedTempFile::new()?;
    file.write_all(&data)?;

    let estimate = state
        .client
        .estimate_upload_cost(file.path(), mode, None)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;

    Ok(Json(DataCostResponse {
        cost: estimate.storage_cost_atto,
        file_size: estimate.file_size,
        chunk_count: estimate.chunk_count,
        estimated_gas_cost_wei: estimate.estimated_gas_cost_wei,
        payment_mode: format_payment_mode(estimate.payment_mode),
    }))
}

pub(super) async fn data_put_public(
    State(state): State<AppState>,
    Json(request): Json<DataRequest>,
) -> Result<Json<DataPutResponse>, ApiError> {
    let data = decode_base64(&request.data)?;
    let mode = parse_payment_mode(request.payment_mode.as_deref().unwrap_or("auto"))?;

    let result = state
        .client
        .data_upload_with_mode(Bytes::from(data), mode)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let address = state
        .client
        .data_map_store(&result.data_map)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;

    Ok(Json(DataPutResponse {
        address: hex::encode(address),
        chunks_stored: result.chunks_stored,
        payment_mode_used: format_payment_mode(result.payment_mode_used),
    }))
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
    let address = hex_to_address(address)?;
    let data_map = state
        .client
        .data_map_fetch(&address)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let root_map = resolve_data_map(state.client.clone(), data_map).await?;
    state
        .client
        .data_download(&root_map)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|err| ApiError::from_message(err.to_string()))
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

#[cfg(test)]
mod tests {
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
