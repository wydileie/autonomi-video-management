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
# Choose one wallet source:
PROD_AUTONOMI_WALLET_KEY=0x<your_wallet_private_key>
# or:
# PROD_AUTONOMI_WALLET_KEY=
# PROD_AUTONOMI_WALLET_KEY_FILE=/run/secrets/autonomi_wallet_key
PROD_ANTD_NETWORK=default
PROD_EVM_NETWORK=arbitrum-one
ANTD_PAYMENT_MODE=auto
ANTD_APPROVE_ON_STARTUP=true
VIDEO_PROCESSING_HOST_PATH=/srv/autonomi-video-management/processing
```

### Wallet Secret Files

For local development and quick private testing, `PROD_AUTONOMI_WALLET_KEY`
can stay in the env file. For production, prefer mounting the wallet key as a
Docker Secret or another root-readable file instead of storing the key directly
in `.env.production`.

`PROD_AUTONOMI_WALLET_KEY_FILE` is an in-container file path. The production
Compose file passes it through to the `antd` gateway as
`AUTONOMI_WALLET_KEY_FILE`; when it is set, the gateway reads that file and
uses it instead of `PROD_AUTONOMI_WALLET_KEY`.

Keep the base `docker-compose.prod.yml` free of a required secret file so
normal `docker compose config` checks work before a real key exists. Add a
local, uncommitted overlay such as `docker-compose.prod.secrets.yml` only on
hosts that have the wallet key file:

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

Then set these values in `.env.production`:

```dotenv
PROD_AUTONOMI_WALLET_KEY=
PROD_AUTONOMI_WALLET_KEY_FILE=/run/secrets/autonomi_wallet_key
```

Start production with the extra overlay:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.prod.secrets.yml \
  up --build -d
```

Without the secrets overlay, keep `PROD_AUTONOMI_WALLET_KEY_FILE` blank and
set `PROD_AUTONOMI_WALLET_KEY` directly. The local/dev compose paths remain
env-based and do not require Docker Secrets.

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

Admin auth is still bearer-token compatible for scripts and older clients. The
browser login route also sets an HttpOnly SameSite cookie. Keep
`ADMIN_AUTH_COOKIE_SECURE=true` for HTTPS deployments; use `false` only for
local HTTP testing.

Metrics endpoints emit Prometheus text:

```bash
curl http://localhost/api/metrics
curl http://localhost/stream/metrics
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec antd curl -fsS http://127.0.0.1:8082/metrics
```

These metrics are intentionally internal. Do not expose the `antd` service port
publicly, and protect any external scrape path at your edge proxy.

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
