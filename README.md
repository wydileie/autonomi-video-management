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
1. User selects a video file and desired resolutions (360p / 480p / 720p / 1080p).
2. The React frontend POSTs the file to the Python admin service.
3. Python admin saves it to a temp volume and queues a background job.
4. FFmpeg transcodes the video into 10-second HLS `.ts` segments at each resolution.
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
| `nginx` | — | 80, 443 | Reverse proxy, TLS termination |
| `db` | PostgreSQL 16 | 5432 | Upload job/status cache while videos are processing |
| `certbot` | — | — | Let's Encrypt certificate renewal |

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

### Development (Docker Compose)

```bash
cp .env.example .env
# Edit .env — at minimum set AUTONOMI_WALLET_KEY for production,
# or leave it blank for local devnet (wallet is auto-provisioned).

docker compose up --build
```

Services are available at:
- Frontend: `http://localhost` (via Nginx) or `http://localhost:3000` directly
- Admin API: `http://localhost:8000`
- Stream API: `http://localhost:8081`

### Production

See [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

---

## Environment variables

Copy `.env.example` to `.env` and fill in values.

| Variable | Required | Description |
|---|---|---|
| `POSTGRES_USER` / `POSTGRES_PASSWORD` | Yes | PostgreSQL root credentials |
| `ADMIN_DB` / `ADMIN_USER` / `ADMIN_PASS` | Yes | App database credentials |
| `DOMAIN` | Yes | Public domain name for TLS |
| `AUTONOMI_WALLET_KEY` | For writes | Hex-encoded EVM private key (`0x…`) for Autonomi storage payments |
| `ANTD_NETWORK` | No | `default` (mainnet) or `local` (devnet). Default: `default` |
| `ANTD_PAYMENT_MODE` | No | Upload payment strategy: `auto`, `merkle`, or `single`. Default: `auto` |
| `ANTD_APPROVE_ON_STARTUP` | No | Whether `python_admin` runs the one-time wallet spend approval on startup. Default: `true` |
| `CATALOG_ADDRESS` | No | Optional bootstrap address for an existing network-hosted video catalog |
| `AUTONOMI_PEERS` | No | Comma-separated bootstrap multiaddrs (empty = built-in mainnet peers) |
| `EVM_RPC_URL` | Local only | EVM JSON-RPC endpoint (local devnet only) |
| `EVM_PAYMENT_TOKEN_ADDRESS` | Local only | Payment token contract (local devnet only) |
| `EVM_PAYMENT_VAULT_ADDRESS` | Local only | Payment vault contract (local devnet only) |

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
| `360p` | 640 × 360 | 500 kbps | 96 kbps |
| `480p` | 854 × 480 | 1,000 kbps | 128 kbps |
| `720p` | 1,280 × 720 | 2,500 kbps | 128 kbps |
| `1080p` | 1,920 × 1,080 | 5,000 kbps | 192 kbps |

HLS segment duration: 10 seconds.

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
│   └── Dockerfile              # Production antd daemon container (builds from source)
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
│   └── conf.d/default.conf     # Reverse proxy + TLS
├── postgres-init/
│   └── init-dbs.sh             # Creates databases, users, and video schema
├── docs/
│   └── DEPLOYMENT.md           # Production deployment guide
├── docker-compose.yml
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
