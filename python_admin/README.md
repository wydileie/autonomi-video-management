# Python Admin Service

FastAPI service responsible for video ingestion, FFmpeg transcoding, Autonomi network upload, and video metadata management.

## Responsibilities

1. Accept video file uploads from the React frontend.
2. Run FFmpeg to produce HLS `.ts` segments at each requested resolution.
3. Quote the real transcoded segment files and wait for user approval.
4. Upload every approved segment to the Autonomi network via the `antd` daemon.
5. Publish a video manifest and catalog snapshot to Autonomi.
6. Use PostgreSQL as a local upload/job status cache and recovery log.
7. Expose a REST API for the frontend to poll status and retrieve video details.

## API

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `POST` | `/auth/login` | Single admin login |
| `GET` | `/auth/me` | Validate admin bearer token |
| `POST` | `/videos/upload` | Upload + queue transcoding (multipart form) |
| `GET` | `/videos` | Public ready-video list with sensitive metadata redacted |
| `GET` | `/videos/{id}` | Public playback detail without segment addresses |
| `GET` | `/videos/{id}/status` | Public status with sensitive addresses redacted |
| `GET` | `/admin/videos` | Admin list of all videos |
| `GET` | `/admin/videos/{id}` | Admin detail with variants and segment addresses |
| `PATCH` | `/admin/videos/{id}/visibility` | Publish or hide original filename and manifest address |
| `POST` | `/admin/videos/{id}/approve` | Approve the final quote and start Autonomi upload |
| `DELETE` | `/admin/videos/{id}` | Delete video record |
| `GET` | `/catalog` | Admin-only latest catalog address and decoded Autonomi catalog |

### Upload parameters

| Field | Type | Description |
|---|---|---|
| `file` | file | Video file (any format FFmpeg supports) |
| `title` | string | Display title |
| `description` | string | Optional description |
| `resolutions` | string | Comma-separated: `8k`, `4k`, `1440p`, `1080p`, `720p`, `540p`, `480p`, `360p`, `240p`, `144p` |
| `show_original_filename` | boolean | Publish the source filename to public web users |
| `show_manifest_address` | boolean | Publish the backend Autonomi manifest address to public web users |

## Configuration (environment variables)

| Variable | Description |
|---|---|
| `ADMIN_DB_HOST` | Postgres host |
| `ADMIN_DB_PORT` | Postgres port (default: 5432) |
| `ADMIN_DB_NAME` | Database name |
| `ADMIN_DB_USER` | Database user |
| `ADMIN_DB_PASS` | Database password |
| `ADMIN_USERNAME` | Single uploader/admin username |
| `ADMIN_PASSWORD` | Single uploader/admin password |
| `ADMIN_AUTH_SECRET` | Long random secret for signing admin login tokens |
| `ADMIN_AUTH_TTL_HOURS` | Admin token lifetime in hours (default: `12`) |
| `ANTD_URL` | antd daemon REST URL (default: `http://localhost:8082`) |
| `ANTD_PAYMENT_MODE` | Autonomi payment mode for segment uploads: `auto`, `merkle`, or `single` (default: `auto`) |
| `ANTD_UPLOAD_VERIFY` | Read each uploaded segment back before publishing a ready manifest (default: `true`) |
| `ANTD_UPLOAD_RETRIES` | Upload/read-back attempts per segment (default: `3`) |
| `ANTD_UPLOAD_TIMEOUT_SECONDS` | Per upload/read-back timeout before retrying (default: `120`) |
| `ANTD_APPROVE_ON_STARTUP` | Run one-time wallet spend approval during startup (default: `true`) |
| `UPLOAD_MAX_FILE_BYTES` | Max accepted source upload bytes (default: `21474836480`, 20 GiB) |
| `UPLOAD_MAX_DURATION_SECONDS` | Max accepted source duration according to `ffprobe` (default: `14400`, 4 hours) |
| `UPLOAD_MAX_SOURCE_PIXELS` | Max accepted source pixel count after rotation metadata (default: `33177600`, 8K) |
| `UPLOAD_MAX_SOURCE_LONG_EDGE` | Max accepted source long edge in pixels after rotation metadata (default: `7680`) |
| `UPLOAD_MIN_FREE_BYTES` | Required free processing-disk headroom before and during upload writes (default: `5368709120`, 5 GiB) |
| `UPLOAD_MAX_CONCURRENT_SAVES` | Max concurrent source files being streamed/probed to disk (default: `2`) |
| `UPLOAD_FFPROBE_TIMEOUT_SECONDS` | Timeout for server-side upload validation probe (default: `30`) |
| `HLS_SEGMENT_DURATION` | Forced-keyframe segment length in seconds (default: `1`) |
| `FINAL_QUOTE_APPROVAL_TTL_SECONDS` | Seconds before an unapproved final quote expires and local transcoded files are deleted (default: `14400`) |
| `APPROVAL_CLEANUP_INTERVAL_SECONDS` | Seconds between cleanup scans for expired final quotes (default: `300`) |
| `CATALOG_ADDRESS` | Optional bootstrap address for an existing network-hosted catalog |
| `CATALOG_STATE_PATH` | Local bookmark file for the latest catalog address |
| `UPLOAD_TEMP_DIR` | Container path for uploads and segments (Compose sets this to `/var/lib/autvid/processing`, backed by `VIDEO_PROCESSING_HOST_PATH`) |

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

## Tests

From the repository root, the smoke and robustness tests use FastAPI's
in-process test client with fake Postgres and Autonomi clients, so they do not
require Docker services:

```bash
python -m unittest discover -s python_admin/tests -v
```

Real Postgres integration tests are included but skipped unless a test database
DSN is provided. They create and drop a temporary schema inside that database,
and the database user must be able to create schemas and the `uuid-ossp`
extension:

```bash
export PYTHON_ADMIN_POSTGRES_TEST_DSN='postgresql://user:pass@localhost:5432/admin_test'
python -m unittest python_admin.tests.test_admin_postgres_integration -v
```

## Processing pipeline detail

```
POST /videos/upload/quote
  → requires Authorization: Bearer <admin token>
  → estimate transcoded HLS bytes for the selected resolutions
  → ask antd for current /v1/data/cost storage quotes
  → return total storage/gas estimate before upload starts

POST /videos/upload
  → requires Authorization: Bearer <admin token>
  → sanitize the source filename
  → stream file to $UPLOAD_TEMP_DIR/{video_id}/original_<name>.uploading
      while enforcing file-size, disk-space, and concurrent-upload limits
  → validate source duration and display resolution with ffprobe
  → rename validated source to $UPLOAD_TEMP_DIR/{video_id}/original_<name>
  → INSERT into videos (status=pending, job_source_path, requested_resolutions)
  → queue worker task: _process_video()
      → for each resolution:
          → FFmpeg → seg_00000.ts, seg_00001.ts, …  (configured HLS segments)
          → INSERT video_variants row
          → INSERT video_segments rows with local file path, byte size, duration
      → ask antd for final cost using the actual .ts segment bytes
      → UPDATE videos SET status='awaiting_approval', final_quote=...

POST /admin/videos/{id}/approve
  → requires Authorization: Bearer <admin token>
  → UPDATE videos SET status='uploading'
  → queue worker task: _upload_approved_video()
      → for each .ts file:
          → AsyncAntdClient.data_put_public(bytes, payment_mode=...)
          → UPDATE video_segments row with address, cost, duration
      → Store video manifest JSON on Autonomi
      → Store updated catalog JSON on Autonomi
      → Write latest catalog address bookmark to CATALOG_STATE_PATH
      → UPDATE videos SET status='ready', manifest_address=..., catalog_address=...
  → cleanup $UPLOAD_TEMP_DIR/{video_id}/
```

On startup the service scans `pending`, `processing`, and `uploading` rows.
Interrupted transcodes are restarted from the saved source file and requested
resolutions; interrupted uploads resume from persisted segment rows and skip
segments that already have Autonomi addresses. In Compose deployments,
`VIDEO_PROCESSING_HOST_PATH` must point at persistent host storage with enough
free space for original uploads plus all requested transcoded renditions.

FFmpeg flags produce H.264/AAC MPEG-TS segments with source aspect ratio preserved at each selected quality tier.
