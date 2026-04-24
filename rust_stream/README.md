# Rust Streaming Service

Axum-based HTTP service that generates HLS playlists on the fly and proxies MPEG-TS segments from the Autonomi network to the video player.

## Responsibilities

1. Generate HLS `.m3u8` manifests dynamically by reading the network-hosted catalog and video manifest from Autonomi.
2. Serve individual `.ts` segment requests by fetching the content from Autonomi via the `antd` daemon and streaming the bytes back to the client.

The player never contacts the Autonomi network directly — this service acts as a transparent proxy.

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check for the `antd` daemon and catalog address availability |
| `GET` | `/stream/{video_id}/{resolution}/playlist.m3u8` | HLS manifest |
| `GET` | `/stream/{video_id}/{resolution}/{index}.ts` | TS segment (Autonomi proxy) |

### Example manifest response

```m3u8
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:10.000,
/stream/550e8400-e29b-41d4-a716-446655440000/720p/0.ts
#EXTINF:10.000,
/stream/550e8400-e29b-41d4-a716-446655440000/720p/1.ts
...
#EXT-X-ENDLIST
```

The manifest only exists for videos present in the latest Autonomi catalog with `status = 'ready'`.

## Configuration (environment variables)

| Variable | Description |
|---|---|
| `ANTD_URL` | antd daemon REST URL (default: `http://localhost:8082`) |
| `CATALOG_STATE_PATH` | Local bookmark file containing the latest Autonomi catalog address |
| `CATALOG_ADDRESS` | Optional bootstrap catalog address if no bookmark file exists |
| `RUST_LOG` | Log level (default: `info`) |

## Dependencies

- `axum` — HTTP server
- `tokio` — async runtime
- `antd-client` — Autonomi SDK; calls `data_get_public` on the `antd` daemon
- `tower-http` — CORS middleware
- `serde`, `serde_json`, `anyhow`, `tracing`

## Local development

```bash
cd rust_stream
cargo run

# Override defaults
ANTD_URL=http://localhost:8082 \
CATALOG_STATE_PATH=/tmp/video_catalog/catalog.json \
cargo run
```

## Segment fetch flow

```
GET /stream/{video_id}/{resolution}/{index}.ts
  → Read latest catalog address from CATALOG_STATE_PATH or CATALOG_ADDRESS
  → Fetch catalog JSON from Autonomi
  → Fetch the video's manifest JSON from Autonomi
  → Resolve resolution + segment_index to an Autonomi segment address
  → antd_client.data_get_public(&address).await
  → Stream bytes with Content-Type: video/mp2t
```

Both the manifest and segment handlers return `404` if the video is not found or is not yet `ready`.
