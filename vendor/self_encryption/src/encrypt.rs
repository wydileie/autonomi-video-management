// Copyright 2021 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::{
    cipher::{self, Key, Nonce, Pad},
    error::Error,
    utils::xor,
    Result, COMPRESSION_QUALITY,
};
use brotli::enc::BrotliEncoderParams;
use bytes::Bytes;
use std::io::Cursor;

/// Encrypt a chunk
pub(crate) fn encrypt_chunk(content: Bytes, pki: (Pad, Key, Nonce)) -> Result<Bytes> {
    let (pad, key, nonce) = pki;

    let mut compressed = vec![];
    let enc_params = BrotliEncoderParams {
        quality: COMPRESSION_QUALITY,
        ..Default::default()
    };
    let _size = brotli::BrotliCompress(
        &mut Cursor::new(content.as_ref()),
        &mut compressed,
        &enc_params,
    )
    .map_err(|_| Error::Compression)?;
    let encrypted = cipher::encrypt(Bytes::from(compressed), &key, &nonce)?;
    Ok(xor(&encrypted, &pad))
}
