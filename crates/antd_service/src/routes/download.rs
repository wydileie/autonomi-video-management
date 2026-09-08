use super::shared::hex_to_address;
use crate::{error::ApiError, state::AppState};
use ant_core::data::DataMap;
use axum::{
    body::Body,
    extract::{Path, State},
    http::header,
    response::IntoResponse,
};
use futures_util::stream;
use std::{sync::Arc, time::Duration};

pub(super) fn validate_map(map: &DataMap, maximum: u64) -> Result<u64, ApiError> {
    if !(3..=65_536).contains(&map.len())
        || map.child().is_some_and(|depth| depth > 8 || depth == 0)
    {
        return Err(ApiError::bad_request(
            "invalid DataMap depth or chunk count",
        ));
    }
    let mut size = 0u64;
    for (index, info) in map.infos().iter().enumerate() {
        if info.index != index
            || info.src_size == 0
            || info.src_size > self_encryption::MAX_CHUNK_SIZE
        {
            return Err(ApiError::bad_request("invalid DataMap chunk index or size"));
        }
        size = size
            .checked_add(info.src_size as u64)
            .filter(|n| *n <= maximum)
            .ok_or_else(|| ApiError::bad_request("download exceeds size limit"))?;
    }
    Ok(size)
}

pub(super) async fn root_map(
    state: &AppState,
    address: &str,
    maximum: u64,
) -> Result<(DataMap, u64), ApiError> {
    let mut map = state
        .client
        .data_map_fetch(&hex_to_address(address)?)
        .await
        .map_err(|e| ApiError::from_autonomi_message(e.to_string()))?;
    while let Some(depth) = map.child() {
        // Resolve one wrapper at a time, checking lengths before allocating or
        // decrypting. The core receives only the validated root map afterwards.
        let size = validate_map(&map, 4 * 1024 * 1024)?;
        let hashes = Arc::new(map.infos().iter().map(|c| c.src_hash).collect::<Vec<_>>());
        let mut bytes = Vec::with_capacity(size as usize);
        for info in map.infos() {
            let chunk = state
                .client
                .chunk_get(&info.dst_hash.0)
                .await
                .map_err(|e| ApiError::from_autonomi_message(e.to_string()))?
                .ok_or_else(|| ApiError::bad_request("missing DataMap wrapper chunk"))?;
            if chunk.content.len() > 4 * 1024 * 1024
                || self_encryption::hash::content_hash(&chunk.content) != info.dst_hash
            {
                return Err(ApiError::bad_request("invalid DataMap wrapper chunk"));
            }
            let hashes = hashes.clone();
            let index = info.index;
            let plain = tokio::task::spawn_blocking(move || {
                self_encryption::decrypt_chunk(index, &chunk.content, &hashes, depth)
            })
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
            if plain.len() != info.src_size
                || self_encryption::hash::content_hash(&plain) != info.src_hash
            {
                return Err(ApiError::bad_request("invalid DataMap wrapper plaintext"));
            }
            bytes.extend_from_slice(&plain);
        }
        let parent =
            DataMap::from_bytes(&bytes).map_err(|e| ApiError::bad_request(e.to_string()))?;
        if parent.child().is_some_and(|next| next >= depth) {
            return Err(ApiError::bad_request(
                "DataMap wrapper depth did not decrease",
            ));
        }
        map = parent;
    }
    let size = validate_map(&map, maximum)?;
    Ok((map, size))
}

pub(super) async fn file_raw(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let permit = state
        .download_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "download capacity exhausted",
            )
        })?;
    let (map, size) = root_map(&state, &address, state.file_upload_max_bytes).await?;
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        let _permit = permit;
        let error_sender = sender.clone();
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            state.client.file_download_to_sender(&map, sender, None),
        )
        .await;
        let error = match result {
            Ok(Ok(downloaded)) if downloaded == size => None,
            Ok(Err(error)) => Some(error),
            _ => Some(ant_core::data::Error::Cancelled(
                "download deadline or size mismatch".into(),
            )),
        };
        if let Some(error) = error {
            let _ = error_sender.send(Err(error)).await;
        }
    });
    let chunks = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        Body::from_stream(chunks),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_hostile_map_sizes_indexes_and_depth_before_fetching() {
        let chunks = (0..3)
            .map(|index| self_encryption::ChunkInfo {
                index,
                src_hash: self_encryption::XorName([0; 32]),
                dst_hash: self_encryption::XorName([0; 32]),
                src_size: 1,
            })
            .collect();
        let mut map = DataMap::new(chunks);
        assert_eq!(validate_map(&map, 3).expect("valid"), 3);
        assert!(validate_map(&map, 2).is_err());
        map.chunk_identifiers[0].src_size = usize::MAX;
        assert!(validate_map(&map, u64::MAX).is_err());
        map.chunk_identifiers[0].src_size = 1;
        map.chunk_identifiers[2].index = usize::MAX;
        assert!(validate_map(&map, 3).is_err());
        map.chunk_identifiers[2].index = 2;
        map.child = Some(9);
        assert!(validate_map(&map, 3).is_err());
    }
}

#[cfg(test)]
mod decompression_guard_tests {
    #[test]
    fn authenticated_chunk_expansion_is_bounded() {
        // Fixed independently encrypted Brotli fixtures: all-zero source hashes,
        // chunk 0, child level 0, protocol v2 KDF/ChaCha20-Poly1305/XOR. They
        // decompress to 3 bytes, MAX_CHUNK_SIZE, and MAX_CHUNK_SIZE + 1 bytes.
        // Exercising the public API ensures the downstream bound is in use.
        let hashes = [self_encryption::XorName([0; 32]); 3];
        for (size, encoded) in [
            (3, "bee84d3d587900ecc869fb32acd3b0cb3450d2d4a25a32"),
            (
                4_190_208,
                "2e162202a05e03598dfd42206fd593c5cf29bcb83eab79c4975059ada1c7",
            ),
            (
                4_190_209,
                "2ee93d02a05e03598dfd22206fd5bc0612fbcde8f892ac5bc473b749c21a",
            ),
        ] {
            let encrypted = bytes::Bytes::from(hex::decode(encoded).expect("fixture"));
            let result = self_encryption::decrypt_chunk(0, &encrypted, &hashes, 0);
            if size <= self_encryption::MAX_CHUNK_SIZE {
                let plain = result.expect("valid bounded chunk");
                assert_eq!(plain.len(), size);
                assert!(plain.iter().all(|byte| *byte == 0));
            } else {
                assert!(matches!(result, Err(self_encryption::Error::Compression)));
            }
        }
    }
}
