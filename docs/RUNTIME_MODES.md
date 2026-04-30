# Runtime Modes

This repository's supported runtime is still the Docker Compose stack. The
native packaged app is a future packaging target, not a second implementation.
Any native host should keep the same backend boundaries and supply the same
endpoints, environment, and persistent paths that Compose supplies today.

See [`runtime-contract.example.json`](runtime-contract.example.json) for a
machine-readable example of the host contract a native launcher could provide.

## Current Mode: Containerized Compose

Compose remains the source of truth for local and production deployment. The
base file starts the shared services and each overlay selects the Autonomi
network mode:

| File | Role |
|---|---|
| `docker-compose.yml` | Base app services, internal network, volumes, Nginx routing, health checks |
| `docker-compose.local.yml` | Self-contained local Autonomi devnet and `antd` for testing |
| `docker-compose.prod.yml` | Production/default-network `antd` daemon configuration |
| `docker-compose.debug-ports.yml` | Optional direct host ports for admin and stream debugging |

The public HTTP surface is Nginx on `${APP_HTTP_PORT:-80}`:

| Public path | Internal service |
|---|---|
| `/` | React frontend |
| `/api/*` | `rust_admin:8000`, with `/api` stripped before proxying |
| `/stream/*` | `rust_stream:8081`, path preserved |

Important Compose-provided runtime glue:

| Concern | Compose value |
|---|---|
| Admin API bind | `rust_admin` listens on `0.0.0.0:8000` |
| Stream API bind | Rust service listens on `0.0.0.0:8081` |
| Autonomi gateway | `ANTD_URL=http://antd:8082` for both backend services |
| Postgres | `ADMIN_DB_HOST=db`, `ADMIN_DB_PORT=5432`, app DB/user/pass from env |
| Processing storage | `${VIDEO_PROCESSING_HOST_PATH:-./.autvid/video_processing}` mounted at `/var/lib/autvid/processing` |
| Catalog bookmark | `catalog_state` volume mounted at `/catalog`; services use `/catalog/catalog.json` |
| Permissions | One-shot init containers chown processing/catalog/devnet paths for non-root service users |

## Future Mode: Native Packaged Host

A native host should act as a launcher and router around the same service roles.
It can run the Rust admin service, Rust streaming service, `antd`, Postgres,
and frontend assets as child processes, bundled sidecars, or user-supplied
external services. The host should not require changes to the Compose runtime.

Native host responsibilities:

1. Provide an `antd` REST endpoint compatible with the current `/health`,
   `/v1/data/public`, `/v1/data/cost`, and wallet endpoints used by
   the admin implementation and `rust_stream`.
2. Provide a PostgreSQL database reachable by the admin implementation. The
   admin service owns schema creation on startup.
3. Create durable, writable processing storage and set `UPLOAD_TEMP_DIR`,
   `TMPDIR`, `TMP`, and `TEMP` to that location.
4. Create durable catalog state storage and point both backend services at the
   same `CATALOG_STATE_PATH`. The stream service may read it read-only.
5. Expose the admin API and stream API to the UI using the same path contract as
   Compose (`/api` and `/stream`) or supply equivalent frontend runtime
   configuration through `window.__AUTONOMI_VIDEO_CONFIG__`.
6. Set explicit `CORS_ALLOWED_ORIGINS` if the UI, admin API, and stream API are
   served from different origins.
7. Preserve admin auth, upload limits, payment settings, and stream cache env
   names so existing deployment docs and tests remain meaningful.

Recommended native defaults:

| Contract item | Recommended native value |
|---|---|
| Public app origin | `http://127.0.0.1:<host-selected-port>` |
| Public admin base | Same origin, `/api` |
| Public stream base | Same origin, `/stream` |
| Admin service URL | `http://127.0.0.1:8000` for `rust_admin` |
| Stream service URL | `http://127.0.0.1:8081` |
| `antd` REST URL | `http://127.0.0.1:8082` |
| Processing path | Per-user app data directory, `processing/` |
| Catalog state path | Per-user app data directory, `catalog/catalog.json` |

Frontend runtime configuration example:

```js
window.__AUTONOMI_VIDEO_CONFIG__ = {
  apiBaseUrl: "http://127.0.0.1:3000/api",
  streamBaseUrl: "http://127.0.0.1:3000/stream",
};
```

## Rust Admin Status

`rust_admin` is the default admin implementation for the containerized and
future native runtimes. Its surface implements:

| Area | Status |
|---|---|
| Health and Autonomi readiness | Implemented |
| Admin login and `/auth/me` bearer validation | Implemented |
| `/videos/upload/quote` cost estimation | Implemented |
| Public catalog/video reads from Autonomi | Implemented |
| Admin list/detail/status reads from Postgres | Implemented |
| Visibility metadata update and public catalog republish | Implemented |
| Multipart upload, FFmpeg transcode, final quote, approval upload | Implemented |
| Publication/catalog mutation and delete | Implemented |

## Compatibility Rules

- Keep Compose behavior unchanged unless a change is explicitly about the
  containerized runtime.
- Treat the admin implementation and `rust_stream` as separate service roles.
  Native packaging may launch them together, but their API and environment
  boundaries should remain clear.
- Keep `/api` and `/stream` stable as the default browser-facing paths.
- Keep `ANTD_URL`, database env vars, `UPLOAD_TEMP_DIR`, `CATALOG_STATE_PATH`,
  `CATALOG_ADDRESS`, and stream cache env vars stable. Add new env vars instead
  of repurposing existing ones.
- Make local files durable if they affect recovery. Postgres is the processing
  status cache; `CATALOG_STATE_PATH` is the latest catalog bookmark; Autonomi is
  the durable playback source of truth.
- Do not make the frontend depend on Docker-only hostnames such as
  `rust_admin`, `rust_stream`, `antd`, or `db`.

## Health Checks

Container and native hosts should verify the same service checks:

```bash
curl http://127.0.0.1:8082/health
curl http://127.0.0.1:8000/health
curl http://127.0.0.1:8081/health
```

Through the Compose/Nginx public surface, those map to:

```bash
curl http://localhost/api/health
curl http://localhost/stream/health
```
