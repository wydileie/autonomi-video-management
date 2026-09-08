use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;
use crate::state::AppState;

use super::shared::parse_payment_mode;
use crate::payments::{approval_header, payment_error, ContentQuote, UploadReceipt};

const CONTENT_SHA256_HEADER: &str = "x-content-sha256";

#[derive(Deserialize)]
pub(super) struct FilePutQuery {
    #[serde(default)]
    payment_mode: Option<String>,
    #[serde(default)]
    verify: bool,
}

pub(super) async fn file_put_public(
    State(state): State<AppState>,
    Query(query): Query<FilePutQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<UploadReceipt>, ApiError> {
    let mode = parse_payment_mode(query.payment_mode.as_deref().unwrap_or("auto"))?;
    let approval = approval_header(&headers)?;
    let lease = crate::payments::lease_header(&headers)?;
    let (file, byte_size, computed_sha256) = receive_file(&state, headers, body).await?;
    state
        .payments
        .upload(
            state.client.clone(),
            crate::payments::UploadRequest {
                file,
                approval,
                lease_key: lease,
                sha256: computed_sha256,
                size: byte_size,
                mode,
                verify: query.verify,
            },
        )
        .await
        .map(Json)
        .map_err(payment_error)
}

pub(super) async fn file_cost(
    State(state): State<AppState>,
    Query(query): Query<FilePutQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<ContentQuote>, ApiError> {
    let mode = parse_payment_mode(query.payment_mode.as_deref().unwrap_or("auto"))?;
    let (file, _, _) = receive_file(&state, headers, body).await?;
    state
        .payments
        .quote(&state.client, file.path(), mode)
        .await
        .map(Json)
        .map_err(|e| ApiError::from_autonomi_message(e.to_string()))
}

async fn receive_file(
    state: &AppState,
    headers: HeaderMap,
    body: Body,
) -> Result<(NamedTempFile, u64, String), ApiError> {
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

    let file = NamedTempFile::new_in(&state.upload_temp_dir)?;
    let mut async_file = tokio::fs::File::from_std(file.reopen()?);
    let mut hasher = Sha256::new();
    let mut byte_size = 0_u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| ApiError::bad_request(format!("invalid body: {err}")))?;
        byte_size += chunk.len() as u64;
        if byte_size > state.file_upload_max_bytes {
            return Err(ApiError::bad_request(format!(
                "file exceeds ANTD_FILE_UPLOAD_MAX_BYTES ({})",
                state.file_upload_max_bytes
            )));
        }
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

    Ok((file, byte_size, computed_sha256))
}
