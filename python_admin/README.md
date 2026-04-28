# Python Admin Service

FastAPI service responsible for video ingestion, FFmpeg transcoding, Autonomi network upload, and video metadata management.

## Responsibilities

1. Accept video file uploads from the React frontend.
2. Run FFmpeg to produce HLS `.ts` segments at each requested resolution.
3. Upload every segment to the Autonomi network via the `antd` daemon.
4. Publish a video manifest and catalog snapshot to Autonomi.
5. Use PostgreSQL only as a local upload/job status cache.
6. Expose a REST API for the frontend to poll status and retrieve video details.

## API

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `POST` | `/videos/upload` | Upload + queue transcoding (multipart form) |
| `GET` | `/videos` | List all videos |
| `GET` | `/videos/{id}` | Video detail with variants and segment addresses |
| `GET` | `/videos/{id}/status` | Poll status: `pending` / `processing` / `ready` / `error` |
| `DELETE` | `/videos/{id}` | Delete video record |
| `GET` | `/catalog` | Latest catalog address and decoded Autonomi catalog |

### Upload parameters

| Field | Type | Description |
|---|---|---|
| `file` | file | Video file (any format FFmpeg supports) |
| `title` | string | Display title |
| `description` | string | Optional description |
| `resolutions` | string | Comma-separated: `8k`, `4k`, `1080p`, `720p`, `480p`, `360p` |

## Configuration (environment variables)

| Variable | Description |
|---|---|
| `ADMIN_DB_HOST` | Postgres host |
| `ADMIN_DB_PORT` | Postgres port (default: 5432) |
| `ADMIN_DB_NAME` | Database name |
| `ADMIN_DB_USER` | Database user |
| `ADMIN_DB_PASS` | Database password |
| `ANTD_URL` | antd daemon REST URL (default: `http://localhost:8082`) |
| `ANTD_PAYMENT_MODE` | Autonomi payment mode for segment uploads: `auto`, `merkle`, or `single` (default: `auto`) |
| `ANTD_UPLOAD_VERIFY` | Read each uploaded segment back before publishing a ready manifest (default: `true`) |
| `ANTD_UPLOAD_RETRIES` | Upload/read-back attempts per segment (default: `3`) |
| `ANTD_UPLOAD_TIMEOUT_SECONDS` | Per upload/read-back timeout before retrying (default: `120`) |
| `ANTD_APPROVE_ON_STARTUP` | Run one-time wallet spend approval during startup (default: `true`) |
| `HLS_SEGMENT_DURATION` | Forced-keyframe segment length in seconds (default: `1`) |
| `CATALOG_ADDRESS` | Optional bootstrap address for an existing network-hosted catalog |
| `CATALOG_STATE_PATH` | Local bookmark file for the latest catalog address |
| `UPLOAD_TEMP_DIR` | Temporary directory for uploads and segments (default: `/tmp/video_uploads`) |

## Dependencies

- `fastapi` — HTTP framework
- `uvicorn` — ASGI server
- `asyncpg` — Async PostgreSQL driver
- `aiofiles` — Async file I/O
- `httpx` — talks to the `antd` daemon REST API
- `python-multipart` — File upload support
- FFmpeg — system binary; must be in `PATH`

## Local development

```bash
cd python_admin
pip install -r requirements.txt

# Set required env vars (or use a .env file with python-dotenv)
export ADMIN_DB_HOST=localhost ADMIN_DB_NAME=admindb \
       ADMIN_DB_USER=admin ADMIN_DB_PASS=AdminSecurePass \
       ANTD_URL=http://localhost:8082

uvicorn src.admin_service:app --reload --port 8000
```

## Processing pipeline detail

```
POST /videos/upload/quote
  → estimate transcoded HLS bytes for the selected resolutions
  → ask antd for current /v1/data/cost storage quotes
  → return total storage/gas estimate before upload starts

POST /videos/upload
  → save file to /tmp/video_uploads/{video_id}/original_<name>
  → INSERT into videos (status=pending)
  → BackgroundTask: _process_video()
      → for each resolution:
          → FFmpeg → seg_00000.ts, seg_00001.ts, …  (10s HLS segments)
          → INSERT video_variants row
          → for each .ts file:
              → AsyncAntdClient.data_put_public(bytes, payment_mode=...)
              → INSERT video_segments row with address, cost, duration
          → Store video manifest JSON on Autonomi
          → Store updated catalog JSON on Autonomi
          → Write latest catalog address bookmark to CATALOG_STATE_PATH
      → UPDATE videos SET status='ready', manifest_address=..., catalog_address=...
  → cleanup /tmp/video_uploads/{video_id}/
```

FFmpeg flags produce H.264/AAC MPEG-TS segments with correct aspect ratio padding to fit the target resolution box.
