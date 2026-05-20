# Architecture

Autonomi Video Management keeps the service boundaries deliberately boring:
the browser talks to Nginx, Nginx routes admin API requests to `rust_admin`,
playback requests to `rust_stream`, and both Rust services talk to the local
`antd` gateway instead of joining the Autonomi peer network directly.

## Service Boundaries

| Service | Responsibility |
|---|---|
| `react_frontend` | Upload, approval, publication, catalog, and playback UI |
| `nginx` | Browser-facing reverse proxy, security headers, request IDs, and rate limits |
| `rust_admin` | Auth, upload intake, FFmpeg transcoding, durable jobs, quotes, uploads, manifests, and catalog publication |
| `rust_stream` | Public HLS playlists and segment proxying from Autonomi |
| `antd` | REST gateway for Autonomi connectivity, payment, storage, and retrieval |
| SQLite | Local admin metadata, auth sessions, durable jobs, and recovery bookkeeping |
| Autonomi | Durable playback objects: segments, manifests, and published catalogs |

The frontend never receives private segment addresses from public endpoints.
Public playback goes through `rust_stream`, which resolves the current catalog
and manifest before streaming segment bytes.

## Upload And Approval Flow

1. `rust_admin` streams the source upload to app data and records the video row,
   requested renditions, source path, and durable job input in SQLite.
2. A durable processing job probes the source, transcodes HLS renditions with
   FFmpeg, records variant and segment rows, and builds a final quote from the
   real bytes on disk.
3. The job pauses in `awaiting_approval` until an admin approves the final
   quote before storage payment is committed.
4. A durable upload job streams every segment, and optionally the original
   source file, through the `antd` file endpoint.
5. Upload verification reads data back before the video is marked `ready`.
6. `rust_admin` stores a video manifest on Autonomi, then catalog publication
   writes portable catalog documents for public and all-ready-video views.

The `rust_admin` durable queue currently stores three job kinds:
`process_video`, `upload_video`, and `publish_catalog`. On startup, the admin
service leases eligible pending, processing, uploading, and catalog-publish
work from SQLite.
Interrupted transcodes restart from the saved source file. Interrupted uploads
reuse persisted segment rows and skip segments that already have Autonomi
addresses.

## Catalog State

Catalog state is intentionally dual-written:

- SQLite stores mutable admin metadata and durable job state.
- Autonomi stores durable manifest and catalog documents for playback.
- The JSON catalog state file stores the latest catalog addresses and decoded
  snapshots as a local bootstrap and recovery aid.

The JSON file is not legacy state. It lets `rust_stream` and restarted stacks
find the latest catalog address without requiring a fresh admin query, and it
gives operators a small inspectable recovery artifact alongside the SQLite
database. Invalid JSON is quarantined to a `.broken` file rather than silently
overwritten.

## Reliability Posture

The architecture keeps these safety rails on purpose:

- SQLite-backed durable jobs with leases, attempts, retry backoff, and startup
  recovery.
- Upload verification before manifests and catalogs point at new data.
- Catalog publication jobs with retries and local state snapshots.
- Health checks, request IDs, service metrics, and smoke tests.
- Separate Compose overlays for local devnet, production, backup, monitoring,
  logging, and debug ports.

These pieces add a little implementation surface area, but they keep expensive
storage writes, long FFmpeg work, and public playback metadata recoverable.
