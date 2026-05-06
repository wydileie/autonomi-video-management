# Autonomi Video Management

A self-hosted, decentralised video management platform. Upload videos, transcode them into adaptive HLS streams at multiple resolutions, and store every segment permanently on the [Autonomi](https://autonomi.com) network. Playback is served directly from the network — no CDN, no single point of failure, pay-once storage.

## How it works

```
Browser
  │
  ├── Upload video ──► Rust Admin (Axum)
  │                        │
  │                        ├─ FFmpeg → HLS .ts segments per resolution
  │                        │
  │                        └─ antd REST ──► antd daemon ──► Autonomi network
  │                                            (stores each segment, returns address)
  │
  └── Play video ───► Rust Streaming (Axum)
                           │
                           ├─ Reads catalog + video manifests from Autonomi
                           │
                           └─ antd-client SDK ──► antd daemon ──► Autonomi network
                                                      (fetches manifests and segments on demand)
```

### Upload flow
1. User drops or selects a video file; the browser detects its source resolution and offers standard adaptive renditions from 8K down to 144p without upscaling by default.
2. The React frontend POSTs the file to the Rust admin service.
3. Rust admin saves it to the configured processing bind mount, records the durable job inputs in Postgres, and queues a worker task.
4. FFmpeg transcodes the video into small HLS `.ts` segments at each resolution.
5. Rust admin sums the actual transcoded segment sizes, optionally includes the original source file, and asks `antd` for a final quote using the real bytes.
6. The job pauses as `awaiting_approval`; the frontend shows the final quote and expiry time.
7. After approval, every segment and the optional original source file are streamed to the Autonomi network through the `antd` daemon file endpoint; small JSON metadata still uses the data endpoint.
8. A video manifest JSON containing resolution, segment order, durations, optional original-file metadata, and Autonomi addresses is stored on Autonomi.
9. The job status flips to `ready`. Admins can then publish or unpublish the video from the public library, or choose automatic publishing during upload.
10. Publishing stores a catalog JSON containing the public video list and manifest addresses on Autonomi. Until `antd` exposes mutable pointers/scratchpads, the latest catalog address is bookmarked in the shared `catalog_state` volume.

### Playback flow
1. The HLS player (hls.js) requests `/stream/{video_id}/{resolution}/playlist.m3u8`.
2. The Rust streaming service fetches the network catalog and video manifest from Autonomi.
3. Each segment URL points back to `/stream/{video_id}/{resolution}/{index}.ts`.
4. The Rust service fetches the segment from Autonomi via `data_get_public` and streams the bytes to the player.

---

## Services

| Service | Language | Port | Purpose |
|---|---|---|---|
| `antd` | Rust | 8082 (REST), 50051 (gRPC) | Autonomi network gateway daemon |
| `rust_admin` | Rust / Axum | 8000 | Video upload, FFmpeg transcoding, metadata API |
| `rust_stream` | Rust / Axum | 8081 | HLS manifest generation + Autonomi segment proxy |
| `react_frontend` | React | 80 | Upload UI, video library, HLS player |
| `nginx` | — | 80 | Reverse proxy for the frontend, admin API, and stream API |
| `db` | PostgreSQL 16 | 5432 | Upload job/status cache and worker recovery state |

### URL routing (via Nginx)

| Path | Proxies to |
|---|---|
| `/` | React frontend |
| `/api/*` | Rust admin (path rewritten to `/*`) |
| `/stream/*` | Rust streaming service |

### Observability and proxy safety

Nginx and all Rust HTTP services accept or generate `X-Request-ID`, expose it
to browser clients, and preserve it in structured request logs. Service logs
include the service name, request ID, method, URI, status, and latency; longer
admin jobs add fields such as `video_id`, `job_id`, `resolution`,
`segment_index`, and `catalog_publish_epoch`.

The Nginx proxy keeps uploads unlimited at the proxy layer and relies on
`UPLOAD_MAX_FILE_BYTES` in `rust_admin` for the application limit. It also
applies standard security headers and rate limits login attempts on
`/api/auth/login`.
Admin login continues returning bearer tokens for compatibility and also sets a
HttpOnly SameSite cookie for browser sessions.

Internal Prometheus-style metrics are exposed on `/api/metrics`,
`/stream/metrics`, and the internal `antd` gateway `/metrics` endpoint. They
cover HTTP request counts/latency, admin job counts, FFmpeg runtime, outbound
`antd` latency, upload retries, and stream segment cache hit/miss/coalescing
counts.

---

## Quick start (development)

The project ships with a devcontainer that handles the full local setup automatically, including a local Autonomi testnet with a pre-funded test wallet.

### Prerequisites
- [Docker Desktop](https://www.docker.com/products/docker-desktop/) (or Docker Engine + Docker Compose)
- [VS Code](https://code.visualstudio.com/) with the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)

### Steps

```bash
git clone <this-repo>
cd autonomi-video-management
code .
# VS Code prompts: "Reopen in Container" → click it
```

On first open, VS Code builds the container image (takes 20–40 minutes — it compiles three Rust projects: `antd`, `ant`, and `ant-devnet`). Subsequent starts are fast.

Once inside the container, a `postStartCommand` automatically runs [`.devcontainer/start_autonomi.sh`](.devcontainer/start_autonomi.sh) which:
1. Starts a local Autonomi testnet (`ant-devnet`) with a pre-funded wallet.
2. Starts the `antd` gateway daemon pointed at it.
3. Waits until `http://localhost:8082/health` responds.

You can verify the daemon is healthy:
```bash
curl http://localhost:8082/health
# {"status":"ok","network":"local"}
```

See [`.devcontainer/README.md`](.devcontainer/README.md) for full details on the dev environment.

---

## Testing and CI

The repository includes a root `Makefile` for local checks and a GitHub Actions
workflow that runs the same service-level gates.

```bash
# Run the Rust backend tests
make test-rust
make test-rust-admin

# Run Postgres-backed durable-job integration tests.
# TEST_DATABASE_URL should point at a maintenance database where temporary
# per-test databases can be created and dropped.
TEST_DATABASE_URL=postgresql://postgres:postgres@localhost:5432/postgres make test-rust-db

# Install, build, and test the React frontend
make install-react
make build-react
make test-react

# Run all local test targets
make test

# Run the full CI-shaped sequence, including dependency installs
make ci

# Run advisory checks locally; optional cargo-audit and Trivy scans are skipped if unavailable
make audit
```

The Rust targets run from the root Cargo workspace and cover `rust_admin`,
`rust_stream`, and `antd_service`. The React target runs the Vite/Vitest test
command once. CI also runs a Postgres service for the feature-gated
`rust_admin` durable-job integration tests. Non-blocking advisory scans run
with `cargo audit`, `npm audit --omit=dev`, and Trivy filesystem scanning.
Optional scanner install failures are reported as warnings so findings can be
triaged before those gates become blocking.

When the local devnet stack is already running, the smoke harness exercises the
same browser-facing contracts a real upload uses: `/api` auth, quote, upload,
final approval, publication, `/stream` playlists, and segment fetches.

```bash
# In one terminal, run the local stack.
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build

# In another terminal, run the workflow smoke.
make smoke-local

# Restart rust_admin during the workflow to exercise durable job recovery.
make smoke-local-restart

# Expand the generated source over 16MB and require original-source storage.
make smoke-local-large-original
```

Set `SMOKE_BASE_URL`, `SMOKE_ADMIN_USERNAME`, `SMOKE_ADMIN_PASSWORD`,
`SMOKE_RESOLUTIONS`, or `SMOKE_VIDEO_PATH` to point the harness at a different
stack, account, rendition set, or source file. The large-original smoke is the
quick regression check that media uploads use the streaming file endpoint rather
than the legacy JSON body path.

---

## Running the application stack

The app is intended to run as a containerized stack. Use the base Compose file
plus one overlay:

- `docker-compose.local.yml` runs a self-contained local Autonomi devnet for testing.
- `docker-compose.local-public.yml` keeps that local devnet internal while exposing only the app proxy for internet-accessible demos.
- `docker-compose.prod.yml` runs `antd` against the configured Autonomi network.

Compose remains the supported deployment runtime. The repo also documents the
service boundary expected by a future native packaged host so native work can
reuse the same Rust admin, Rust stream, `antd`, Postgres, endpoint, and
storage-path contracts without changing current containers. See
[`docs/RUNTIME_MODES.md`](docs/RUNTIME_MODES.md) and the machine-readable
[`docs/runtime-contract.example.json`](docs/runtime-contract.example.json).

### Local Testnet

```bash
cp .env.local.example .env.local

# Optional but recommended for large uploads: point processing files at a
# large, persistent host disk.
# VIDEO_PROCESSING_HOST_PATH=/mnt/video-processing/autvid
#
# Local devnet defaults set UPLOAD_MIN_FREE_BYTES=0, so uploads are only
# limited by the actual free space on VIDEO_PROCESSING_HOST_PATH.

docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build
```

Services are available at:
- Frontend: `http://localhost` via Nginx
- Admin API: `http://localhost/api` via Nginx
- Stream API: `http://localhost/stream` via Nginx
- Autonomi gateway: `http://localhost:8082`

To publish direct admin and stream debug ports, add the debug overlay:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.debug-ports.yml \
  up --build
```

### Internet-accessible Local Devnet Demo

This mode is useful for a public demo that should not spend real Autonomi
storage tokens. It uses production-strength app auth with the local devnet and
publishes only the Nginx app proxy.

```bash
cp .env.local-public.example .env.local-public
# Fill in the domain, admin credentials, auth secret, and processing path.

docker compose --env-file .env.local-public \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.local-public.yml \
  up --build -d
```

Do not add `docker-compose.debug-ports.yml` on an internet-facing demo unless
you intentionally want debug ports exposed. The local devnet is not permanent
public Autonomi storage; data lives in local Docker volumes.

### Production

```bash
cp .env.production.example .env.production
# Fill in a wallet source and any network/payment settings.

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  up --build -d
```

For production wallet keys, the simple env path remains supported:
`PROD_AUTONOMI_WALLET_KEY=0x...`. The preferred production path is to mount a
Docker Secret or other read-only file and set
`PROD_AUTONOMI_WALLET_KEY_FILE=/run/secrets/autonomi_wallet_key` in
`.env.production`; when that file variable is set, it wins over the direct env
key.

Keep the base production compose files usable without a real secret file by
putting the secret mount in a local overlay such as:

```yaml
services:
  antd:
    secrets:
      - source: autonomi_wallet_key
        target: autonomi_wallet_key
        mode: 0400

secrets:
  autonomi_wallet_key:
    file: ./secrets/autonomi_wallet_key.txt
```

Run with that extra overlay only on hosts where the secret file exists:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.prod.secrets.yml \
  up --build -d
```

For public deployments, put TLS and domain routing in front of the stack with
your preferred reverse proxy or hosting platform. Upload and management actions
are protected by the app's single-admin login. The production compose path
publishes only the Nginx reverse proxy; publish admin, stream, or antd ports
only with an explicit debug override.

---

## Environment variables

Start from `.env.local.example` for local testing or `.env.production.example`
for deployment. `.env.example` contains the full variable set in one file.

| Variable | Required | Description |
|---|---|---|
| `POSTGRES_USER` / `POSTGRES_PASSWORD` | Yes | PostgreSQL root credentials |
| `ADMIN_DB` / `ADMIN_USER` / `ADMIN_PASS` | Yes | App database credentials |
| `APP_ENV` | Production | Set to `production` in deployments. Production mode rejects default or weak admin credentials at startup |
| `ADMIN_USERNAME` / `ADMIN_PASSWORD` | Yes | Single uploader/admin login for uploads, approvals, deletes, and library management |
| `ADMIN_AUTH_SECRET` | Yes | Long random secret used to sign admin login tokens |
| `ADMIN_AUTH_TTL_HOURS` | No | Admin login token lifetime. Default: `12` |
| `ADMIN_REFRESH_TOKEN_TTL_HOURS` | No | HttpOnly refresh-cookie session lifetime. Default: `720` |
| `ADMIN_AUTH_COOKIE_SAME_SITE` | No | SameSite attribute for admin access and refresh cookies: `Strict`, `Lax`, or `None`. Default: `Lax`; `None` requires secure cookies |
| `ADMIN_AUTH_COOKIE_SECURE` | No | Whether the HttpOnly admin cookie includes `Secure`. Defaults to `true` when `APP_ENV=production`, otherwise `false` |
| `ADMIN_REQUEST_TIMEOUT_SECONDS` | No | Default `rust_admin` route timeout for non-upload requests. Default: `120` |
| `ADMIN_UPLOAD_REQUEST_TIMEOUT_SECONDS` | No | `rust_admin` source upload route timeout. Default: `3600` |
| `VIDEO_PROCESSING_HOST_PATH` | Recommended | Host path bind-mounted for original uploads and transcoded segment files while jobs are processing, awaiting approval, or resuming after a restart. A one-shot Compose init container creates and chowns it for the non-root admin service user |
| `DOMAIN` | No | Domain label for external proxies or deployment tooling |
| `APP_HTTP_PORT` | No | Host port for Nginx, the only app-facing port published by the production compose path |
| `ADMIN_HTTP_PORT` / `STREAM_HTTP_PORT` | Local/debug only | Direct host ports for the admin and stream services when using a local/debug compose override |
| `CORS_ALLOWED_ORIGINS` | No | Comma-separated explicit browser origins allowed to call admin/stream directly. Wildcard `*` is rejected |
| `ANTD_REST_PORT` / `ANTD_GRPC_PORT` | Local/debug only | Direct host ports for the Autonomi gateway when using a local/debug compose override |
| `PROD_AUTONOMI_WALLET_KEY` | Production writes | Hex-encoded EVM private key (`0x...`) for Autonomi storage payments |
| `PROD_AUTONOMI_WALLET_KEY_FILE` | Production writes | Optional file path, usually a Docker Secret mounted under `/run/secrets`, containing the wallet private key. When set, this takes precedence over `PROD_AUTONOMI_WALLET_KEY` |
| `PROD_ANTD_NETWORK` | Production | `default` unless you are targeting a custom network |
| `PROD_AUTONOMI_PEERS` | Production/custom | Comma-separated bootstrap multiaddrs |
| `ANT_DEVNET_PRESET` | Local only | Local devnet size: `minimal`, `small`, or `default` |
| `ANT_DEVNET_RESET_ON_START` | Local only | Reset active local devnet node data on container start to avoid stale testnet peers. Default: `true` |
| `ANTD_PAYMENT_MODE` | No | Upload payment strategy: `auto`, `merkle`, or `single`. Default: `auto` |
| `ANTD_METADATA_PAYMENT_MODE` | No | Rust admin payment strategy for small manifest/catalog JSON writes. Default: `merkle` |
| `ANTD_UPLOAD_VERIFY` | No | Read each uploaded segment back before publishing the manifest. Default: `true` |
| `ANTD_UPLOAD_RETRIES` | No | Number of upload/verify attempts per segment. Default: `3` |
| `ANTD_UPLOAD_TIMEOUT_SECONDS` | No | Per upload/read-back timeout before retrying a segment. Default: `120` |
| `ANTD_QUOTE_CONCURRENCY` | No | Rust admin concurrent final-quote cost checks. Default: `8` |
| `ANTD_UPLOAD_CONCURRENCY` | No | Rust admin concurrent segment upload/verify tasks. Default: `4` |
| `ANTD_APPROVE_ON_STARTUP` | No | Whether `rust_admin` runs the one-time wallet spend approval on startup. Default: `true` |
| `ANTD_REQUIRE_COST_READY` | No | Whether admin startup and health require a successful Autonomi write-cost probe, not just `/health`. Default: `false` |
| `ANTD_DIRECT_UPLOAD_MAX_BYTES` | No | Max bytes allowed through the legacy base64 JSON data endpoint from `rust_admin`; media files use the streaming file endpoint. Default: `16777216` |
| `ANTD_REQUEST_TIMEOUT_SECONDS` | No | `antd` gateway default route timeout for non-file-upload requests. Default: `60` |
| `ANTD_FILE_UPLOAD_REQUEST_TIMEOUT_SECONDS` | No | `antd` gateway streaming file upload route timeout. Default: `3600` |
| `ANTD_JSON_BODY_LIMIT_BYTES` | No | `antd` gateway JSON body limit for quote/cost and legacy data routes. Default: `33554432` |
| `ADMIN_JOB_WORKERS` | No | Number of durable `rust_admin` DB job workers. Default: `1` |
| `ADMIN_JOB_POLL_INTERVAL_SECONDS` | No | Poll interval when no durable jobs are ready. Default: `2` |
| `ADMIN_JOB_LEASE_SECONDS` | No | Lease duration for a running durable job before another worker may recover it. Default: `43200` |
| `ADMIN_JOB_MAX_ATTEMPTS` | No | Max processing/upload job attempts before a video is marked `error`. Default: `3` |
| `CATALOG_PUBLISH_JOB_MAX_ATTEMPTS` | No | Max durable catalog publish attempts before the publish job is marked failed. Default: `12` |
| `UPLOAD_QUOTE_TRANSCODED_OVERHEAD` | No | Multiplier applied to upload quote estimates for transcoded bytes. Default: `1.08` |
| `UPLOAD_QUOTE_MAX_SAMPLE_BYTES` | No | Max sample size for upload quote cost checks. Default: `16777216` |
| `UPLOAD_MAX_FILE_BYTES` | No | Max accepted source upload bytes. Default: `21474836480` (20 GiB) |
| `UPLOAD_MAX_DURATION_SECONDS` | No | Max accepted source duration from server-side `ffprobe`. Default: `14400` |
| `UPLOAD_MAX_SOURCE_PIXELS` | No | Max accepted source pixel count after rotation metadata. Default: `33177600` (8K) |
| `UPLOAD_MAX_SOURCE_LONG_EDGE` | No | Max accepted source long edge in pixels after rotation metadata. Default: `7680` |
| `UPLOAD_MIN_FREE_BYTES` | No | Extra processing-disk free-space headroom required before and during upload writes. Base/prod default: `5368709120` (5 GiB). Local devnet default: `0` |
| `UPLOAD_MAX_CONCURRENT_SAVES` | No | Max concurrent source uploads being streamed/probed to disk. Default: `2` |
| `UPLOAD_FFPROBE_TIMEOUT_SECONDS` | No | Server-side upload validation `ffprobe` timeout. Default: `30` |
| `HLS_SEGMENT_DURATION` | No | Target seconds per forced-keyframe HLS segment. Default: `1` |
| `FFMPEG_THREADS` | No | Maximum FFmpeg/x264 encoder threads. Default: `2` to reduce memory use for high-resolution portrait transcodes |
| `FFMPEG_FILTER_THREADS` | No | Maximum FFmpeg filter graph threads. Default: `1` |
| `FFMPEG_MAX_PARALLEL_RENDITIONS` | No | Max renditions to transcode at once inside one processing job. Base/prod default: `1`; local examples set `2` |
| `FINAL_QUOTE_APPROVAL_TTL_SECONDS` | No | Seconds before an unapproved final quote expires and local transcoded files are deleted. Default: `14400` |
| `APPROVAL_CLEANUP_INTERVAL_SECONDS` | No | Seconds between cleanup scans for expired final quotes. Default: `300` |
| `CATALOG_ADDRESS` | No | Optional bootstrap address for an existing network-hosted video catalog |
| `STREAM_CATALOG_CACHE_TTL_SECONDS` | No | Rust stream catalog metadata cache TTL. Default: `10`; set `0` to disable |
| `STREAM_MANIFEST_CACHE_TTL_SECONDS` | No | Rust stream video manifest cache TTL. Default: `300`; set `0` to disable |
| `STREAM_SEGMENT_CACHE_TTL_SECONDS` | No | Rust stream segment byte cache TTL and response `Cache-Control` max-age. Default: `3600`; set `0` to disable in-process segment caching |
| `STREAM_SEGMENT_CACHE_MAX_BYTES` | No | Rust stream in-process segment cache byte cap. Default: `67108864` |
| `STREAM_REQUEST_TIMEOUT_SECONDS` | No | `rust_stream` route timeout. Default: `60` |
| `PROD_EVM_RPC_URL` | Production/custom | EVM JSON-RPC endpoint for custom payment networks |
| `PROD_EVM_PAYMENT_TOKEN_ADDRESS` | Production/custom | Payment token contract for custom payment networks |
| `PROD_EVM_PAYMENT_VAULT_ADDRESS` | Production/custom | Payment vault contract for custom payment networks |

---

## Optional Operations Overlays

The base Compose files stay self-contained for local development. Optional
production and operations overlays live in separate docs:

- [Image publishing](docs/IMAGE_PUBLISHING.md) covers GHCR image builds and tags.
- [Backup sidecar](docs/BACKUP_SIDECAR.md) covers scheduled Postgres/catalog backups.
- [Observability](docs/OBSERVABILITY.md) covers Prometheus, Grafana, and Alertmanager.

---

## Database schema

Managed by the Rust admin service on startup and seeded by [`postgres-init/init-dbs.sh`](postgres-init/init-dbs.sh). Postgres is no longer the source of truth for ready video playback; it is a processing/status cache and recovery log for interrupted local jobs.

```
videos
  id, title, original_filename, description, status, job_dir,
  job_source_path, requested_resolutions, upload_original,
  original_file_address, original_file_byte_size,
  publish_when_ready, created_at, user_id

video_variants          (one per resolution per video)
  id, video_id → videos, resolution, width, height,
  video_bitrate, audio_bitrate, segment_duration, total_duration, segment_count

video_segments          (one per .ts chunk per variant)
  id, variant_id → video_variants, segment_index,
  local_path, autonomi_address, duration, byte_size

On startup the admin service scans for `pending`, `processing`, and `uploading`
rows. Interrupted transcodes are restarted from the saved source file and
requested resolutions; interrupted uploads resume from persisted segment rows and
skip segments that already have Autonomi addresses. This requires
`VIDEO_PROCESSING_HOST_PATH` to point at persistent host storage, because the
admin container must still be able to read the original upload and transcoded
segments after a restart.

Autonomi stores the durable playback metadata:

video manifest
  id, title, visibility flags, variants[], segments[] with Autonomi addresses and durations

catalog manifest
  videos[] with video id, title, visibility flags, variant summaries, and video manifest address
```

---

## API reference

### Rust Admin (`/api`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/metrics` | Internal Prometheus-style metrics |
| `POST` | `/auth/login` | Sign in as the single admin user |
| `POST` | `/auth/logout` | Clear the HttpOnly admin cookie |
| `GET` | `/auth/me` | Validate the current admin bearer token |
| `POST` | `/videos/upload/quote` | Estimate Autonomi storage and gas cost for selected upload renditions and optional original file |
| `POST` | `/videos/upload` | Upload video (multipart: `file`, `title`, `description`, `resolutions`, optional `upload_original`, optional `publish_when_ready`) |
| `GET` | `/videos` | Public list of published ready videos, with filename and manifest address redacted unless enabled |
| `GET` | `/videos/{id}` | Public video detail for playback, without segment addresses |
| `GET` | `/videos/{id}/status` | Public processing status with sensitive addresses redacted |
| `GET` | `/admin/videos` | Admin list of all videos and processing states |
| `GET` | `/admin/videos/{id}` | Admin video detail with variants and segment addresses |
| `PATCH` | `/admin/videos/{id}/publication` | Publish or hide a ready video from the public catalog |
| `PATCH` | `/admin/videos/{id}/visibility` | Publish or hide original filename and manifest address in public responses |
| `POST` | `/admin/videos/{id}/approve` | Approve the final quote and start segment upload/manifest storage |
| `DELETE` | `/admin/videos/{id}` | Delete video record (does not remove data from Autonomi) |
| `GET` | `/catalog` | Admin-only latest catalog address and decoded network catalog |

**Upload example:**
```bash
curl -X POST http://localhost/api/videos/upload \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -F "file=@myvideo.mp4" \
  -F "title=My Video" \
  -F "resolutions=480p,720p" \
  -F "show_original_filename=false" \
  -F "show_manifest_address=false" \
  -F "upload_original=true" \
  -F "publish_when_ready=false"
```

### Rust Streaming (`/stream`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/metrics` | Internal Prometheus-style metrics |
| `GET` | `/stream/{video_id}/{resolution}/playlist.m3u8` | HLS manifest |
| `GET` | `/stream/{video_id}/{resolution}/{index}.ts` | TS segment (proxied from Autonomi) |

---

## Resolution presets

| Label | 16:9 width × height | Video bitrate | Audio bitrate |
|---|---|---|---|
| `8k` | 7,680 × 4,320 | 45,000 kbps | 320 kbps |
| `4k` | 3,840 × 2,160 | 16,000 kbps | 256 kbps |
| `1440p` | 2,560 × 1,440 | 8,000 kbps | 192 kbps |
| `1080p` | 1,920 × 1,080 | 5,000 kbps | 192 kbps |
| `720p` | 1,280 × 720 | 2,500 kbps | 128 kbps |
| `540p` | 960 × 540 | 1,600 kbps | 128 kbps |
| `480p` | 854 × 480 | 1,000 kbps | 128 kbps |
| `360p` | 640 × 360 | 500 kbps | 96 kbps |
| `240p` | 426 × 240 | 300 kbps | 64 kbps |
| `144p` | 256 × 144 | 150 kbps | 48 kbps |

Each label targets that short-edge quality tier while preserving the source
aspect ratio. For example, `1080p` becomes 1,920 × 1,080 for 16:9,
1,080 × 1,920 for 9:16, 1,440 × 1,080 for 4:3, and 1,080 × 1,080 for
square sources. Dimensions are rounded down to even numbers for H.264 and are
capped at the source size to avoid accidental upscaling.
Video bitrates are scaled from the 16:9 baseline by the actual output pixel
count for non-16:9 sources.

HLS segment duration is configurable with `HLS_SEGMENT_DURATION`; the local
example uses `0.75` seconds to keep 4K Autonomi objects small and reliable on
devnets.

---

## Project structure

```
autonomi-video-management/
├── Cargo.toml                  # Root Rust workspace for shared helpers and Rust services
├── .devcontainer/
│   ├── Dockerfile              # Dev image: Rust, Node 24, Python helpers, antd, ant, ant-devnet
│   ├── devcontainer.json       # VS Code dev container config + MCP servers
│   ├── start_autonomi.sh       # postStartCommand: starts local devnet + antd
│   ├── setup_claude.py         # Writes MCP server config to ~/.claude.json
│   └── setup_codex.py          # Registers MCP servers with Codex
├── antd_service/
│   ├── Dockerfile              # Production/default-network antd daemon container
│   └── src/                    # Axum gateway split into client, routes, errors, and state
├── autonomi_devnet/
│   ├── Dockerfile              # Self-contained local ant-devnet + antd testnet
│   └── start-local-devnet.sh
├── common/
│   └── src/lib.rs              # Shared Rust helpers used by multiple services
├── rust_admin/
│   ├── Dockerfile
│   ├── Cargo.toml
│   └── src/                    # Axum admin API split into config, auth, routes, jobs, upload, media, catalog, pipeline, storage, DB, models, state, and antd client
├── rust_stream/
│   ├── Dockerfile
│   ├── Cargo.toml
│   └── src/                    # Axum stream API split into config, cache, HLS, routes, and antd client
├── react_frontend/
│   ├── Dockerfile
│   ├── index.html
│   ├── package.json
│   ├── vite.config.mjs
│   └── src/
│       ├── main.jsx
│       ├── App.jsx             # Root shell and tab composition
│       ├── api/                # Axios API client wrappers
│       ├── components/         # Upload, library, login, quote, and player components
│       ├── hooks/              # Browser auth/session hooks
│       ├── styles/             # Split component-oriented CSS
│       └── utils/              # Formatting, status, and resolution helpers
├── nginx/
│   └── conf.d/default.conf     # Local HTTP reverse proxy
├── postgres-init/
│   └── init-dbs.sh             # Creates databases and users; Rust services apply SQLx migrations
├── docs/
│   ├── DEPLOYMENT.md           # Production deployment guide
│   ├── RUNTIME_MODES.md        # Containerized and future native runtime boundaries
│   └── runtime-contract.example.json # Example native host endpoint/path contract
├── docker-compose.yml          # Base app services
├── docker-compose.local.yml    # Local self-contained Autonomi devnet overlay
├── docker-compose.debug-ports.yml # Optional direct admin/stream debug ports
├── docker-compose.prod.yml     # Production/default-network antd overlay
├── .env.local.example
├── .env.production.example
└── .env.example
```

---

## Autonomi SDK 2.0

This project uses the [ant-sdk](https://github.com/WithAutonomi/ant-sdk) (v2.0) architecture:

- **`antd`** is a local gateway daemon (Rust) that handles all network connectivity, EVM payments, and content addressing. Your application code never touches the Autonomi peer network directly.
- **Admin writes** — `rust_admin` talks to the `antd` REST API for quotes, uploads, wallet approval, and catalog/manifest writes.
- **`antd-client`** (`cargo add antd-client`) — Rust SDK used by the streaming service.
- **`antd-mcp`** — MCP server exposing Autonomi tools to Claude (runs automatically in the devcontainer).
- **Payment modes** — uploads use `ANTD_PAYMENT_MODE=auto` by default, which lets `antd` pick merkle batch payments for larger uploads and single payments otherwise.
- **Wallet approval** — `rust_admin` calls wallet approval on startup when `ANTD_APPROVE_ON_STARTUP=true`, so storage writes fail fast if the configured wallet cannot pay.
- **Upload verification** — `rust_admin` reads uploaded segment data and optional original source data back before publishing a video as ready. This is slower, but prevents bad addresses from reaching the playback manifest/catalog. The default 1-second HLS chunk cadence keeps each local-devnet object small enough to avoid multi-MB storage stalls.

Key operations used in this project:

```rust
// Fetch a segment (Rust)
let bytes = client.data_get_public(&address).await?;
```

---

## License

GNU General Public License v3. See [LICENSE](LICENSE).
