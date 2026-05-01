# Rust Admin Service

Rust implementation of the AutVid admin/control-plane API.

This service is the default containerized admin service. It owns upload,
transcode, approval, Autonomi write, catalog, and admin metadata workflows:

- health check with Autonomi status
- admin login and bearer-token validation
- catalog/public video reads from Autonomi
- admin video reads and status reads from Postgres
- upload quote estimation using the existing Autonomi cost endpoint
- multipart upload, optional original source storage, FFmpeg transcoding, final
  quote approval, Autonomi upload, publication/catalog mutation, auto-publish,
  and delete workflows

Run locally:

```bash
cd rust_admin
cargo test
cargo run
```

Run in the local stack:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build rust_admin
```
