# Bounded decompression patch

Base: `self_encryption 0.36.2`, verified published crate SHA-256:
`6a6c9e93b470f0fb747a609cfd7c4a23ea39bfbc1813de3d00fa429a9dc99e38`.
Upstream: https://github.com/WithAutonomi/self_encryption

The sole downstream source change is in `src/decrypt.rs`. Brotli writes through
a bounded writer that checks before extending its buffer. The bound is the
smaller of the caller's declared maximum and the protocol's `MAX_CHUNK_SIZE`.
This preserves the public decrypt API's protocol limit and also protects the
new upstream `DataMapIndex` path when a map declares an oversized chunk.
Formats, keys, addresses and valid chunk bytes are unchanged.
Maps made with a larger custom chunk maximum remain outside the gateway's
supported policy: its map validator already rejects chunks above this build's
`MAX_CHUNK_SIZE`. The indexed API now enforces that same limit.

Upstream 0.36.2 bounds `DataMapIndex` reads by the declared size, but its public
`decrypt_chunk` still passes `usize::MAX`. The local patch therefore remains
necessary. Retire it only when both paths have equivalent protection.
Rebase only from a verified published crate; retain license notices and review
the patch on each dependency upgrade. Before retiring it, test existing-address
reads and both declared-size and protocol-size boundary cases.

The vendored source retains upstream license notices and tests. Optional Python
bindings and upstream development dependencies are outside the application's
production dependency graph. The vendor is excluded from workspace membership.
A standalone vendor `Cargo.lock` may be generated for local upstream tests;
it is ignored and is not shipped or used by either application lockfile.

Validation: `cargo test --locked -p antd authenticated_chunk_expansion_is_bounded`
uses fixed independently encrypted fixtures through the public API and the
indexed API with an oversized declared size. The vendor
unit test checks exact limits and over-limit output; upstream DataMapIndex tests
cover declared lengths and hashes. Gateway validation separately limits map
depth, chunk counts, sizes and request concurrency.
