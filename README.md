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
3. Python admin saves it to a temp volume and queues a background job.
4. FFmpeg transcodes the video into small HLS `.ts` segments at each resolution.
5. Every segment is uploaded to the Autonomi network via the `antd` daemon using `data_put_public`.
6. A video manifest JSON containing resolution, segment order, durations, and Autonomi addresses is stored on Autonomi.
7. A catalog JSON containing the video list and manifest addresses is stored on Autonomi. Until `antd` exposes mutable pointers/scratchpads, the latest catalog address is bookmarked in the shared `catalog_state` volume.
8. The job status flips to `ready`. The frontend polls and then activates the player.

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
| `rust_stream` | Rust / Axum | 8081 | HLS manifest generation + Autonomi segment proxy |
| `react_frontend` | React | 80 | Upload UI, video library, HLS player |
| `nginx` | — | 80 | Reverse proxy for the frontend, admin API, and stream API |
| `db` | PostgreSQL 16 | 5432 | Upload job/status cache while videos are processing |

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

## Running the application stack

The app is intended to run as a containerized stack. Use the base Compose file
plus one overlay:

- `docker-compose.local.yml` runs a self-contained local Autonomi devnet for testing.
- `docker-compose.prod.yml` runs `antd` against the configured Autonomi network.

### Local Testnet

```bash
cp .env.local.example .env.local

# If running from inside a devcontainer with OrbStack/Docker Desktop, set this
# to the host-machine repo path, not /workspaces/...
# HOST_WORKSPACE_DIR=/Users/you/path/to/autonomi-video-management

docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build
```

Services are available at:
- Frontend: `http://localhost` via Nginx
- Admin API: `http://localhost:8000`
- Stream API: `http://localhost:8081`
- Autonomi gateway: `http://localhost:8082`

### Production

```bash
cp .env.production.example .env.production
# Fill in PROD_AUTONOMI_WALLET_KEY and any network/payment settings.

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  up --build -d
```

For public deployments, put TLS, authentication, and domain routing in front of
the stack with your preferred reverse proxy or hosting platform.

---

## Environment variables

Start from `.env.local.example` for local testing or `.env.production.example`
for deployment. `.env.example` contains the full variable set in one file.

| Variable | Required | Description |
|---|---|---|
| `POSTGRES_USER` / `POSTGRES_PASSWORD` | Yes | PostgreSQL root credentials |
| `ADMIN_DB` / `ADMIN_USER` / `ADMIN_PASS` | Yes | App database credentials |
| `HOST_WORKSPACE_DIR` | Devcontainer/host Docker | Host-machine repo path used for Compose bind mounts |
| `DOMAIN` | No | Domain label for external proxies or deployment tooling |
| `APP_HTTP_PORT` / `ADMIN_HTTP_PORT` / `STREAM_HTTP_PORT` | No | Host ports for Nginx, admin API, and stream API |
| `ANTD_REST_PORT` / `ANTD_GRPC_PORT` | No | Host ports for the Autonomi gateway |
| `PROD_AUTONOMI_WALLET_KEY` | Production writes | Hex-encoded EVM private key (`0x...`) for Autonomi storage payments |
| `PROD_ANTD_NETWORK` | Production | `default` unless you are targeting a custom network |
| `PROD_AUTONOMI_PEERS` | Production/custom | Comma-separated bootstrap multiaddrs |
| `ANT_DEVNET_PRESET` | Local only | Local devnet size: `minimal`, `small`, or `default` |
| `ANTD_PAYMENT_MODE` | No | Upload payment strategy: `auto`, `merkle`, or `single`. Default: `auto` |
| `ANTD_UPLOAD_VERIFY` | No | Read each uploaded segment back before publishing the manifest. Default: `true` |
| `ANTD_UPLOAD_RETRIES` | No | Number of upload/verify attempts per segment. Default: `3` |
| `ANTD_UPLOAD_TIMEOUT_SECONDS` | No | Per upload/read-back timeout before retrying a segment. Default: `120` |
| `ANTD_APPROVE_ON_STARTUP` | No | Whether `python_admin` runs the one-time wallet spend approval on startup. Default: `true` |
| `HLS_SEGMENT_DURATION` | No | Target seconds per forced-keyframe HLS segment. Default: `1` |
| `CATALOG_ADDRESS` | No | Optional bootstrap address for an existing network-hosted video catalog |
| `PROD_EVM_RPC_URL` | Production/custom | EVM JSON-RPC endpoint for custom payment networks |
| `PROD_EVM_PAYMENT_TOKEN_ADDRESS` | Production/custom | Payment token contract for custom payment networks |
| `PROD_EVM_PAYMENT_VAULT_ADDRESS` | Production/custom | Payment vault contract for custom payment networks |

---

## Database schema

Managed by the Python admin service (`_ensure_schema()` on startup) and seeded by [`postgres-init/init-dbs.sh`](postgres-init/init-dbs.sh). Postgres is no longer the source of truth for ready video playback; it is a processing/status cache.

```
videos
  id, title, original_filename, description, status, created_at, user_id

video_variants          (one per resolution per video)
  id, video_id → videos, resolution, width, height,
  video_bitrate, audio_bitrate, segment_duration, total_duration, segment_count

video_segments          (one per .ts chunk per variant)
  id, variant_id → video_variants, segment_index,
  autonomi_address, duration, byte_size

Autonomi stores the durable playback metadata:

video manifest
  id, title, variants[], segments[] with Autonomi addresses and durations

catalog manifest
  videos[] with video id, title, variant summaries, and video manifest address
```

---

## API reference

### Python Admin (`/api`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `POST` | `/videos/upload/quote` | Estimate Autonomi storage and gas cost for selected upload renditions |
| `POST` | `/videos/upload` | Upload video (multipart: `file`, `title`, `description`, `resolutions`) |
| `GET` | `/videos` | List all videos |
| `GET` | `/videos/{id}` | Get video with variants and segment addresses |
| `GET` | `/videos/{id}/status` | Poll processing status (`pending` / `processing` / `ready` / `error`) |
| `DELETE` | `/videos/{id}` | Delete video record (does not remove data from Autonomi) |
| `GET` | `/catalog` | Return the latest catalog address and decoded network catalog |

**Upload example:**
```bash
curl -X POST http://localhost:8000/videos/upload \
  -F "file=@myvideo.mp4" \
  -F "title=My Video" \
  -F "resolutions=480p,720p"
```

### Rust Streaming (`/stream`)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/stream/{video_id}/{resolution}/playlist.m3u8` | HLS manifest |
| `GET` | `/stream/{video_id}/{resolution}/{index}.ts` | TS segment (proxied from Autonomi) |

---

## Resolution presets

| Label | Width × Height | Video bitrate | Audio bitrate |
|---|---|---|---|
| `8k` | 7,680 × 4,320 | 45,000 kbps | 320 kbps |
| `4k` | 3,840 × 2,160 | 16,000 kbps | 256 kbps |
| `360p` | 640 × 360 | 500 kbps | 96 kbps |
| `480p` | 854 × 480 | 1,000 kbps | 128 kbps |
| `720p` | 1,280 × 720 | 2,500 kbps | 128 kbps |
| `1080p` | 1,920 × 1,080 | 5,000 kbps | 192 kbps |

HLS segment duration is configurable with `HLS_SEGMENT_DURATION`; the local
default is `1` second to keep Autonomi objects small and reliable on devnets.

---

## Project structure

```
autonomi-video-management/
├── .devcontainer/
│   ├── Dockerfile              # Dev image: Python 3.12, Rust, Node 18, antd, ant, ant-devnet
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
├── rust_stream/
│   ├── Dockerfile
│   ├── Cargo.toml
│   └── src/main.rs             # Axum: HLS manifest + Autonomi segment proxy
├── react_frontend/
│   ├── Dockerfile
│   ├── package.json
│   ├── public/index.html
│   └── src/
│       ├── index.js
│       └── App.js              # Upload form, video library, hls.js player
├── nginx/
│   └── conf.d/default.conf     # Local HTTP reverse proxy
├── postgres-init/
│   └── init-dbs.sh             # Creates databases, users, and video schema
├── docs/
│   └── DEPLOYMENT.md           # Production deployment guide
├── docker-compose.yml          # Base app services
├── docker-compose.local.yml    # Local self-contained Autonomi devnet overlay
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
