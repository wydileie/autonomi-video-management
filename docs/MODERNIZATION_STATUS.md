# Modernization implementation and validation

Implemented on `codex/security-modernization`, based on `1b94920` and the
approved September 8, 2026 plan. This is an implementation record; production
promotion still requires the release gates below.

## Security and correctness

- Native services and devbench host ports bind to loopback. Compose explicitly
  selects container interfaces. Native Host/Origin checks, proxy-header filtering,
  private app-data permissions, and separate generated read/write gateway
  credentials protect direct service access.
- Admin JSON bodies are capped at 64 KiB. Protected routes authenticate and
  check CSRF before extraction; only file uploads receive streaming exceptions.
  Service-level login throttling applies independently of Nginx.
- Manifests validate counts, indices, lengths, and structure before indexing.
  Segment indexing uses memory proportional to actual segments. Parsed metadata
  caches, buffered downloads, upload/download concurrency, and network fetches
  have explicit bounds. Shared segment fetches survive caller cancellation and
  clean up their own entries safely.
- Network DataMap wrappers and decrypted chunk lengths are validated. A narrow,
  protocol-compatible `self_encryption` patch rejects oversized Brotli output
  before growing the output buffer; provenance is in the vendor patch note.
- Jobs renew leases and fence database publication/payment effects with owner
  and generation. Recovery preserves active leases and reschedules interrupted
  quoting work. Stale workers cannot publish success or start another payment.
- FFmpeg/probe input protocols, formats, output capture, and execution time are
  bounded. Heavy hashing/decompression runs outside async executor threads.
- Desktop CSP and Tauri-to-loopback navigation were exercised. Quit handling
  now shuts down sidecars, including application-level exit requests.
- PR review fixes keep empty-catalog approval controls available, prioritize
  payment recovery over nested approval errors, and materialize catalog files
  only after the authoritative database snapshot commits. Materialization
  reloads the latest committed snapshot while holding SQLite's writer lock.

## Autonomi and spending approvals

- The custom application gateway remains the runtime integration. Core 0.8.1,
  protocol 2.3.5, saorsa-core 0.27.3, transport 0.36.3, and evmlib 0.9.1 are locked
  together. Node/devnet 0.18.1, SDK tooling 0.12.1, and CLI 0.3.6 are aligned.
- Gateway configuration fails on explicit invalid wallet/EVM settings. Upstream
  peer recovery/cache APIs and actual read/write readiness are integrated.
  Health includes protocol `autvid-gateway-v2`; admin rejects the SDK tooling
  daemon as an application gateway. Optional SDK tooling uses port 8182.
- Executable quotes prepare actual media plus originals, public DataMaps,
  manifests, and both catalog snapshots. Approval binds content, network/payer,
  payment mode, expiry, quote ID, and separate aggregate storage/gas caps.
- The gateway atomically reserves budgets before signing, bounds transaction
  gas and token allowance, journals signed bytes/hashes/nonces/receipts, and
  returns idempotent upload results. Uncertain broadcasts block further signing.
- Partial storage uses retained upstream resumable finalization. Video and
  catalog Resume actions reuse their original job and approval. Lost recovery
  material after restart pauses instead of paying again. Read
  `PAYMENT_RECOVERY.md` before operating recovery.
- SQLite migration 0003 preserves published addresses and child rows, expands
  status/job constraints, adds approval and revision state, and invalidates
  legacy pending approvals. Backup/restore includes the payment journal;
  restoring an older snapshot explicitly pauses signing for reconciliation.

## Dependencies, organization, and runtime efficiency

- Rust 1.98.1, Node 24.20.0, Reqwest 0.13.4, SQLx 0.9, Rand 0.10, h2 0.4.19,
  React 19.2.8, Vite 8.2.2, and the approved compatible frontend/tooling versions
  are integrated with committed root, desktop, and npm lockfiles.
- JWT uses AWS-LC without the unused PEM/RustCrypto RSA tree. The RSA advisory
  exception was removed. Remaining Rust exceptions concern upstream maintenance
  notices, with reasons in `deny.toml`.
- Catalog snapshots use two batched queries and shared immutable manifests.
  Frontend requests cancel on selection changes and polling does not overlap.
  Restored sessions retain requested admin detail URLs.
- `deploy/versions.json` and `make check-runtime` detect version drift. CI covers
  both Rust/npm trees, all Compose renders, security tools, and desktop locks.
  Dependencies for the desktop are included in Dependabot.
- Promtail is replaced by Alloy with read-only log-file access, bounded Docker
  log rotation, project filtering, and no Docker socket. Updated monitoring
  images run with Loki TSDB v13.
- Desktop media archives require SHA-256 verification and safe extraction.
  Linux bundling now has square icons and the required xdg-utils dependency.
  Devcontainer auxiliary tooling/MCP startup is opt-in; VS Code remains supported.
- Local ignored `AGENTS.md` uses `claude --model opus`, records the resolved
  model, and documents fallback behavior. This installation resolved Opus 5.

## Validation completed

- Workspace Rust: 103 package tests, including the public-API decompression regression,
  plus 13 SQLite tests (including migration preservation, lease fencing, and
  catalog resume identity); formatting and Clippy passed. Later auth-backend
  changes and review regressions passed all 50 admin unit tests and seven DB
  integration tests, plus Clippy with the DB-test feature enabled.
- Frontend: lint, formatting, production build, seven Node tests, 41 Vitest
  tests; Node 24 Linux build/tests also passed. Playwright passed the real
  login/upload/UI spending approval/catalog publication/HLS playback flow.
  Review tests cover catalog cap approval/recovery, non-overlapping catalog
  polling, and discarding late responses after leaving administration. Branch
  coverage is 71.02%, above the unchanged 70% threshold.
- Full web and desktop npm audits: zero findings. Root and desktop Rust advisory
  checks passed with the documented upstream maintenance exceptions.
- All eight Compose render combinations, including the CI override, passed. Core, monitoring, and logging
  services ran successfully; Loki received the correct project's logs.
- Standard local smoke, admin-restart recovery smoke, an original above 16 MiB,
  and the native Linux launcher upload/playback smoke passed.
- Actual 20 MiB and 1 GiB + 4 KiB gateway uploads, configured verification,
  streamed byte/hash round trips, and duplicate retries passed. The large
  upload spanned two Merkle payment batches; neither retry added a transaction.
- macOS app/DMG and Linux AppImage/deb/rpm builds succeeded. The macOS first-run
  and configured loopback UI were inspected; the Linux executable ran under Xvfb.
- Three Python security-tool tests passed, including hostile archive rejection
  and backup/restore signing pause. A real backup-sidecar run captured both
  databases and catalog state.
- Repository filesystem scanning found no high/critical vulnerabilities or secrets.
- Runtime image scans found no fixable high/critical vulnerabilities in the
  admin, stream, frontend, Nginx, and custom gateway/devnet images.
- Reproducible benchmark scripts and observed timings/memory are documented in
  `PERFORMANCE_TUNING.md`. These are local measurements, not baseline speedup claims.

## Remaining release gates and limitations

- No production deployment or paid public-network transaction was performed.
  Read compatibility with historical manifest shapes and migration preservation
  are covered; a known historical public-network address has not been tested live.
- Desktop validation used ARM64 macOS/Linux development media binaries. Clean
  machines, x86-64 builds, signed/notarized macOS distribution, and supplied
  self-contained release FFmpeg archives still require release-environment checks.
- Current development utilities retain five high/critical dependency findings:
  pip-related vendored/SBOM msgpack and setuptools, GitHub CLI's x/mod (two),
  and Docker Buildx's go-archive. GitHub CLI 2.100.0 and Buildx 0.37.0
  were confirmed as the current official releases. Runtime images are unaffected. These remain
  visible in scans; no vulnerability ignore was added for them.
- Full concurrent-load tests and controlled before/after performance baselines
  have not run. One large gateway upload peaked around 1.93 GiB RSS.
- Upstream devnet 0.18.1 changes node identities and resets its ephemeral chain
  on full restart. Files are retained with `--no-cleanup`, but old local network
  storage is not automatically reattached. App-worker restart recovery was tested
  while retaining the network; this is distinct from devnet persistence.

## Independent PR review

Claude Code CLI resolved `--model opus` to `claude-opus-5` for the strict PR
review. It found no duplicate-payment path in the reviewed implementation and
identified recovery defects, which were corrected alongside Codex's own findings.
A follow-up review of the corrections is required before merging PR #204.

- Unpaid preparation can retry under its original identity, including after
  restart, only when no transaction reservation exists. Settled receipts alone
  cannot reopen paid work whose required SDK recovery material was lost.
- Exhausted approved uploads retain a recovery action. Catalog divergence and
  other failures after storage begins retain the original payment identity.
- Invalid unpaid plans can return to approval only after atomic gateway
  cancellation proves that no transaction reservation exists. Concurrent
  cancellation/signing tests prove that only one can succeed; paid or uncertain
  work remains in recovery.
- Login throttling counts failed attempts and uses bounded concurrency plus a
  short delay, avoiding a global minute-long lockout of valid credentials.
- Maximum-size metadata now fits the bounded cache (16 MiB of estimated parsed
  memory per cache). Local catalog requests share immutable catalog allocations;
  segment-cache insertion and metrics perform bounded expiry cleanup.
- Production startup repairs gateway journal ownership to `10001:10001` after
  restore. This was verified using a root-owned, mode-600 journal fixture.
- Backup volume selection is explicit. An incomplete backup remains a failure,
  with logs and backup metrics, rather than a falsely successful recovery set.
- Desktop cargo-deny now explicitly selects the shared configuration. CLI help
  confirms its default is the current directory's `deny.toml`; the prior local
  command already ran from the repository root. Full dependency audits remain
  blocking as required by the approved plan. The devbench uses cargo-deny 0.20.2
  to match CI, with configuration passed before the `check` subcommand.
- Native Host/Origin validation deliberately covers loopback services; Compose
  uses Nginx plus application authentication/CSRF. Alloy's host-wide read access
  before project filtering and the historical-manifest release gate are explicit.

Drain paid jobs, back up both databases and pending files, rehearse migrations
and restore, then promote verified artifacts. Preserve existing volumes and
published network data. Final cleanup removed about 20 GiB of repository build/dependency artifacts
and all task containers. Existing application data and volumes were preserved;
the rebuilt devbench image remains available for reuse.
