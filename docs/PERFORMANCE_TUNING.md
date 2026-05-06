# Performance Tuning

Tune one dimension at a time and keep a short note of the before/after values.
The most useful signals are job duration, upload retries, stream cache hit
ratio, eviction rate, container memory, and processing disk space.

## Transcoding

Start conservatively:

```dotenv
ADMIN_JOB_WORKERS=1
FFMPEG_THREADS=2
FFMPEG_FILTER_THREADS=1
FFMPEG_MAX_PARALLEL_RENDITIONS=1
```

Increase `FFMPEG_THREADS` when CPU is available and a single rendition is slow.
Increase `FFMPEG_MAX_PARALLEL_RENDITIONS` only when memory and disk I/O have
clear headroom. Keep `UPLOAD_MIN_FREE_BYTES` large enough for original files,
temporary FFmpeg output, and final HLS segments.

## Autonomi Uploads And Quotes

These values control pressure on the `antd` gateway and Autonomi network:

```dotenv
ANTD_QUOTE_CONCURRENCY=8
ANTD_UPLOAD_CONCURRENCY=4
ANTD_UPLOAD_RETRIES=3
ANTD_UPLOAD_TIMEOUT_SECONDS=120
```

Raise upload concurrency when peer count is healthy, upload retries are low,
and `antd` latency is stable. Lower it when uploads time out, peer count drops,
or `autvid_admin_upload_retries_total` climbs.

## Frontend API Retries

The React client retries idempotent reads and upload quote requests after
transient network or 5xx failures with short backoffs of 150 ms and 350 ms.
If production sits behind a slow cold-starting load balancer, tune the client
delay constants alongside load balancer health-check and warm-up behavior.

## Stream Cache

The segment cache trades memory for fewer Autonomi reads:

```dotenv
STREAM_SEGMENT_CACHE_TTL_SECONDS=3600
STREAM_SEGMENT_CACHE_MAX_BYTES=67108864
STREAM_REQUEST_TIMEOUT_SECONDS=60
```

Increase `STREAM_SEGMENT_CACHE_MAX_BYTES` when the stream cache dashboard shows
resident bytes near the ceiling with frequent evictions:

- `autvid_stream_segment_cache_evictions_total`
- `autvid_stream_segment_cache_bytes_resident`
- `autvid_stream_segment_cache_entries`

Decrease cache size when the `rust_stream` container approaches its memory
limit or the host starts swapping. Keep the production memory limit above the
cache size because in-flight segment responses also use memory.

## Scaling Guidance

Scale `rust_admin` vertically first. More CPU and memory directly improves
transcode throughput and reduces FFmpeg OOM risk.

Scale `rust_stream` when request latency rises while cache hit ratio is healthy.
If cache hit ratio is poor, tune cache size and segment TTL before adding more
replicas.

Scale `antd` resources when peer operations, cost quotes, or uploads are slow
even though Rust services are healthy. Watch `antd` scrape latency and admin
outbound `antd` error metrics.

## Production Resource Defaults

The production Compose overlay sets resource ceilings for the main services:

| Service | Limit |
| --- | --- |
| `antd` | 2 CPU / 2 GB |
| `rust_admin` | 2 CPU / 2 GB |
| `rust_stream` | 1 CPU / 512 MB |
| `db` | 1 CPU / 1 GB |
| `nginx` | 0.5 CPU / 256 MB |
| `react_frontend` | 0.5 CPU / 256 MB |
