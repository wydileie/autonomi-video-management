# Deployment Guide

This project is designed to deploy as containers. That is the easiest path
across Linux, macOS, and Windows because the stack depends on Python, Rust,
FFmpeg, Postgres, Nginx, and Autonomi tooling.

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

If you run this from inside the devcontainer while Docker is provided by the
host, set `HOST_WORKSPACE_DIR` in `.env.local` to the host path of the repo.
For example:

```dotenv
HOST_WORKSPACE_DIR=/Users/you/Repos/autonomi-video-management
```

Verify:

```bash
curl http://localhost:8082/health
curl http://localhost:8000/health
curl http://localhost:8081/health
open http://localhost
```

If another local process already owns those ports, change `APP_HTTP_PORT`,
`ADMIN_HTTP_PORT`, `STREAM_HTTP_PORT`, `ANTD_REST_PORT`, or `ANTD_GRPC_PORT` in
your env file. Container-to-container traffic still uses the standard internal
ports.

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
PROD_AUTONOMI_WALLET_KEY=0x<your_wallet_private_key>
PROD_ANTD_NETWORK=default
ANTD_PAYMENT_MODE=auto
ANTD_APPROVE_ON_STARTUP=true
```

For public deployments, put TLS, auth, and domain routing in front of the stack
with a host reverse proxy, cloud load balancer, Tailscale/Funnel, Caddy,
Traefik, Nginx Proxy Manager, or similar. The app stack itself serves local HTTP
on port `80`.

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
  logs -f python_admin rust_stream antd
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
state.

## Moving Hosts

Ready video manifests and the catalog are stored on Autonomi. The
`catalog_state` Docker volume stores only the latest catalog address bookmark.
If you move to another host, set `CATALOG_ADDRESS` to the last catalog address
to bootstrap the video list from the network.
