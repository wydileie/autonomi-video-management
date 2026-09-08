use crate::cipher::{Key, Nonce, Pad, KEY_SIZE, NONCE_SIZE, PAD_SIZE};
use bytes::Bytes;
use xor_name::XorName;

/// Helper function to XOR a data with a pad (pad will be rotated to fill the length)
pub(crate) fn xor(data: &Bytes, &Pad(pad): &Pad) -> Bytes {
    let vec: Vec<_> = data
        .iter()
        .zip(pad.iter().cycle())
        .map(|(&a, &b)| a ^ b)
        .collect();
    Bytes::from(vec)
}

pub fn extract_hashes(data_map: &crate::DataMap) -> Vec<XorName> {
    data_map.infos().iter().map(|c| c.src_hash).collect()
}

pub(crate) fn get_pad_key_and_nonce(
    chunk_index: usize,
    chunk_hashes: &[XorName],
    child_level: usize,
) -> crate::Result<(Pad, Key, Nonce)> {
    let (n_1, n_2) = get_n_1_n_2(chunk_index, chunk_hashes.len())?;

    let src_hash = chunk_hashes
        .get(chunk_index)
        .ok_or_else(|| crate::Error::Generic(format!("chunk_index {chunk_index} out of bounds")))?;
    let n_1_src_hash = chunk_hashes
        .get(n_1)
        .ok_or_else(|| crate::Error::Generic(format!("n_1 index {n_1} out of bounds")))?;
    let n_2_src_hash = chunk_hashes
        .get(n_2)
        .ok_or_else(|| crate::Error::Generic(format!("n_2 index {n_2} out of bounds")))?;

    Ok(get_pki(
        src_hash,
        n_1_src_hash,
        n_2_src_hash,
        chunk_index,
        child_level,
    ))
}

pub(crate) fn get_n_1_n_2(
    chunk_index: usize,
    total_num_chunks: usize,
) -> crate::Result<(usize, usize)> {
    if total_num_chunks < 3 {
        return Err(crate::Error::Generic(format!(
            "total_num_chunks must be at least 3, got {total_num_chunks}"
        )));
    }
    if chunk_index >= total_num_chunks {
        return Err(crate::Error::Generic(format!(
            "chunk_index {chunk_index} out of bounds for total_num_chunks {total_num_chunks}"
        )));
    }
    match chunk_index {
        0 => Ok((total_num_chunks - 1, total_num_chunks - 2)),
        1 => Ok((0, total_num_chunks - 1)),
        n => Ok((n - 1, n - 2)),
    }
}

pub(crate) fn get_pki(
    src_hash: &XorName,
    n_1_src_hash: &XorName,
    n_2_src_hash: &XorName,
    chunk_index: usize,
    child_level: usize,
) -> (Pad, Key, Nonce) {
    // Domain-separated BLAKE3 KDF with full chunk context.
    // Including src_hash ensures that two different chunks sharing the same
    // predecessors (n_1, n_2) will derive different (key, nonce) pairs,
    // eliminating the nonce-reuse hazard in ChaCha20-Poly1305.
    let mut context_material = Vec::with_capacity(32 + 32 + 32 + 8 + 8);
    context_material.extend_from_slice(&src_hash.0);
    context_material.extend_from_slice(&n_1_src_hash.0);
    context_material.extend_from_slice(&n_2_src_hash.0);
    context_material.extend_from_slice(&(chunk_index as u64).to_le_bytes());
    context_material.extend_from_slice(&(child_level as u64).to_le_bytes());

    let mut output = [0u8; PAD_SIZE + KEY_SIZE + NONCE_SIZE];
    let mut hasher = blake3::Hasher::new_derive_key("self_encryption/chunk/v2");
    let _ = hasher.update(&context_material);
    let mut output_reader = hasher.finalize_xof();
    output_reader.fill(&mut output);

    let mut pad = [0u8; PAD_SIZE];
    let mut key = [0u8; KEY_SIZE];
    let mut nonce = [0u8; NONCE_SIZE];

    pad.copy_from_slice(&output[..PAD_SIZE]);
    key.copy_from_slice(&output[PAD_SIZE..PAD_SIZE + KEY_SIZE]);
    nonce.copy_from_slice(&output[PAD_SIZE + KEY_SIZE..]);

    (Pad(pad), Key(key), Nonce(nonce))
}

// Returns the number of chunks according to file size.
pub(crate) fn get_num_chunks(file_size: usize) -> usize {
    get_num_chunks_with_variable_max(file_size, crate::MAX_CHUNK_SIZE)
}

// Returns the number of chunks according to file size.
pub(crate) fn get_num_chunks_with_variable_max(file_size: usize, max_chunk_size: usize) -> usize {
    if file_size < (3 * crate::MIN_CHUNK_SIZE) {
        return 0;
    }
    if file_size < (3 * max_chunk_size) {
        return 3;
    }
    if file_size.is_multiple_of(max_chunk_size) {
        file_size / max_chunk_size
    } else {
        (file_size / max_chunk_size) + 1
    }
}

// Returns the size of a chunk according to file size.
pub(crate) fn get_chunk_size(file_size: usize, chunk_index: usize) -> usize {
    get_chunk_size_with_variable_max(file_size, chunk_index, crate::MAX_CHUNK_SIZE)
}

// Returns the size of a chunk according to file size.
pub(crate) fn get_chunk_size_with_variable_max(
    file_size: usize,
    chunk_index: usize,
    max_chunk_size: usize,
) -> usize {
    if file_size < 3 * crate::MIN_CHUNK_SIZE {
        return 0;
    }
    if file_size < 3 * max_chunk_size {
        if chunk_index < 2 {
            return file_size / 3;
        } else {
            // When the file_size % 3 > 0, the third (last) chunk includes the remainder
            return file_size - (2 * (file_size / 3));
        }
    }
    let total_chunks = get_num_chunks_with_variable_max(file_size, max_chunk_size);
    if chunk_index < total_chunks - 2 {
        return max_chunk_size;
    }
    let remainder = file_size % max_chunk_size;
    let penultimate = (total_chunks - 2) == chunk_index;
    if remainder == 0 {
        return max_chunk_size;
    }
    if remainder < crate::MIN_CHUNK_SIZE {
        if penultimate {
            max_chunk_size - crate::MIN_CHUNK_SIZE
        } else {
            crate::MIN_CHUNK_SIZE + remainder
        }
    } else if penultimate {
        max_chunk_size
    } else {
        remainder
    }
}

// Returns the [start, end) half-open byte range of a chunk.
pub(crate) fn get_start_end_positions(file_size: usize, chunk_index: usize) -> (usize, usize) {
    if get_num_chunks(file_size) == 0 {
        return (0, 0);
    }
    let start = get_start_position(file_size, chunk_index);
    (start, start + get_chunk_size(file_size, chunk_index))
}

pub(crate) fn get_start_position(file_size: usize, chunk_index: usize) -> usize {
    let total_chunks = get_num_chunks(file_size);
    if total_chunks == 0 {
        return 0;
    }
    let last = (total_chunks - 1) == chunk_index;
    let first_chunk_size = get_chunk_size(file_size, 0);
    if last {
        first_chunk_size * (chunk_index - 1) + get_chunk_size(file_size, chunk_index - 1)
    } else {
        first_chunk_size * chunk_index
    }
}

#[allow(dead_code)]
pub(crate) fn get_chunk_index(file_size: usize, position: usize) -> usize {
    let num_chunks = get_num_chunks(file_size);
    if num_chunks == 0 {
        return 0; // FIX THIS SHOULD NOT BE ALLOWED
    }

    let chunk_size = get_chunk_size(file_size, 0);
    let remainder = file_size % chunk_size;

    if remainder == 0
        || remainder >= crate::MIN_CHUNK_SIZE
        || position < file_size - remainder - crate::MIN_CHUNK_SIZE
    {
        usize::min(position / chunk_size, num_chunks - 1)
    } else {
        num_chunks - 1
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_different_chunks_same_predecessors_yield_different_key_nonce() {
        let n1 = XorName([0xAA; 32]);
        let n2 = XorName([0xBB; 32]);

        let src_hash_a = XorName([0x01; 32]);
        let src_hash_b = XorName([0x02; 32]);

        let (pad_a, key_a, nonce_a) = get_pki(&src_hash_a, &n1, &n2, 0, 0);
        let (pad_b, key_b, nonce_b) = get_pki(&src_hash_b, &n1, &n2, 0, 0);

        assert_ne!(
            key_a.0, key_b.0,
            "different src_hash must produce different keys"
        );
        assert_ne!(
            nonce_a.0, nonce_b.0,
            "different src_hash must produce different nonces"
        );
        assert_ne!(
            pad_a.0, pad_b.0,
            "different src_hash must produce different pads"
        );
    }

    #[test]
    fn test_kdf_domain_separation_by_chunk_index() {
        let src = XorName([0x11; 32]);
        let n1 = XorName([0x22; 32]);
        let n2 = XorName([0x33; 32]);

        let (_, key_0, nonce_0) = get_pki(&src, &n1, &n2, 0, 0);
        let (_, key_1, nonce_1) = get_pki(&src, &n1, &n2, 1, 0);

        assert_ne!(
            key_0.0, key_1.0,
            "different chunk_index must produce different keys"
        );
        assert_ne!(
            nonce_0.0, nonce_1.0,
            "different chunk_index must produce different nonces"
        );
    }

    #[test]
    fn test_kdf_domain_separation_by_child_level() {
        let src = XorName([0x11; 32]);
        let n1 = XorName([0x22; 32]);
        let n2 = XorName([0x33; 32]);

        let (_, key_c0, nonce_c0) = get_pki(&src, &n1, &n2, 0, 0);
        let (_, key_c1, nonce_c1) = get_pki(&src, &n1, &n2, 0, 1);

        assert_ne!(
            key_c0.0, key_c1.0,
            "different child_level must produce different keys"
        );
        assert_ne!(
            nonce_c0.0, nonce_c1.0,
            "different child_level must produce different nonces"
        );
    }

    #[test]
    fn test_kdf_deterministic() {
        let src = XorName([0x42; 32]);
        let n1 = XorName([0xAA; 32]);
        let n2 = XorName([0xBB; 32]);

        let (pad_a, key_a, nonce_a) = get_pki(&src, &n1, &n2, 5, 2);
        let (pad_b, key_b, nonce_b) = get_pki(&src, &n1, &n2, 5, 2);

        assert_eq!(pad_a.0, pad_b.0, "same inputs must produce identical pads");
        assert_eq!(key_a.0, key_b.0, "same inputs must produce identical keys");
        assert_eq!(
            nonce_a.0, nonce_b.0,
            "same inputs must produce identical nonces"
        );
    }

    #[test]
    fn test_kdf_avalanche_single_bit_flip() {
        let src_a = [0x42u8; 32];
        let src_b = {
            let mut b = src_a;
            b[0] ^= 0x01; // flip one bit
            b
        };

        let n1 = XorName([0xAA; 32]);
        let n2 = XorName([0xBB; 32]);

        let (pad_a, key_a, nonce_a) = get_pki(&XorName(src_a), &n1, &n2, 0, 0);
        let (pad_b, key_b, nonce_b) = get_pki(&XorName(src_b), &n1, &n2, 0, 0);

        // Count differing bytes — expect at least 30% differ (avalanche property)
        let key_diff = key_a
            .0
            .iter()
            .zip(key_b.0.iter())
            .filter(|(a, b)| a != b)
            .count();
        let nonce_diff = nonce_a
            .0
            .iter()
            .zip(nonce_b.0.iter())
            .filter(|(a, b)| a != b)
            .count();
        let pad_diff = pad_a
            .0
            .iter()
            .zip(pad_b.0.iter())
            .filter(|(a, b)| a != b)
            .count();

        assert!(
            key_diff >= 10,
            "expected at least 10/32 key bytes to differ, got {}",
            key_diff
        );
        assert!(
            nonce_diff >= 4,
            "expected at least 4/12 nonce bytes to differ, got {}",
            nonce_diff
        );
        assert!(
            pad_diff >= 15,
            "expected at least 15/52 pad bytes to differ, got {}",
            pad_diff
        );
    }

    #[test]
    fn test_get_n_1_n_2_out_of_bounds() {
        let result = get_n_1_n_2(5, 3);
        assert!(result.is_err(), "chunk_index >= total should fail");
        let result = get_n_1_n_2(3, 3);
        assert!(result.is_err(), "chunk_index == total should fail");
        let result = get_n_1_n_2(2, 3);
        assert!(result.is_ok(), "chunk_index < total should succeed");
    }

    #[test]
    fn test_kdf_output_sizes() {
        let src = XorName([0x01; 32]);
        let n1 = XorName([0x02; 32]);
        let n2 = XorName([0x03; 32]);

        let (pad, key, nonce) = get_pki(&src, &n1, &n2, 0, 0);

        assert_eq!(pad.0.len(), PAD_SIZE, "pad must be {PAD_SIZE} bytes");
        assert_eq!(key.0.len(), KEY_SIZE, "key must be {KEY_SIZE} bytes");
        assert_eq!(
            nonce.0.len(),
            NONCE_SIZE,
            "nonce must be {NONCE_SIZE} bytes"
        );
    }
}
