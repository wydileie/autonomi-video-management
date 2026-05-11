# Troubleshooting

Use `X-Request-ID` values from Nginx or service logs to connect browser
requests, admin API calls, stream requests, and worker logs.

## Stuck Admin Job

Check recent worker logs and database health first:

```bash
make logs-prod

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec db pg_isready -U "$POSTGRES_USER"
```

Common causes are an exhausted processing disk, `antd` write quote failures,
FFmpeg exits, or a worker lease that expired while the container was down.
Restarting `rust_admin` is safe; durable jobs are recovered from Postgres and
the processing bind mount.

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  restart rust_admin
```

## Expired Approval

Uploads waiting for approval use `FINAL_QUOTE_APPROVAL_TTL_SECONDS`. If an
approval expired, create a new quote from the UI or API rather than approving
the old one. If the source files were removed from `VIDEO_PROCESSING_HOST_PATH`,
upload the video again.

Check cleanup timing:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  logs --tail=200 rust_admin
```

## Upload Quote Unavailable

If the UI says `Autonomi request circuit is open`, `rust_admin` has already
seen repeated retryable `antd` failures and is failing fast for about 30
seconds. Check the first `antd` error in the message or logs; common causes are
`Found 0 peers, need 7`, low peer count, or a timed-out write quote.

For the local Compose devnet, confirm the gateway can see peers and return a
small write quote:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  exec antd curl -fsS http://127.0.0.1:8082/health

docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  exec antd sh -ceu '
    token="$(cat "$ANTD_INTERNAL_TOKEN_FILE" 2>/dev/null || printf %s "$ANTD_INTERNAL_TOKEN")"
    curl -fsS -X POST \
      -H "Authorization: Bearer $token" \
      -H "Content-Type: application/json" \
      -d "{\"data\":\"aGV5\"}" \
      http://antd:8082/v1/data/cost
  '
```

If local `/health` reports `peer_count: 0`, the local devnet/gateway is stale.
Restarting `antd` usually rebuilds the test network, but the default
`ANT_DEVNET_RESET_ON_START=true` deletes local devnet data.

If final quotes fail with `POST /v1/data/cost failed: 408 Request Timeout`,
the `antd` route timeout fired before the Autonomi quote finished. Keep
`ANTD_REQUEST_TIMEOUT_SECONDS` higher than `ANTD_QUOTE_TIMEOUT_SECS` and
`ANTD_STORE_TIMEOUT_SECS`, and lower `ANTD_QUOTE_CONCURRENCY` when the local
devnet is small or peer count is unstable.

## antd Peer Count Low

The gateway can be healthy before enough Autonomi peers are reachable for
quotes or uploads. Confirm peer state and try a small cost probe:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  exec antd /usr/local/bin/antd --healthcheck 127.0.0.1:8082 /livez
```

For production write-cost probes, run any temporary HTTP client on the Compose
network and call `http://antd:8082/health` and `POST /v1/data/cost`; the
production `antd` image intentionally does not include `curl`.

If peer count remains low, verify outbound UDP/QUIC from the host and set a
known-good `PROD_AUTONOMI_PEERS` list.

## Segment Cache Thrash

Symptoms are low hit ratio, rising eviction rate, and frequent misses during
seeks. Check the stream cache dashboard for:

- `autvid_stream_segment_cache_hits_total`
- `autvid_stream_segment_cache_misses_total`
- `autvid_stream_segment_cache_evictions_total`
- `autvid_stream_segment_cache_bytes_resident`
- `autvid_stream_segment_cache_entries`

Increase `STREAM_SEGMENT_CACHE_MAX_BYTES` when resident bytes regularly sit at
the configured ceiling and evictions rise during normal playback. Keep enough
container memory headroom for in-flight segment fetches.

## FFmpeg OOM Or Slow Transcodes

If the host kills `rust_admin` or FFmpeg exits during large uploads, reduce
parallelism before raising memory:

```dotenv
FFMPEG_THREADS=1
FFMPEG_FILTER_THREADS=1
FFMPEG_MAX_PARALLEL_RENDITIONS=1
ADMIN_JOB_WORKERS=1
```

If transcodes are slow and CPU and memory are available, increase one setting
at a time and watch job completion time, container memory, and disk space in
`VIDEO_PROCESSING_HOST_PATH`.

## Useful Validation Commands

```bash
make compose-config
make logs-prod
curl -I http://localhost/
curl http://localhost/api/health
curl http://localhost/stream/health
```
