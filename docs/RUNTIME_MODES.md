# Runtime Modes

This repository now supports two launch paths over the same service roles:

- Docker Compose for local and production deployment.
- `autvid_launcher`, a Linux/macOS local web launcher that starts native child
  processes and opens the browser UI.

Both paths use the same Rust admin service, Rust streaming service, `antd`
gateway, SQLite database, catalog state file, `/api`, and `/stream` contracts.

## Containerized Compose

| File | Role |
|---|---|
| `docker-compose.yml` | Base app services, internal network, app-data mount, Nginx routing, health checks |
| `docker-compose.local.yml` | Self-contained local Autonomi devnet and `antd` for testing |
| `docker-compose.prod.yml` | Production/default-network `antd` daemon configuration |
| `docker-compose.debug-ports.yml` | Optional direct host ports for admin and stream debugging |

Important runtime glue:

| Concern | Compose value |
|---|---|
| Admin API bind | `rust_admin` listens on `0.0.0.0:8000` |
| Stream API bind | `rust_stream` listens on `0.0.0.0:8081` |
| Autonomi gateway | `ANTD_URL=http://antd:8082` |
| App data | `${AUTVID_DATA_HOST_PATH:-./.autvid/app_data}` mounted at `/var/lib/autvid` |
| SQLite | `/var/lib/autvid/autvid.sqlite3` |
| Catalog state | `/var/lib/autvid/catalog/catalog.json` |
| Processing files | `/var/lib/autvid/processing` |

## Standalone Launcher

Run after building the workspace and frontend:

```bash
cargo run -p autvid_launcher -- --mode configured
cargo run -p autvid_launcher -- --mode local-devnet
```

Launcher responsibilities:

1. Resolve a per-user app-data directory, or use `AUTVID_DATA_DIR`.
2. Create `processing`, `catalog`, and SQLite database locations.
3. Start `antd` or the local devnet script, `rust_admin`, and `rust_stream`.
4. Serve the built frontend and proxy `/api` and `/stream`.
5. Serve runtime frontend config.
6. Poll health checks, open the browser, and stop child processes on shutdown.

Useful overrides:

| Variable | Purpose |
|---|---|
| `AUTVID_DATA_DIR` | Native app-data directory. |
| `AUTVID_FRONTEND_DIR` | Built frontend directory containing `index.html`. |
| `AUTVID_ANTD_BIN` / `AUTVID_ADMIN_BIN` / `AUTVID_STREAM_BIN` | Child binary paths. |
| `AUTVID_DEVNET_CMD` | Local devnet launcher script. |
| `AUTVID_LAUNCHER_PORT` | Local browser port, default `8080`. |
| `RUST_ADMIN_PORT` / `RUST_STREAM_PORT` / `ANTD_REST_PORT` | Child service ports. |
| `ADMIN_SHUTDOWN_GRACE_SECONDS` | Reused as the launcher's child-process shutdown grace period. |

## Health Checks

```bash
curl http://127.0.0.1:8082/livez
curl http://127.0.0.1:8000/livez
curl http://127.0.0.1:8081/livez
curl http://127.0.0.1:8000/health
curl http://127.0.0.1:8081/health
```

Through Compose/Nginx or the standalone launcher:

```bash
curl http://localhost/api/health
curl http://localhost/stream/health
```
