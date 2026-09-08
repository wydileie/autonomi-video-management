# Bounded decompression patch

Base: `self_encryption 0.36.0`, published crate checksum
`47ab904569f88dcbde4f0feadb693c184577dc81e8243f96bb725e72a779c637`.
Upstream: https://github.com/maidsafe/self_encryption

The sole source change is in `src/decrypt.rs`: Brotli writes through a bounded
writer which rejects plaintext above the encryptor's `MAX_CHUNK_SIZE`. It checks
before extending the buffer. Formats, keys, addresses and valid chunk bytes are
unchanged. The regression test covers a tiny chunk, the exact maximum and an
expansion above the maximum. Existing upstream tests remain included.

The workspace patches the registry version to this copy because the public
upstream API does not expose a bounded decompressor. Remove this copy when an
upstream release supplies an equivalent bound, after testing existing-address
reads and chunk boundary cases. Rebase only from a verified published crate;
retain license notices and review the patch on each dependency upgrade.

Validation: `cargo test --locked -p antd authenticated_chunk_expansion_is_bounded`
exercises fixed independently encrypted fixtures through the public decrypt API,
plus the
application's gateway round-trip tests. This bound covers each decrypted chunk;
gateway checks separately limit DataMap depth, chunk counts, sizes and concurrent
requests.

This vendored dependency is excluded from workspace membership. Its optional
Python bindings and upstream development dependencies are not part of the
application lockfile or production dependency graph. The application-level
regression runs in the gateway's normal test suite.
