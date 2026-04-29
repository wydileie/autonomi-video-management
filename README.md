# Autonomi Video Management

A self-hosted, decentralised video management platform. Upload videos, transcode them into adaptive HLS streams at multiple resolutions, and store every segment permanently on the [Autonomi](https://autonomi.com) network. Playback is served directly from the network — no CDN, no single point of failure, pay-once storage.

## How it works

```
Browser
  │
  ├── Upload video ──► Python Admin (FastAPI)
  │                        │
  │                        ├─ FFmpeg → HLS .ts segments per resolution
  │                        │
  │                        └─ antd-py SDK ──► antd daemon ──► Autonomi network
  │                                               (stores each segment, returns address)
  │
  └── Play video ───► Rust Streaming (Axum)
                           │
                           ├─ Reads catalog + video manifests from Autonomi
                           │
                           └─ antd-client SDK ──► antd daemon ──► Autonomi network
                                                      (fetches manifests and segments on demand)
```

### Upload flow
1. User drops or selects a video file; the browser detects its source resolution and offers 8K / 4K / 1080P / 720p / 480P / 360P renditions without upscaling by default.
2. The React frontend POSTs the file to the Python admin service.
3. Python admin saves it to the configured processing bind mount, records the durable job inputs in Postgres, and queues a worker task.
4. FFmpeg transcodes the video into small HLS `.ts` segments at each resolution.
5. Python admin sums the actual transcoded segment sizes and asks `antd` for a final quote using the real segment bytes.
6. The job pauses as `awaiting_approval`; the frontend shows the final quote and expiry time.
7. After approval, every segment is uploaded to the Autonomi network via the `antd` daemon using `data_put_public`.
8. A video manifest JSON containing resolution, segment order, durations, and Autonomi addresses is stored on Autonomi.
9. The job status flips to `ready`. Admins can then publish or unpublish the video from the public library.
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
| `python_admin` | Python / FastAPI | 8000 | Video upload, FFmpeg transcoding, metadata API |
| `rust_admin` | Rust / Axum | 8000 (8002 debug) | Experimental admin API migration target |
| `rust_stream` | Rust / Axum | 8081 | HLS manifest generation + Autonomi segment proxy |
| `react_frontend` | React | 80 | Upload UI, video library, HLS player |
| `nginx` | — | 80 | Reverse proxy for the frontend, admin API, and stream API |
| `db` | PostgreSQL 16 | 5432 | Upload job/status cache and worker recovery state |

### URL routing (via Nginx)

| Path | Proxies to |
|---|---|
| `/` | React frontend |
| `/api/*` | Python admin (path rewritten to `/*`) |
| `/stream/*` | Rust streaming service |

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
# Install Python admin dependencies and run unittest discovery
make install-python
make test-python

# Run the Rust streaming service tests
make test-rust
make test-rust-admin

# Install, build, and test the React frontend
make install-react
make build-react
make test-react

# Run all local test targets
make test

# Run the full CI-shaped sequence, including dependency installs
make ci
```

The Python target runs `python -m unittest discover` under `python_admin/tests`.
The Rust stream and experimental Rust admin targets run `cargo test` and
`cargo clippy` under `rust_stream` and `rust_admin`. The React target runs the
Vite/Vitest test command once.

---

## Running the application stack

The app is intended to run as a containerized stack. Use the base Compose file
plus one overlay:

- `docker-compose.local.yml` runs a self-contained local Autonomi devnet for testing.
- `docker-compose.prod.yml` runs `antd` against the configured Autonomi network.

Compose remains the supported deployment runtime. The repo also documents the
service boundary expected by a future native packaged host so native work can
reuse the same Python admin, Rust stream, `antd`, Postgres, endpoint, and
storage-path contracts without changing current containers. See
[`docs/RUNTIME_MODES.md`](docs/RUNTIME_MODES.md) and the machine-readable
[`docs/runtime-contract.example.json`](docs/runtime-contract.example.json).

`docker-compose.rust-admin.yml` is an optional migration overlay. When included,
it starts `rust_admin` beside the stable services and swaps Nginx `/api/*`
routing from `python_admin` to `rust_admin` for parity testing. Leave the overlay
out to keep the current Python-backed runtime.

### Local Testnet

```bash
cp .env.local.example .env.local

# Optional but recommended for large uploads: point processing files at a
# large, persistent host disk.
# VIDEO_PROCESSING_HOST_PATH=/mnt/video-processing/autvid

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

To test the Rust admin migration target through the same browser-facing `/api`
contract, include the Rust admin overlay:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.rust-admin.yml \
  up --build
```

The overlay also publishes `rust_admin` directly at
`http://localhost:${RUST_ADMIN_HTTP_PORT:-8002}`. It currently matches health,
auth, quote, catalog, and read/status APIs; upload/transcode, approval,
publication, and delete still intentionally return `501` until those workflows
are migrated from `python_admin`.

### Production

```bash
cp .env.production.example .env.production
# Fill in PROD_AUTONOMI_WALLET_KEY and any network/payment settings.

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
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
| `VIDEO_PROCESSING_HOST_PATH` | Recommended | Host path bind-mounted for original uploads and transcoded segment files while jobs are processing, awaiting approval, or resuming after a restart. A one-shot Compose init container creates and chowns it for the non-root admin service user |
| `DOMAIN` | No | Domain label for external proxies or deployment tooling |
| `APP_HTTP_PORT` | No | Host port for Nginx, the only app-facing port published by the production compose path |
| `ADMIN_HTTP_PORT` / `STREAM_HTTP_PORT` | Local/debug only | Direct host ports for the admin and stream services when using a local/debug compose override |
| `CORS_ALLOWED_ORIGINS` | No | Comma-separated explicit browser origins allowed to call admin/stream directly. Wildcard `*` is rejected |
| `ANTD_REST_PORT` / `ANTD_GRPC_PORT` | Local/debug only | Direct host ports for the Autonomi gateway when using a local/debug compose override |
| `PROD_AUTONOMI_WALLET_KEY` | Production writes | Hex-encoded EVM private key (`0x...`) for Autonomi storage payments |
| `PROD_ANTD_NETWORK` | Production | `default` unless you are targeting a custom network |
| `PROD_AUTONOMI_PEERS` | Production/custom | Comma-separated bootstrap multiaddrs |
| `ANT_DEVNET_PRESET` | Local only | Local devnet size: `minimal`, `small`, or `default` |
| `ANTD_PAYMENT_MODE` | No | Upload payment strategy: `auto`, `merkle`, or `single`. Default: `auto` |
| `ANTD_UPLOAD_VERIFY` | No | Read each uploaded segment back before publishing the manifest. Default: `true` |
| `ANTD_UPLOAD_RETRIES` | No | Number of upload/verify attempts per segment. Default: `3` |
| `ANTD_UPLOAD_TIMEOUT_SECONDS` | No | Per upload/read-back timeout before retrying a segment. Default: `120` |
| `ANTD_APPROVE_ON_STARTUP` | No | Whether `python_admin` runs the one-time wallet spend approval on startup. Default: `true` |
| `UPLOAD_MAX_FILE_BYTES` | No | Max accepted source upload bytes. Default: `21474836480` (20 GiB) |
| `UPLOAD_MAX_DURATION_SECONDS` | No | Max accepted source duration from server-side `ffprobe`. Default: `14400` |
| `UPLOAD_MAX_SOURCE_PIXELS` | No | Max accepted source pixel count after rotation metadata. Default: `33177600` (8K) |
| `UPLOAD_MAX_SOURCE_LONG_EDGE` | No | Max accepted source long edge in pixels after rotation metadata. Default: `7680` |
| `UPLOAD_MIN_FREE_BYTES` | No | Processing-disk free-space headroom required before and during upload writes. Default: `5368709120` (5 GiB) |
| `UPLOAD_MAX_CONCURRENT_SAVES` | No | Max concurrent source uploads being streamed/probed to disk. Default: `2` |
| `UPLOAD_FFPROBE_TIMEOUT_SECONDS` | No | Server-side upload validation `ffprobe` timeout. Default: `30` |
| `HLS_SEGMENT_DURATION` | No | Target seconds per forced-keyframe HLS segment. Default: `1` |
| `FFMPEG_THREADS` | No | Maximum FFmpeg/x264 encoder threads. Default: `2` to reduce memory use for high-resolution portrait transcodes |
| `FFMPEG_FILTER_THREADS` | No | Maximum FFmpeg filter graph threads. Default: `1` |
| `FINAL_QUOTE_APPROVAL_TTL_SECONDS` | No | Seconds before an unapproved final quote expires and local transcoded files are deleted. Default: `14400` |
| `APPROVAL_CLEANUP_INTERVAL_SECONDS` | No | Seconds between cleanup scans for expired final quotes. Default: `300` |
| `CATALOG_ADDRESS` | No | Optional bootstrap address for an existing network-hosted video catalog |
| `STREAM_CATALOG_CACHE_TTL_SECONDS` | No | Rust stream catalog metadata cache TTL. Default: `10`; set `0` to disable |
| `STREAM_MANIFEST_CACHE_TTL_SECONDS` | No | Rust stream video manifest cache TTL. Default: `300`; set `0` to disable |
| `STREAM_SEGMENT_CACHE_TTL_SECONDS` | No | Rust stream segment byte cache TTL and response `Cache-Control` max-age. Default: `60`; set `0` to disable in-process segment caching |
| `STREAM_SEGMENT_CACHE_MAX_BYTES` | No | Rust stream in-process segment cache byte cap. Default: `67108864` |
| `PROD_EVM_RPC_URL` | Production/custom | EVM JSON-RPC endpoint for custom payment networks |
| `PROD_EVM_PAYMENT_TOKEN_ADDRESS` | Production/custom | Payment token contract for custom payment networks |
| `PROD_EVM_PAYMENT_VAULT_ADDRESS` | Production/custom | Payment vault contract for custom payment networks |

---

## Database schema

Managed by the Python admin service (`_ensure_schema()` on startup) and seeded by [`postgres-init/init-dbs.sh`](postgres-init/init-dbs.sh). Postgres is no longer the source of truth for ready video playback; it is a processing/status cache and recovery log for interrupted local jobs.

```
videos
  id, title, original_filename, description, status, job_dir,
  job_source_path, requested_resolutions, created_at, user_id

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

### Python Admin (`/api`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `POST` | `/auth/login` | Sign in as the single admin user |
| `GET` | `/auth/me` | Validate the current admin bearer token |
| `POST` | `/videos/upload/quote` | Estimate Autonomi storage and gas cost for selected upload renditions |
| `POST` | `/videos/upload` | Upload video (multipart: `file`, `title`, `description`, `resolutions`) |
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
  -F "show_manifest_address=false"
```

### Rust Streaming (`/stream`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/stream/{video_id}/{resolution}/playlist.m3u8` | HLS manifest |
| `GET` | `/stream/{video_id}/{resolution}/{index}.ts` | TS segment (proxied from Autonomi) |

---

## Resolution presets

| Label | Landscape width × height | Video bitrate | Audio bitrate |
|---|---|---|---|
| `8k` | 7,680 × 4,320 | 45,000 kbps | 320 kbps |
| `4k` | 3,840 × 2,160 | 16,000 kbps | 256 kbps |
| `360p` | 640 × 360 | 500 kbps | 96 kbps |
| `480p` | 854 × 480 | 1,000 kbps | 128 kbps |
| `720p` | 1,280 × 720 | 2,500 kbps | 128 kbps |
| `1080p` | 1,920 × 1,080 | 5,000 kbps | 192 kbps |

Portrait sources use the same long-edge target for each label with width and
height swapped. For example, a vertical `4k` rendition is encoded as
2,160 × 3,840 rather than padded into a 3,840 × 2,160 landscape frame.

HLS segment duration is configurable with `HLS_SEGMENT_DURATION`; the local
default is `1` second to keep Autonomi objects small and reliable on devnets.

---

## Project structure

```
autonomi-video-management/
├── .devcontainer/
│   ├── Dockerfile              # Dev image: Python 3.12, Rust, Node 24, antd, ant, ant-devnet
│   ├── devcontainer.json       # VS Code dev container config + MCP servers
│   ├── start_autonomi.sh       # postStartCommand: starts local devnet + antd
│   ├── setup_claude.py         # Writes MCP server config to ~/.claude.json
│   └── setup_codex.py          # Registers MCP servers with Codex
├── antd_service/
│   └── Dockerfile              # Production/default-network antd daemon container
├── autonomi_devnet/
│   ├── Dockerfile              # Self-contained local ant-devnet + antd testnet
│   └── start-local-devnet.sh
├── python_admin/
│   ├── Dockerfile
│   ├── requirements.txt
│   └── src/admin_service.py    # FastAPI: upload, transcode, Autonomi upload, metadata API
├── rust_admin/
│   ├── Dockerfile
│   ├── Cargo.toml
│   └── src/main.rs             # Axum: experimental admin API migration target
├── rust_stream/
│   ├── Dockerfile
│   ├── Cargo.toml
│   └── src/main.rs             # Axum: HLS manifest + Autonomi segment proxy
├── react_frontend/
│   ├── Dockerfile
│   ├── index.html
│   ├── package.json
│   ├── vite.config.mjs
│   └── src/
│       ├── main.jsx
│       └── App.jsx             # Upload form, video library, hls.js player
├── nginx/
│   └── conf.d/default.conf     # Local HTTP reverse proxy
├── postgres-init/
│   └── init-dbs.sh             # Creates databases, users, and video schema
├── docs/
│   ├── DEPLOYMENT.md           # Production deployment guide
│   ├── RUNTIME_MODES.md        # Containerized and future native runtime boundaries
│   └── runtime-contract.example.json # Example native host endpoint/path contract
├── docker-compose.yml          # Base app services
├── docker-compose.local.yml    # Local self-contained Autonomi devnet overlay
├── docker-compose.rust-admin.yml # Optional overlay routing /api to rust_admin
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
- **`antd-py`** (`pip install antd[rest]`) — Python SDK used by the admin service.
- **`antd-client`** (`cargo add antd-client`) — Rust SDK used by the streaming service.
- **`antd-mcp`** — MCP server exposing Autonomi tools to Claude (runs automatically in the devcontainer).
- **Payment modes** — uploads use `ANTD_PAYMENT_MODE=auto` by default, which lets `antd` pick merkle batch payments for larger uploads and single payments otherwise.
- **Wallet approval** — `python_admin` calls `wallet_approve()` on startup when `ANTD_APPROVE_ON_STARTUP=true`, so storage writes fail fast if the configured wallet cannot pay.
- **Segment verification** — `python_admin` reads uploaded segment data back before publishing a video as ready. This is slower, but prevents bad segment addresses from reaching the playback catalog. The default 1-second HLS chunk cadence keeps each local-devnet object small enough to avoid multi-MB storage stalls.

Key operations used in this project:

```python
# Store a segment (Python)
result = await client.data_put_public(segment_bytes, payment_mode="auto")
```
```rust
// Fetch a segment (Rust)
let bytes = client.data_get_public(&address).await?;
```

---

## License

GNU General Public License v3. See [LICENSE](LICENSE).
