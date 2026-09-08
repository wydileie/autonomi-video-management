// Copyright 2021 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS"  BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

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
    let pki = get_pad_key_and_nonce(chunk_index, src_hashes, child_level)?;
    let (pad, key, nonce) = pki;

    // First remove the XOR obfuscation
    let xored = xor(content, &pad);

    // Then decrypt the content
    let decrypted = cipher::decrypt(xored, &key, &nonce)?;

    // Finally decompress
    let mut decompressed = BoundedChunk(Vec::new());
    let mut cursor = Cursor::new(&decrypted);

    brotli::BrotliDecompress(&mut cursor, &mut decompressed).map_err(|_| Error::Compression)?;

    Ok(Bytes::from(decompressed.0))
}

// AutVid downstream patch: reject oversized plaintext before extending a buffer.
// MAX_CHUNK_SIZE is the same limit used by the protocol's encryptor.
struct BoundedChunk(Vec<u8>);
impl Write for BoundedChunk {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > crate::MAX_CHUNK_SIZE.saturating_sub(self.0.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "decrypted chunk exceeds protocol maximum",
            ));
        }
        self.0.extend_from_slice(bytes);
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
    fn decompresses_legitimate_chunks_and_rejects_expansion_bombs() {
        for size in [3, crate::MAX_CHUNK_SIZE, crate::MAX_CHUNK_SIZE + 1] {
            let input = vec![0; size];
            let mut compressed = Vec::new();
            let _ = brotli::BrotliCompress(
                &mut Cursor::new(input),
                &mut compressed,
                &brotli::enc::BrotliEncoderParams::default(),
            )
            .expect("compress fixture");
            let mut output = BoundedChunk(Vec::new());
            let result = brotli::BrotliDecompress(&mut Cursor::new(compressed), &mut output);
            assert!(output.0.len() <= crate::MAX_CHUNK_SIZE);
            if size <= crate::MAX_CHUNK_SIZE {
                assert!(result.is_ok());
                assert_eq!(output.0.len(), size);
            } else {
                assert!(result.is_err());
            }
        }
    }
}
