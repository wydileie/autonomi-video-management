# Production Deployment Guide

This guide covers deploying the Autonomi Video Management stack to a Linux server with a public domain name.

---

## Prerequisites

- A Linux server (Ubuntu 22.04+ recommended) with:
  - Docker Engine + Docker Compose v2
  - Ports 80 and 443 open in the firewall
- A domain name pointed at the server's IP (`va.worldwidenodes.cyou` by default)
- An EVM wallet on Arbitrum One funded with:
  - ETH for gas
  - ANT tokens for Autonomi storage payments

---

## 1. Clone and configure

```bash
git clone <repo-url> autonomi-video-management
cd autonomi-video-management
cp .env.example .env
```

Edit `.env`:

```dotenv
# Postgres
POSTGRES_USER=postgres
POSTGRES_PASSWORD=<strong-random-password>

# App DB
ADMIN_DB=admindb
ADMIN_USER=admin
ADMIN_PASS=<strong-random-password>

# Domain
DOMAIN=va.worldwidenodes.cyou

# Autonomi — required for storage writes
AUTONOMI_WALLET_KEY=0x<your-arbitrum-one-private-key>
ANTD_NETWORK=default
ANTD_PAYMENT_MODE=auto
ANTD_APPROVE_ON_STARTUP=true
CATALOG_ADDRESS=

# Leave AUTONOMI_PEERS empty to use built-in mainnet bootstrap peers
AUTONOMI_PEERS=
```

> **Security:** Never commit `.env` to version control. Add it to `.gitignore`.

---

## 2. Obtain TLS certificates (first time only)

Before starting Nginx with HTTPS, obtain certificates with Certbot in standalone mode:

```bash
# Temporarily expose port 80 and run certbot
docker compose run --rm --entrypoint "" certbot certbot certonly \
  --standalone \
  --agree-tos --no-eff-email \
  -m your@email.com \
  -d va.worldwidenodes.cyou
```

This writes certificates into the `certbot-etc` Docker volume mounted at `/etc/letsencrypt`.

---

## 3. Build and start

```bash
docker compose up -d --build
```

The first `docker compose up --build` will take a long time because the `antd_service` container compiles the Rust `antd` binary from source. Subsequent builds use Docker layer caching.

Check that all services are up:

```bash
docker compose ps
docker compose logs antd         # should show "REST API listening on 0.0.0.0:8082"
docker compose logs python_admin # should show "Uvicorn running on 0.0.0.0:8000"
docker compose logs rust_stream  # should show "Listening on 0.0.0.0:8081"
```

---

## 4. Verify the stack

```bash
# Autonomi daemon health
curl https://va.worldwidenodes.cyou/stream/health   # via Nginx → rust_stream
curl http://localhost:8082/health                   # direct on server

# Admin API health
curl http://localhost:8000/health
curl http://localhost:8000/catalog

# Frontend
open https://va.worldwidenodes.cyou
```

---

## 5. Certificate renewal

Certbot renewal is managed via a cron job on the host (recommended) or via the `certbot` container:

```bash
# Add to crontab (runs twice daily, standard Let's Encrypt practice)
0 0,12 * * * docker compose -f /path/to/docker-compose.yml run --rm certbot renew --quiet && docker compose exec nginx nginx -s reload
```

---

## 6. Scaling considerations

### Large video files

- The Nginx `client_max_body_size` is set to `4096m` (4 GB). Adjust in `nginx/conf.d/default.conf` if needed.
- The Python admin service streams the upload to disk before processing; ensure the `video_tmp` volume has sufficient space.
- FFmpeg transcoding is CPU-intensive. For concurrent uploads, consider running multiple `python_admin` replicas behind a load balancer.

### Autonomi storage costs

Storage on Autonomi is pay-once and permanent. Approximate cost per upload (rough estimate, varies with network conditions):

| Resolution | 10-min video | Estimated segments | Approx. cost |
|---|---|---|---|
| 360p | ~180 MB | ~60 | Low |
| 720p | ~450 MB | ~60 | Medium |
| 1080p | ~900 MB | ~60 | Higher |

Check the current cost before a large upload:

```bash
curl -X POST http://localhost:8000/videos/upload \
  -F "file=@test.mp4" \
  -F "title=Cost Test" \
  -F "resolutions=720p"
# Monitor logs: docker compose logs -f python_admin
```

### Catalog persistence

- Ready video manifests and the video catalog are stored on Autonomi.
- The `catalog_state` Docker volume stores only the latest catalog address bookmark.
- If you move hosts, set `CATALOG_ADDRESS` to the latest catalog address to restore the video list from the network.
- Once `antd` exposes mutable Autonomi pointers/scratchpads, this bookmark can be replaced by a true network mutable object.

### Database backups

```bash
docker compose exec db pg_dump -U "$POSTGRES_USER" admindb > backup_$(date +%Y%m%d).sql
```

---

## 7. Updating

```bash
git pull
docker compose up -d --build
```

The `antd_service` Dockerfile is built with `--depth 1` clones so it always picks up the latest `ant-sdk` release on rebuild.

---

## 8. Stopping / teardown

```bash
# Stop (preserves volumes)
docker compose down

# Stop and remove all local data (destructive — Postgres and catalog bookmark volumes will be lost)
docker compose down -v
```

> **Note:** Data stored on the Autonomi network is permanent and cannot be deleted. `down -v` removes only local processing state and the local latest-catalog-address bookmark.

---

## Architecture diagram

```
Internet
   │
   ▼
Nginx (80/443)
   ├── /              → react_frontend :80
   ├── /api/*         → python_admin   :8000
   └── /stream/*      → rust_stream    :8081
                              │
                         appnet (Docker bridge)
                              │
              ┌───────────────┼───────────────┐
              │               │               │
        python_admin    rust_stream          db
         (FastAPI)        (Axum)         (Postgres)
              │               │
              └───────┬───────┘
                      │
                   antd :8082
                      │
               Autonomi Network
                (Arbitrum One
                 + P2P nodes)
```
