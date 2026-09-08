use xor_name::XorName;

/// Compute a BLAKE3 hash of the given content and return it as a XorName.
/// This replaces the SHA3-256 hashing previously done by `XorName::from_content`.
pub fn content_hash(content: &[u8]) -> XorName {
    let hash = blake3::hash(content);
    XorName(*hash.as_bytes())
}
