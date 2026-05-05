use std::io::{Read, Write};
use std::path::Path as FsPath;

use ant_core::data::{Client as CoreClient, DataMap, PaymentMode};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;
use crate::state::AppState;

const CONTENT_SHA256_HEADER: &str = "x-content-sha256";

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    network: String,
    peer_count: usize,
}

#[derive(Serialize)]
struct WalletAddressResponse {
    address: String,
}

#[derive(Serialize)]
struct WalletBalanceResponse {
    balance: String,
    gas_balance: String,
}

#[derive(Serialize)]
struct WalletApproveResponse {
    approved: bool,
}

#[derive(Deserialize)]
struct DataRequest {
    data: String,
    #[serde(default)]
    payment_mode: Option<String>,
}

#[derive(Serialize)]
struct DataCostResponse {
    cost: String,
    file_size: u64,
    chunk_count: usize,
    estimated_gas_cost_wei: String,
    payment_mode: String,
}

#[derive(Serialize)]
struct DataPutResponse {
    address: String,
    chunks_stored: usize,
    payment_mode_used: String,
}

#[derive(Deserialize)]
struct FilePutQuery {
    #[serde(default)]
    payment_mode: Option<String>,
    #[serde(default)]
    verify: bool,
}

#[derive(Serialize)]
struct FilePutResponse {
    address: String,
    byte_size: u64,
    chunks_stored: usize,
    total_chunks: usize,
    chunks_failed: usize,
    storage_cost_atto: String,
    estimated_gas_cost_wei: String,
    payment_mode_used: String,
    verified: bool,
}

#[derive(Serialize)]
struct DataGetResponse {
    data: String,
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/wallet/address", get(wallet_address))
        .route("/v1/wallet/balance", get(wallet_balance))
        .route("/v1/wallet/approve", post(wallet_approve))
        .route("/v1/data/cost", post(data_cost))
        .route("/v1/data/public", post(data_put_public))
        .route("/v1/data/public/{address}", get(data_get_public))
        .route("/v1/file/public", post(file_put_public))
        .with_state(state)
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus("antd_service"),
    )
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let peer_count = state.client.network().connected_peers().await.len();
    Json(HealthResponse {
        status: "ok",
        network: state.network,
        peer_count,
    })
}

async fn wallet_address(
    State(state): State<AppState>,
) -> Result<Json<WalletAddressResponse>, ApiError> {
    let wallet = state
        .client
        .wallet()
        .ok_or_else(|| ApiError::wallet("wallet is not configured"))?;
    Ok(Json(WalletAddressResponse {
        address: format!("{:#x}", wallet.address()),
    }))
}

async fn wallet_balance(
    State(state): State<AppState>,
) -> Result<Json<WalletBalanceResponse>, ApiError> {
    let wallet = state
        .client
        .wallet()
        .ok_or_else(|| ApiError::wallet("wallet is not configured"))?;
    let balance = wallet
        .balance_of_tokens()
        .await
        .map_err(|err| ApiError::wallet(err.to_string()))?;
    let gas_balance = wallet
        .balance_of_gas_tokens()
        .await
        .map_err(|err| ApiError::wallet(err.to_string()))?;
    Ok(Json(WalletBalanceResponse {
        balance: balance.to_string(),
        gas_balance: gas_balance.to_string(),
    }))
}

async fn wallet_approve(
    State(state): State<AppState>,
) -> Result<Json<WalletApproveResponse>, ApiError> {
    state
        .client
        .approve_token_spend()
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    Ok(Json(WalletApproveResponse { approved: true }))
}

async fn data_cost(
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

async fn data_put_public(
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

async fn file_put_public(
    State(state): State<AppState>,
    Query(query): Query<FilePutQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<FilePutResponse>, ApiError> {
    let mode = parse_payment_mode(query.payment_mode.as_deref().unwrap_or("auto"))?;
    let expected_sha256 = headers
        .get(CONTENT_SHA256_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if expected_sha256
        .as_deref()
        .is_some_and(|value| value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return Err(ApiError::bad_request(format!(
            "{CONTENT_SHA256_HEADER} must be a lowercase or uppercase hex SHA-256 digest"
        )));
    }

    let file = NamedTempFile::new()?;
    let path = file.path().to_path_buf();
    let mut async_file = tokio::fs::File::from_std(file.reopen()?);
    let mut hasher = Sha256::new();
    let mut byte_size = 0_u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| ApiError::bad_request(format!("invalid body: {err}")))?;
        byte_size += chunk.len() as u64;
        hasher.update(&chunk);
        async_file.write_all(&chunk).await?;
    }
    async_file.flush().await?;
    drop(async_file);

    if byte_size < 3 {
        return Err(ApiError::bad_request(
            "file too small: self-encryption requires at least 3 bytes",
        ));
    }
    let computed_sha256 = hex::encode(hasher.finalize());
    if expected_sha256
        .as_deref()
        .is_some_and(|expected| expected != computed_sha256)
    {
        return Err(ApiError::bad_request(format!(
            "{CONTENT_SHA256_HEADER} did not match request body"
        )));
    }

    let result = state
        .client
        .file_upload_with_mode(&path, mode)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let address = state
        .client
        .data_map_store(&result.data_map)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;

    let mut verified = false;
    if query.verify {
        let verify_file = NamedTempFile::new()?;
        let verify_path = verify_file.path().to_path_buf();
        let downloaded = state
            .client
            .file_download(&result.data_map, &verify_path)
            .await
            .map_err(|err| ApiError::from_message(err.to_string()))?;
        let (verify_size, verify_sha256) = file_sha256(&verify_path)?;
        if downloaded != byte_size || verify_size != byte_size || verify_sha256 != computed_sha256 {
            return Err(ApiError::from_message(format!(
                "file verification mismatch: uploaded {byte_size} bytes sha256={computed_sha256}, downloaded {downloaded} bytes sha256={verify_sha256}"
            )));
        }
        verified = true;
    }

    tracing::info!(
        "Stored public file bytes={} chunks={} payment_mode={} verified={}",
        byte_size,
        result.chunks_stored,
        format_payment_mode(result.payment_mode_used),
        verified
    );

    // Keep the temp file alive until all upload and verification work is complete.
    file.close()?;

    Ok(Json(FilePutResponse {
        address: hex::encode(address),
        byte_size,
        chunks_stored: result.chunks_stored,
        total_chunks: result.total_chunks,
        chunks_failed: result.chunks_failed,
        storage_cost_atto: result.storage_cost_atto,
        estimated_gas_cost_wei: result.gas_cost_wei.to_string(),
        payment_mode_used: format_payment_mode(result.payment_mode_used),
        verified,
    }))
}

async fn data_get_public(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DataGetResponse>, ApiError> {
    let address = hex_to_address(&address)?;
    let data_map = state
        .client
        .data_map_fetch(&address)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let root_map = resolve_data_map(&state.client, data_map)?;
    let content = state
        .client
        .data_download(&root_map)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    Ok(Json(DataGetResponse {
        data: BASE64.encode(content),
    }))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, ApiError> {
    BASE64
        .decode(value)
        .map_err(|err| ApiError::bad_request(format!("invalid base64 data: {err}")))
}

fn file_sha256(path: &FsPath) -> Result<(u64, String), ApiError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut byte_size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        byte_size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((byte_size, hex::encode(hasher.finalize())))
}

fn parse_payment_mode(mode: &str) -> Result<PaymentMode, ApiError> {
    match mode.trim().to_lowercase().as_str() {
        "auto" => Ok(PaymentMode::Auto),
        "merkle" => Ok(PaymentMode::Merkle),
        "single" => Ok(PaymentMode::Single),
        other => Err(ApiError::bad_request(format!(
            "invalid payment_mode {other:?}; use auto, merkle, or single"
        ))),
    }
}

fn format_payment_mode(mode: PaymentMode) -> String {
    match mode {
        PaymentMode::Auto => "auto".to_string(),
        PaymentMode::Merkle => "merkle".to_string(),
        PaymentMode::Single => "single".to_string(),
    }
}

fn hex_to_address(value: &str) -> Result<[u8; 32], ApiError> {
    let bytes = hex::decode(value.trim())
        .map_err(|err| ApiError::bad_request(format!("invalid hex address: {err}")))?;
    bytes
        .try_into()
        .map_err(|_| ApiError::bad_request("address must be 32 bytes"))
}

fn resolve_data_map(inner: &CoreClient, data_map: DataMap) -> Result<DataMap, ApiError> {
    if !data_map.is_child() {
        return Ok(data_map);
    }

    let handle = tokio::runtime::Handle::current();
    tokio::task::block_in_place(|| {
        let fetch = |batch: &[(usize, xor_name::XorName)]| -> Result<
            Vec<(usize, bytes::Bytes)>,
            self_encryption::Error,
        > {
            let batch_owned: Vec<(usize, xor_name::XorName)> = batch.to_vec();
            handle.block_on(async {
                let mut results = Vec::with_capacity(batch_owned.len());
                for (idx, hash) in batch_owned {
                    let chunk = inner
                        .chunk_get(&hash.0)
                        .await
                        .map_err(|err| {
                            self_encryption::Error::Generic(format!(
                                "DataMap chunk_get failed: {err}"
                            ))
                        })?
                        .ok_or_else(|| {
                            self_encryption::Error::Generic(format!(
                                "DataMap chunk not found: {}",
                                hex::encode(hash.0)
                            ))
                        })?;
                    results.push((idx, chunk.content));
                }
                Ok(results)
            })
        };
        self_encryption::get_root_data_map_parallel(data_map, &fetch)
    })
    .map_err(|err| ApiError::from_message(format!("DataMap resolution failed: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payment_modes_case_insensitively() {
        assert!(matches!(
            parse_payment_mode("auto").unwrap(),
            PaymentMode::Auto
        ));
        assert!(matches!(
            parse_payment_mode("MERKLE").unwrap(),
            PaymentMode::Merkle
        ));
        assert!(matches!(
            parse_payment_mode(" single ").unwrap(),
            PaymentMode::Single
        ));
        assert!(parse_payment_mode("bad").is_err());
    }

    #[test]
    fn hashes_files_for_upload_verification() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"autvid").unwrap();
        let (size, digest) = file_sha256(file.path()).unwrap();
        assert_eq!(size, 6);
        assert_eq!(
            digest,
            "da51c62a769f30231ff3ac84fa522acccf38218551eb1a2a7a120011bf3d6e6a"
        );
    }
}
