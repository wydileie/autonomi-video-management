# Repository Restructure & Optimization Plan — autonomi-video-management

## Context

The user wants this app brought to its best form: standardized directory structure (full monorepo restructure), aggressive code refactoring, and standardized config/tooling — executed phase-by-phase with the plan also delivered as a document in the repo.

Exploration found the repo already in strong shape: proper Cargo workspace with shared `workspace.dependencies`, hardened Docker (compose override patterns, read-only FS, cap-drop, secrets), comprehensive CI (fmt, clippy `-D warnings`, coverage, Trivy), strict TypeScript with zero `any`. The real gaps: no rustfmt/deny/lint configs, duplicated env-parsing helpers and two divergent Autonomi REST clients across the Rust services, god-modules (rust_admin/media.rs 1,102 lines, pipeline.rs 1,010, launcher_core/lib.rs 1,143, common/lib.rs 713), three React god-components (UploadPanel 543, VideoPlayer 491, Library 455), manual Docker layer caching instead of cargo-chef, flat root layout mixing crates/apps/infra, e2e not in CI, non-blocking audits.

**Important corrections found during design (verified against source):**
- Of ~228 `.unwrap()` calls, only **2 are production code**: `rust_admin/src/quote.rs:90` and `rust_admin/src/antd_client.rs:179`. The rest are in `#[cfg(test)]` modules. Remediation = 2 fixes + lint enforcement.
- `launcher_core/src/lib.rs:873` compiles in the relative path `../react_frontend/build` — silently breaks after the move; no test covers it.
- `docker-compose.yml` has no top-level `name:` — moving compose files to `deploy/` changes the default project name to `deploy`, which would **recreate all named volumes empty**. Must pin `name:` before/with the move.
- The desktop app (workspace-excluded) is never compile-checked in CI — a restructure could silently break it. Add a `cargo check` CI job *before* moving anything.
- Frontend error parsing (`apps/web` `client.ts:230`) reads `data.detail` (rust_admin's format); antd_service's `{error, code}` format is only consumed server-to-server — safe to unify on `{detail, code}`.

## Final decisions

| Decision | Choice |
|---|---|
| Layout | `crates/{common,antd_service,rust_admin,rust_stream,launcher_core,standalone_launcher}`, `apps/web` (was react_frontend), `apps/desktop` (was desktop_app), `deploy/` (8 compose files + nginx, monitoring, backup_sidecar, autonomi_devnet). `scripts/`, `docs/`, `.github/`, `.devcontainer/`, `testvids/`, Makefile, env files stay at root |
| Crate/binary renames | **None** — package/binary names (`antd`, `rust_admin`, `rust_stream`, …) stay stable; renaming would churn Dockerfiles, compose, tauri `externalBin`, sidecar staging for zero functional gain |
| Dockerfiles | Stay with their component (`crates/rust_admin/Dockerfile`, `apps/web/Dockerfile`, …) |
| Lints | `[workspace.lints]` in root Cargo.toml, `[lints] workspace = true` per crate; `clippy::unwrap_used = "warn"` (CI `-D warnings` makes it blocking); `#![allow(clippy::unwrap_used)]` in test modules |
| Error body | Unified `ApiError` in `autvid_common` rendering `{"detail", "code"}` |
| Antd client | Core `AntdClient` in `autvid_common` with `AntdMetricsRecorder` trait (noop default); rust_admin implements recorder, rust_stream uses subset |
| Runtime images | rust_stream distroless (unchanged); rust_admin debian-slim (needs ffmpeg); antd_service: try distroless/cc + liblzma copy, fall back to debian-slim |
| Audits | cargo-deny + `npm audit --omit=dev` + Trivy all blocking; `.git-blame-ignore-revs` for mechanical commits |
| e2e in CI | Runs on main pushes + nightly + manual dispatch (too heavy per-PR), using prebuilt GHCR devnet image via `deploy/docker-compose.ci.yml` |
| Plan document | Committed as `docs/IMPROVEMENT_PLAN.md` in Phase 1 (user requested the doc as a deliverable) |

## Phase 1 — Tooling baseline & CI safety net (no moves, no refactors)

Create: `rustfmt.toml` (minimal, defaults already match), `deny.toml` (advisories/licenses/bans/sources; start licenses at `warn` if transitive conflicts appear), `react_frontend/.prettierrc.json` + `.prettierignore`, `.git-blame-ignore-revs`, `docs/IMPROVEMENT_PLAN.md` (this plan).

Modify:
- Root `Cargo.toml`: `[workspace.lints.clippy] unwrap_used = "warn"`; all crate Cargo.tomls + desktop: `[lints] workspace = true` (desktop copies the table — workspace-excluded)
- Add `#![allow(clippy::unwrap_used)]` to each test module (~30 sites)
- Fix the 2 production unwraps: `quote.rs:90` → `.expect(...)` or entry API; `antd_client.rs:179` → match instead of unwrap
- `react_frontend/package.json`: prettier + eslint-config-prettier, `format`/`format:check` scripts, one-time reformat (record in blame-ignore); `eslint.config.mjs` appends prettier config
- `.github/workflows/ci.yml`: add `cargo-deny` job (EmbarkStudios action), **`desktop-check` job** (`cargo check` on desktop manifest with Tauri system deps), prettier check in frontend job; make `advisory-scans` blocking (drop `continue-on-error`)
- `Makefile`: `fmt-react`, `deny-rust` targets wired into `ci`; `.pre-commit-config.yaml`: prettier hook

Verify: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo test -p rust_admin --features db-tests db_tests -- --test-threads=1`; `npm run lint && npm run format:check && npm test && npm run build`; `cargo deny check`; `make compose-config`; CI green including desktop-check.

## Phase 2 — Full directory restructure (pure `git mv` + path updates, zero logic changes)

Moves: crates → `crates/`, react_frontend → `apps/web`, desktop_app → `apps/desktop`, 8 compose files + nginx + monitoring + backup_sidecar + autonomi_devnet → `deploy/`.

Reference updates (categories, all verified):
1. Root `Cargo.toml` members/exclude; `apps/desktop/src-tauri/Cargo.toml` launcher_core path → `../../../crates/launcher_core`
2. **`crates/launcher_core/src/lib.rs:873`**: `../react_frontend/build` → `../../apps/web/build`
3. **`apps/desktop/src-tauri/tauri.conf.json`**: `beforeDevCommand`/`beforeBuildCommand` (`cd ../web`), `frontendDist` + `bundle.resources` → `../../web/build`
4. 3 Rust Dockerfiles: COPY paths gain `crates/` prefix (context stays repo root)
5. Compose files: build contexts/dockerfile paths (`context: ..` for root-context builds, `context: ../apps/web` for frontend); **add `name: autonomi-video-management` to `deploy/docker-compose.yml`** (volume-identity preservation); document `COMPOSE_PROJECT_NAME` for prod hosts in DEPLOYMENT.md
6. `Makefile`: `COMPOSE_DIR = deploy` prefix on all `-f` lists; `cd apps/web` / `cd apps/desktop`
7. `ci.yml`, `publish-images.yml` (matrix `file:`/context values), `desktop-release.yml` (working-directories, artifact paths, cache paths)
8. `dependabot.yml`: all 10 stream directories
9. `.pre-commit-config.yaml` regexes; `scripts/stage-tauri-sidecars.sh`; `scripts/smoke-local-devnet.sh` compose paths
10. `apps/web/e2e/upload-playback.spec.ts:7` testvids path → `../../../testvids`
11. `.dockerignore`, `.gitignore` path entries; README (~25 refs) + docs/*.md (~80 refs total)
12. Final sweep: `grep -rn 'react_frontend|desktop_app|docker-compose' .` until only intentional hits remain

Two commits: (1) pure `git mv`, (2) path-literal updates; both recorded in `.git-blame-ignore-revs`.

Verify: full Phase 1 gate + `make compose-config` + **`make smoke-local`** (runtime proof) + `cargo check` on desktop + manual `cargo run -p autvid_launcher` check for the compiled-in frontend path (untested by suite).

## Phase 3 — `autvid_common` consolidation

Split `crates/common/src/lib.rs` (713 lines) into: `metrics.rs`, `resilience.rs` (CircuitBreaker, retry classification, jitter), `env.rs` (existing helpers + deduplicated `duration_from_env`/`usize_from_env`/`parse_*_env` family lifted from rust_admin/config.rs:429–491), `security.rs` (constant_time_eq, CORS parsing), `health.rs`, `error.rs` (unified ApiError), `antd/{mod,types,recorder}.rs` (unified client). `lib.rs` keeps `pub use` re-exports.

- Delete duplicated helpers from the 3 service `config.rs` files. Note: rust_stream's copies silently default on parse errors — switching to strict fail-on-bad-env is an intentional behavior change.
- Unified `ApiError` `{detail, code}`; antd_service body flips `error`→`detail` (grep-verified nothing parses the old field)
- Unified `AntdClient` with `AntdMetricsRecorder` trait; port rust_stream (smaller consumer) first, preserving its `raw_endpoint_unavailable` fallback semantics exactly, then rust_admin (implements recorder over its metrics, keeps circuit breaker + retry config). DTOs to `antd/types.rs`. Existing mock-based client tests are the contract.

Verify: full gate + `make smoke-local` + `make smoke-local-restart`; **diff rust_admin `/metrics` output before/after** — metric names must not change (Grafana dashboards depend on them; existing render_prometheus test helps).

Risk: highest-risk refactor in the plan (retry counts/timeout shapes differ between the two clients) — replicate behavior exactly first, improve later.

## Phase 4 — Aggressive Rust decomposition

Target: no source file over ~500 lines outside tests. Each `x.rs` → `x/` directory with `mod.rs` re-exporting the current public surface (call sites unchanged). One commit per module tree, `cargo test -p <crate>` between each.

- `rust_admin/media/` → `probe.rs`, `transcode.rs`, `encoding.rs`, `segments.rs`, `paths.rs`
- `rust_admin/pipeline/` → `process.rs`, `final_quote.rs`, `publish.rs`
- `rust_admin/auth/` → `handlers.rs`, `tokens.rs`, `sessions.rs`, `extract.rs`, `cookies.rs`
- `rust_admin/config/` → `mod.rs` (smaller post-Phase 3), `validation.rs`, `cors.rs`
- `rust_admin/models/` → `video.rs`, `manifest.rs`, `quote.rs`, `jobs.rs`, `health.rs`
- `rust_admin/upload/` → `accept.rs`, `multipart.rs`, `validate.rs` (`format_bytes` → common)
- `rust_admin/jobs.rs` remainder → `jobs/queue.rs`
- `launcher_core/lib.rs` (1,143) → `options.rs`, `setup.rs`, `stack.rs`, `process.rs`, `proxy.rs`, `tools.rs`, `util.rs`; `lib.rs` = re-exports (stable API for standalone_launcher + desktop)
- `rust_stream/main.rs` (587) → thin `main.rs` (~60 lines), `server.rs`, tests → `tests.rs`/`test_support.rs`

Verify: full gate per sub-commit; db-tests; desktop `cargo check`; `make smoke-local` at phase end. Pure moves — `git diff --stat` near-zero net line change besides mod/use lines.

## Phase 5 — Frontend refactors

- `hooks/useUploadWorkflow` family: `useUploadForm` (17 states → consider `useReducer`; quote depends on file+settings, bitrate defaults depend on codec — reducer avoids stale closures), `useUploadQuote`, `useUploadSubmit`, `useDragAndDrop`; `components/upload/{FileDropZone,EncodeSettingsFields,UploadQuoteSummary}.tsx`; UploadPanel → composition (~150 lines)
- `hooks/useHlsPlayer.ts` (HLS lifecycle incl. the subtle seek-sync suppression at VideoPlayer.tsx:105–190 — move wholesale, don't "improve"), `useControlsVisibility`; `components/player/{PlayerControls,QualityMenu}.tsx`
- `hooks/useLibrary` family (`useLibraryData`, `useVideoDetail`, `useVideoActions`, `useCatalogs`); `components/library/{VideoGrid,VideoDetailPane,CatalogPanel}.tsx`
- `App.tsx`: `React.lazy` for Library + UploadPanel (defers the `hls` chunk from initial bundle) with `<Suspense>`; LoginPanel/DesktopSetupGate stay eager

One component per commit (player → library → upload), then lazy-loading. Existing `src/__tests__/` page-level tests are the behavioral contract and must keep passing; 70% coverage thresholds must not dip.

Verify: `npm run lint && npm run format:check && npm test && npm run test:coverage && npm run build`; compare vite chunk report before/after; `make smoke-local`.

## Phase 6 — Docker & CI optimization

- cargo-chef in the 3 Rust Dockerfiles (chef → planner → cook recipe → build; root context; tighten `.dockerignore` to exclude `apps/`, `deploy/monitoring`, `docs/` so the planner stage doesn't invalidate spuriously)
- `deploy/autonomi_devnet/Dockerfile`: pin Foundry via `ARG FOUNDRY_VERSION` (delete the live GitHub API "latest" call); pin ant-sdk/ant-node clones to tags/SHAs via ARGs
- antd_service runtime: attempt distroless/cc + `liblzma.so.5` copy (all crates use rustls, no libssl needed); fall back to debian-slim if smoke fails. rust_admin stays debian-slim (ffmpeg)
- New CI `e2e` job (main pushes + nightly schedule + workflow_dispatch): committed `deploy/docker-compose.ci.yml` overriding devnet to prebuilt GHCR image; compose up --wait; `npx playwright install chromium --with-deps`; `E2E_BASE_URL=http://localhost npm run e2e`; upload report + compose logs on failure. Verify `.env.local.example` boots the stack (add committed `.env.ci` if placeholders block boot). Mark non-required initially; promote after a week of green nightlies
- Makefile `e2e-local` target

Verify: build all 3 images; second build with a 1-line source change must skip dependency compilation (chef cache hit); `make smoke-local` with new images; run e2e job via workflow_dispatch; publish-images.yml still builds.

## Phase 7 — Integration test extraction

Convert `rust_admin` and `antd_service` to lib+bin crates (thin `main.rs` calling `lib.rs::run()`; visibility bumps `pub(crate)` → `pub` for tested modules, `#[doc(hidden)]` where warranted):
- `crates/rust_admin/tests/db_tests.rs` receives the ~700-line harness currently at `routes.rs:118+` (`#![cfg(feature = "db-tests")]`; keep module name `db_tests` so the existing Makefile/CI invocation still matches)
- `crates/rust_admin/tests/common/mod.rs`: shared fixtures
- `crates/antd_service/tests/api.rs`: mock-upstream route tests (reuse rust_stream's axum test-server pattern)
- Genuinely unit-level tests (encoding math, parsing, token round-trips) stay inline

Verify: full gate; `cargo test -p rust_admin --features db-tests db_tests -- --test-threads=1` and `cargo test -p antd` unchanged; `cargo llvm-cov --workspace` (coverage job) still works; Docker builds unaffected.

## Overall verification strategy

Per-phase gate (local + CI):
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p rust_admin --features db-tests db_tests -- --test-threads=1
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo deny check
cd apps/web && npm run lint && npm run format:check && npm test && npm run build
make compose-config
```
Runtime gate (phases 2, 3, 6 + once after 4/5): `make smoke-local` (login → upload → transcode → publish → HLS fetch). Release-path gate (phases 2, 6): `make stage-tauri-sidecars`, workflow_dispatch runs of publish-images.yml and desktop-release.yml. Each phase = one PR; mechanical commits in `.git-blame-ignore-revs`.

## Critical files

- `Cargo.toml` (workspace members/lints/exclude — phases 1, 2)
- `Makefile` (~45 targets; compose paths — phases 1, 2, 6)
- `.github/workflows/ci.yml` (every phase's gate; new deny/desktop-check/e2e jobs)
- `common/src/lib.rs` (nucleus of phases 3–4)
- `desktop_app/src-tauri/tauri.conf.json` + `launcher_core/src/lib.rs:873` (highest-risk untested path couplings in the restructure)
- `docker-compose.yml` (needs `name:` pinned before the move — volume identity)
