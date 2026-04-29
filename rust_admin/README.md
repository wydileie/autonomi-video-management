# Rust Admin Service

Experimental Rust implementation of the AutVid admin/control-plane API.

This service is a migration target for `python_admin`; it is not the default
containerized admin service yet. The initial implementation keeps the same API
and environment contracts for the low-risk paths that are useful during
side-by-side testing:

- health check with Autonomi status
- admin login and bearer-token validation
- catalog/public video reads from Autonomi
- admin video reads and status reads from Postgres
- upload quote estimation using the existing Autonomi cost endpoint
- explicit `501 Not Implemented` responses for upload, transcode, approval,
  publication, and delete workflows that are still owned by `python_admin`

Run locally:

```bash
cd rust_admin
cargo test
cargo run
```

Run as a side-by-side container:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.rust-admin.yml \
  up --build rust_admin
```

Add `docker-compose.rust-admin.yml` to the full stack command when you want
Nginx to route `/api/*` to `rust_admin` for parity testing. Leave the overlay
out to keep the stable Python-backed runtime.
