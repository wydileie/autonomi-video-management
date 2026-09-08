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
ANTD_QUOTE_CONCURRENCY=2
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

## SQLite Writes

SQLite runs in WAL mode, so readers can continue during writes, but there is
still only one writer at a time. Admin writes, visibility changes, publication
changes, durable job leasing, and retry bookkeeping can queue behind each other
during bursts. `ADMIN_DB_CONNECT_TIMEOUT_SECONDS` also sets SQLite's busy
timeout; the default gives writers up to 30 seconds to wait before surfacing a
database busy error.

If busy errors appear, reduce write pressure before increasing worker counts:
keep `ADMIN_JOB_WORKERS` conservative, avoid bulk visibility/publication flips
while uploads are active, and check whether long Autonomi or FFmpeg work is
happening outside database transactions.

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
| `nginx` | 0.5 CPU / 256 MB |
| `apps/web` | 0.5 CPU / 256 MB |

## September 8, 2026 modernization measurements

Measured on a local ARM64 Docker devnet while other builds were running. These
are observations, not a controlled before/after speedup or public-network SLA.

| Original | Prepare quote | Upload + verify | First raw download | Repeat raw download | Transactions | Retry extra transactions |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 20 MiB | 0.82s | 3.07s | 0.14s | 0.12s | 2 | 0 |
| 1 GiB + 4 KiB | 73.11s | 93.60s | 3.69s | 3.37s | 3 | 0 |

The large upload stored and verified all 261 chunks, spanning two Merkle payment
batches plus bounded allowance. Both streamed downloads matched source SHA-256.
Gateway Linux VmHWM was 2,020,236 KiB (about 1.93 GiB) during the large test; the
combined gateway/devnet container peaked around 3.74 GiB. Upstream preparation
and storage use bounded waves, but these measurements show substantial memory
headroom is still necessary. Concurrent large operations need further load tests.

After restarting only the streaming service, playlist latency was
28.96 ms, first-segment latency 5.83 ms,
and the median of ten warm segment requests was 0.51 ms.
Gateway/network caches remained warm. Catalog construction uses two batched
queries in one read snapshot, independent of video count; this is a code-level
query count, not a production database trace measurement.

Reproduce with the local test stack (funded test chain only):

```bash
ANTD_INTERNAL_TOKEN=dev-internal-token python3 scripts/benchmark-gateway.py --url http://127.0.0.1:8082 --bytes 1073745920
python3 scripts/benchmark-playback.py --url http://127.0.0.1:8080
```

Use the actual published ports for your stack. Capture gateway VmHWM and
container `memory.peak` alongside the JSON output. Keep workload, peer count,
cache state, hardware, and other build activity constant for comparisons.
