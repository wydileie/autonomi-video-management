# Deployment Guide

This project is designed to deploy as containers. That is the easiest path
across Linux, macOS, and Windows because the stack depends on Rust, FFmpeg,
Postgres, Nginx, and Autonomi tooling.

## Local Testnet

Use this mode for repeatable end-to-end testing without spending real storage
tokens. It starts `ant-devnet`, Anvil, and `antd` inside the Compose stack.

```bash
cp .env.local.example .env.local

docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build
```

Set `VIDEO_PROCESSING_HOST_PATH` to a host path with enough free disk space for
original uploads and transcoded segments. This directory is bind-mounted into
`rust_admin` and is required for interrupted transcode/upload jobs to resume
after a container restart.

```dotenv
VIDEO_PROCESSING_HOST_PATH=/mnt/large-disk/autonomi-video-processing
```

The Compose stack runs a one-shot `init_permissions` container before the app
starts. It creates this directory when Docker has not already created it, then
chowns the bind mount and catalog volume for the non-root admin service user
(UID/GID `1000`).

Verify:

```bash
curl http://localhost:8082/health
curl http://localhost/api/health
curl http://localhost/stream/health
open http://localhost
```

If another local process already owns those ports, change `APP_HTTP_PORT`,
`ANTD_REST_PORT`, or `ANTD_GRPC_PORT` in your env file. `ADMIN_HTTP_PORT` and
`STREAM_HTTP_PORT` are only published when you use a local/debug compose
override. Container-to-container traffic still uses the standard internal ports.

To publish direct admin and stream debug ports, add
`docker-compose.debug-ports.yml` to the same command:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.debug-ports.yml \
  up --build
```

## Public Local-Devnet Demo

Use this mode when you want an internet-accessible demo without spending real
Autonomi storage tokens. It runs the same self-contained local devnet as the
local test mode, but keeps the devnet `antd` REST/gRPC ports internal to the
Compose network and exposes only the app's Nginx reverse proxy.

```bash
cp .env.local-public.example .env.local-public
# Edit .env.local-public before starting.

docker compose --env-file .env.local-public \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.local-public.yml \
  up --build -d
```

Required public-demo values:

```dotenv
APP_ENV=production
ADMIN_USERNAME=autvid-demo-admin
ADMIN_PASSWORD=<long random admin password>
ADMIN_AUTH_SECRET=<long random token signing secret, at least 32 chars>
DOMAIN=demo.example.com
APP_HTTP_PORT=80
CORS_ALLOWED_ORIGINS=https://demo.example.com,http://demo.example.com
VIDEO_PROCESSING_HOST_PATH=/srv/autonomi-video-management/processing
```

Do not add `docker-compose.debug-ports.yml` for a public demo unless you
intentionally want direct admin/stream debug ports published. Keep cloud or host
firewall rules limited to SSH plus HTTP/HTTPS. Docker-published ports can bypass
some host firewall frontends, so prefer removing unwanted port publishes with
the public-demo overlay rather than relying only on `ufw`.

This mode is for demonstration only. The local devnet is not the public
Autonomi network, and data in the local devnet volume is not permanent network
storage. `ANT_DEVNET_RESET_ON_START=true` starts from a clean devnet each time;
set it to `false` only if you intentionally want to try preserving local-devnet
state across restarts for a short-lived demo.

## Production/Default Network

Use this mode when you want `antd` to connect to the configured Autonomi
network and pay with a real wallet.

For hardened deployments, pin image references by digest in a local production
overlay after publishing tested images. The current Dockerfiles still use
runtime images with shells and package managers where needed; a follow-up
hardening pass should evaluate distroless or similarly minimized runtime
images for services that no longer need shell tooling at runtime.

```bash
cp .env.production.example .env.production
# Edit .env.production before starting.

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  up --build -d
```

Required production values:

```dotenv
APP_ENV=production
ADMIN_USERNAME=autvid-admin
ADMIN_PASSWORD=<long random admin password>
ADMIN_AUTH_SECRET=<long random token signing secret, at least 32 chars>
ADMIN_REFRESH_TOKEN_TTL_HOURS=720
ADMIN_AUTH_COOKIE_SAME_SITE=Lax
PROD_ANTD_NETWORK=default
PROD_EVM_NETWORK=arbitrum-one
ANTD_PAYMENT_MODE=auto
ANTD_APPROVE_ON_STARTUP=true
VIDEO_PROCESSING_HOST_PATH=/srv/autonomi-video-management/processing
```

### Production Secrets

The production overlay mounts Postgres, admin-login, auth-signing, internal
service-token, backup-role, and wallet values as Docker Compose file-backed
secrets under `/run/secrets`. The service containers receive only `_FILE`
environment variables for those values.

Create the files referenced by `.env.production` before starting production:

```bash
install -d -m 0700 secrets
printf '%s\n' '<postgres root password>' > secrets/postgres_root_password
printf '%s\n' '<admin database password>' > secrets/admin_db_password
printf '%s\n' '<backup read-only database password>' > secrets/backup_db_password
printf '%s\n' '<admin login password>' > secrets/admin_login_password
printf '%s\n' '<long auth signing secret>' > secrets/admin_auth_secret
printf '%s\n' '<internal bearer token>' > secrets/antd_internal_token
printf '%s\n' '0x<your_wallet_private_key>' > secrets/autonomi_wallet_key
chmod 0600 secrets/*
```

The `*_SECRET_FILE` variables in `.env.production` can point at a host secret
manager mount instead of `./secrets/...`. Keep direct secret values blank or as
throwaway placeholders in production; the overlay resets them before container
startup.

Start production:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  up --build -d
```

For public deployments, put TLS, auth, and domain routing in front of the stack
with a host reverse proxy, cloud load balancer, Tailscale/Funnel, Caddy,
Traefik, Nginx Proxy Manager, or similar. The app stack itself serves local HTTP
on port `80`.

### Live Network Checks

The production Compose file starts an Autonomi 2.0 compatibility gateway on
the `antd` service name. It preserves the REST endpoints used by this app, but
connects directly to the Autonomi P2P network with `ant-core`/`ant-node`.

The gateway can report healthy while still failing write quotes if the P2P
client has not connected to enough storage peers yet:

```text
POST /v1/data/cost failed: 502 {"error":"Network error: Found 0 peers, need 7","code":"NETWORK_ERROR"}
```

`PROD_AUTONOMI_PEERS` is optional. Leave it blank to use the built-in
Autonomi 2.0 bootstrap peers, or provide a comma/newline-separated list of
plain `host:port` peers or full multiaddrs. The gateway also accepts
`ANT_PEERS` for compatibility with Autonomi bootstrap tooling.

Useful checks:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  logs --tail=200 antd

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec antd curl -fsS http://127.0.0.1:8082/health

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec antd curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -d '{"data":"aGV5"}' \
    http://127.0.0.1:8082/v1/data/cost
```

If `/health` reports `peer_count: 0` or the cost probe returns
`Found 0 peers, need 7`, the app is on the Autonomi 2.0 path but the host still
cannot reach enough live peers. Try a known-good peer list in
`PROD_AUTONOMI_PEERS`, or run the stack from a network that allows outbound
UDP/QUIC to the current Autonomi 2.0 bootstrap peers.

## Operations

Check services:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  ps
```

Follow logs:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  logs -f rust_admin rust_stream antd
```

Every request flowing through Nginx and the Rust services has an
`X-Request-ID`. Provide your own request ID when debugging, or let the proxy and
services generate one. Request logs include request ID, service, method, URI,
status, and latency; admin job logs add `video_id`, `job_id`, `resolution`,
`segment_index`, and `catalog_publish_epoch` where applicable.

The public Nginx gateway rate-limits login, refresh, upload quote, and stream
requests by client address. It also rewrites forwarded headers from sanitized
proxy-side values before traffic reaches the Rust services.

Admin auth is cookie-only for browser and smoke-test flows. Login and refresh
set HttpOnly SameSite access/refresh cookies plus a non-HttpOnly `autvid_csrf`
cookie; unsafe authenticated requests must echo that value in
`X-CSRF-Token`. Keep `ADMIN_AUTH_COOKIE_SECURE=true` for HTTPS deployments; use
`false` only for local HTTP testing. `ADMIN_AUTH_COOKIE_SAME_SITE=None` is
rejected unless secure cookies are enabled.

The Rust admin service does not infer cookie security from client-supplied
forwarded headers; `ADMIN_AUTH_COOKIE_SECURE` controls the `Secure` attribute.
Only expose the app through Nginx or a trusted upstream that overwrites
`X-Forwarded-*` before requests enter the Compose network.

Metrics endpoints emit Prometheus text on the internal Compose network. The
public Nginx proxy intentionally blocks `/api/metrics` and `/stream/metrics`.

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec rust_admin curl -fsS http://127.0.0.1:8000/metrics
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec rust_stream curl -fsS http://127.0.0.1:8081/metrics
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec antd curl -fsS http://127.0.0.1:8082/metrics
```

These metrics are intentionally internal. Do not expose the `antd` service port
publicly, and protect any external scrape path at your edge proxy.

To enable host and backup textfile metrics, include the monitoring override and
the backup override with a shared `BACKUP_TEXTFILE_HOST_PATH`:

```bash
BACKUP_TEXTFILE_HOST_PATH=/srv/autonomi-video-management/textfile \
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  -f docker-compose.backup.yml \
  up --build -d
```

The backup sidecar writes `autvid_backup.prom` after each attempt, node-exporter
exports it, and Prometheus evaluates backup freshness and failure alerts.

Create a timestamped production backup:

```bash
make backup-production
```

The backup helper writes a directory such as
`backups/autvid-20260505T120000Z/` containing a custom-format
`postgres.dump`, `manifest.env`, and `catalog.json` when the catalog bookmark
exists.

Restore Postgres and the catalog bookmark into a fresh stack:

```bash
make restore-production ARGS='--backup-dir backups/autvid-YYYYMMDDTHHMMSSZ --yes'
```

The restore helper refuses to run without `--yes` because it uses
`pg_restore --clean --if-exists` against `ADMIN_DB`. To restore only a database
dump or to point at a different Compose env/file set, call the script directly:

```bash
COMPOSE_ENV_FILE=.env.production \
COMPOSE_FILES='docker-compose.yml docker-compose.prod.yml' \
scripts/restore-production.sh --db-file ./backups/autvid-.../postgres.dump --yes
```

Stop without deleting data:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  down
```

Destructive local reset:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  down -v
```

Data already written to Autonomi is permanent. `down -v` only removes local
volumes such as Postgres state, the latest catalog bookmark, and local devnet
state. The processing bind mount at `VIDEO_PROCESSING_HOST_PATH` is a normal
host directory, so Compose will not delete it.

## Moving Hosts

Ready video manifests and the catalog are stored on Autonomi. The
`catalog_state` Docker volume stores only the latest catalog address bookmark.
If you move to another host, set `CATALOG_ADDRESS` to the last catalog address
to bootstrap the video list from the network.
