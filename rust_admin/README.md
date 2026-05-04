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

Source layout:

- `main.rs` wires configuration, database, Autonomi readiness, workers, and the Axum router.
- `routes.rs` owns HTTP handlers and admin authorization boundaries.
- `upload.rs`, `media.rs`, `quote.rs`, and `pipeline.rs` handle upload intake, probing/transcoding, quote estimation, and process/upload jobs.
- `catalog.rs` and `storage.rs` own manifests, catalog state, publication, and verified Autonomi JSON writes.
- `db.rs`, `jobs.rs`, `models.rs`, `state.rs`, `config.rs`, and `antd_client.rs` hold shared service plumbing.

Run in the local stack:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  up --build rust_admin
```
