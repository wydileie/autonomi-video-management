// Copyright 2021 MaidSafe.net limited.
//
// This Autonomi Software is licensed under the MIT license <LICENSE-MIT or
// https://opensource.org/licenses/MIT> or the Apache License, Version 2.0
// <LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0>, at your
// option. This file may not be copied, modified, or distributed except
// according to those terms.

use crate::{cipher, utils::get_pad_key_and_nonce, utils::xor, EncryptedChunk, Error, Result};
use bytes::Bytes;
use rayon::prelude::*;
use std::io::{Cursor, Write};
use xor_name::XorName;

// encrypted_Chunks are sorted !!
pub fn decrypt_sorted_set(
    src_hashes: Vec<XorName>,
    encrypted_chunks: &[&EncryptedChunk],
    child_level: usize,
) -> Result<Bytes> {
    // Decrypt chunks in parallel, then concatenate in order
    let decrypted_chunks: Vec<Bytes> = encrypted_chunks
        .par_iter()
        .enumerate()
        .map(|(chunk_index, chunk)| {
            decrypt_chunk(chunk_index, &chunk.content, &src_hashes, child_level)
        })
        .collect::<Result<Vec<_>>>()?;

    let total_len = decrypted_chunks.iter().map(|c| c.len()).sum();
    let mut all_bytes = Vec::with_capacity(total_len);
    for chunk in &decrypted_chunks {
        all_bytes.extend_from_slice(chunk);
    }

    Ok(Bytes::from(all_bytes))
}

/// Decrypt a chunk, given the index of that chunk in the sequence of chunks,
/// and the raw encrypted content.
pub fn decrypt_chunk(
    chunk_index: usize,
    content: &Bytes,
    src_hashes: &[XorName],
    child_level: usize,
) -> Result<Bytes> {
    decrypt_chunk_bounded(chunk_index, content, src_hashes, child_level, usize::MAX)
}

/// As [`decrypt_chunk`], but fails rather than decompress more than
/// `max_size` bytes.
pub(crate) fn decrypt_chunk_bounded(
    chunk_index: usize,
    content: &Bytes,
    src_hashes: &[XorName],
    child_level: usize,
    max_size: usize,
) -> Result<Bytes> {
    let pki = get_pad_key_and_nonce(chunk_index, src_hashes, child_level)?;
    let (pad, key, nonce) = pki;

    // First remove the XOR obfuscation
    let xored = xor(content, &pad);

    // Then decrypt the content
    let decrypted = cipher::decrypt(xored, &key, &nonce)?;

    // Enforce both the caller's bound and the protocol maximum before allocation.
    let mut decompressed = BoundedChunk {
        bytes: Vec::new(),
        max_size: max_size.min(crate::MAX_CHUNK_SIZE),
    };
    brotli::BrotliDecompress(&mut Cursor::new(&decrypted), &mut decompressed)
        .map_err(|_| Error::Compression)?;
    Ok(Bytes::from(decompressed.bytes))
}

// AutVid downstream patch: reject oversized plaintext before extending a buffer.
struct BoundedChunk {
    bytes: Vec<u8>,
    max_size: usize,
}

impl Write for BoundedChunk {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max_size.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decrypted chunk exceeds maximum",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    #[test]
    fn decompression_enforces_declared_and_protocol_limits() {
        for limit in [3, crate::MAX_CHUNK_SIZE] {
            for size in [limit, limit + 1] {
                let mut compressed = Vec::new();
                let _ = brotli::BrotliCompress(
                    &mut Cursor::new(vec![0; size]),
                    &mut compressed,
                    &brotli::enc::BrotliEncoderParams::default(),
                )
                .expect("compress fixture");
                let mut output = BoundedChunk {
                    bytes: Vec::new(),
                    max_size: limit,
                };
                let result = brotli::BrotliDecompress(&mut Cursor::new(compressed), &mut output);
                assert!(output.bytes.len() <= limit);
                if size <= limit {
                    assert!(result.is_ok());
                    assert_eq!(output.bytes.len(), size);
                } else {
                    assert!(result.is_err());
                }
            }
        }
    }
}
