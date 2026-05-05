use std::io::Read;
use std::path::Path as FsPath;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;
use crate::state::AppState;

use super::shared::{format_payment_mode, parse_payment_mode};

const CONTENT_SHA256_HEADER: &str = "x-content-sha256";

#[derive(Deserialize)]
pub(super) struct FilePutQuery {
    #[serde(default)]
    payment_mode: Option<String>,
    #[serde(default)]
    verify: bool,
}

#[derive(Serialize)]
pub(super) struct FilePutResponse {
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

pub(super) async fn file_put_public(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
