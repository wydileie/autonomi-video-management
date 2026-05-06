use std::sync::Arc;

use ant_core::data::{Client as CoreClient, DataMap, PaymentMode};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::error::ApiError;

pub(super) fn decode_base64(value: &str) -> Result<Vec<u8>, ApiError> {
    BASE64
        .decode(value)
        .map_err(|err| ApiError::bad_request(format!("invalid base64 data: {err}")))
}

pub(super) fn parse_payment_mode(mode: &str) -> Result<PaymentMode, ApiError> {
    match mode.trim().to_lowercase().as_str() {
        "auto" => Ok(PaymentMode::Auto),
        "merkle" => Ok(PaymentMode::Merkle),
        "single" => Ok(PaymentMode::Single),
        other => Err(ApiError::bad_request(format!(
            "invalid payment_mode {other:?}; use auto, merkle, or single"
        ))),
    }
}

pub(super) fn format_payment_mode(mode: PaymentMode) -> String {
    match mode {
        PaymentMode::Auto => "auto".to_string(),
        PaymentMode::Merkle => "merkle".to_string(),
        PaymentMode::Single => "single".to_string(),
    }
}

pub(super) fn hex_to_address(value: &str) -> Result<[u8; 32], ApiError> {
    let bytes = hex::decode(value.trim())
        .map_err(|err| ApiError::bad_request(format!("invalid hex address: {err}")))?;
    bytes
        .try_into()
        .map_err(|_| ApiError::bad_request("address must be 32 bytes"))
}

pub(super) async fn resolve_data_map(
    inner: Arc<CoreClient>,
    data_map: DataMap,
) -> Result<DataMap, ApiError> {
    if !data_map.is_child() {
        return Ok(data_map);
    }

    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let fetch =
            |batch: &[(usize, xor_name::XorName)]| -> Result<
                Vec<(usize, bytes::Bytes)>,
                self_encryption::Error,
            > {
                let batch_owned: Vec<(usize, xor_name::XorName)> = batch.to_vec();
                let inner = inner.clone();
                handle.block_on(async move {
                    let mut results = Vec::with_capacity(batch_owned.len());
                    for (idx, hash) in batch_owned {
                        let chunk = inner.chunk_get(&hash.0).await.map_err(|err| {
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
    .await
    .map_err(|err| ApiError::from_message(format!("DataMap resolution task failed: {err}")))?
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
    fn decodes_base64_payloads_and_rejects_invalid_input() {
        assert_eq!(decode_base64("YXV0dmlk").unwrap(), b"autvid");
        assert!(decode_base64("%%%").is_err());
    }

    #[test]
    fn parses_32_byte_hex_addresses_only() {
        let address =
            hex_to_address("abababababababababababababababababababababababababababababababab")
                .unwrap();
        assert_eq!(address, [0xab; 32]);
        assert!(hex_to_address("ab").is_err());
        assert!(hex_to_address("not-hex").is_err());
    }
}
