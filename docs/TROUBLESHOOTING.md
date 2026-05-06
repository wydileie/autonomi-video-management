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

## antd Peer Count Low

The gateway can be healthy before enough Autonomi peers are reachable for
quotes or uploads. Confirm peer state and try a small cost probe:

```bash
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
