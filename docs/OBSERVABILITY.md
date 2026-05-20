# Observability

The Compose stack can run optional Prometheus, Grafana, Alertmanager, Loki, and
Promtail services with `docker-compose.observability.yml`. Prometheus scrapes
the Rust services and `antd` gateway. Promtail tails Docker container logs into
Loki for browsing from Grafana.

## Metrics Endpoints

Prometheus scrapes these internal Compose targets:

| Service | Target | Metrics path | Primary labels |
|---|---|---|---|
| Admin API | `rust_admin:8000` | `/metrics` | `service="rust_admin"` |
| Stream API | `rust_stream:8081` | `/metrics` | `service="rust_stream"` |
| antd gateway | `antd:8082` | `/metrics` | `service="antd_service"` |

The shared HTTP metrics include counters and Prometheus histograms:

| Metric | Meaning |
|---|---|
| `autvid_http_requests_total` | Total HTTP requests handled by a service |
| `autvid_http_request_errors_total` | Total HTTP requests with 5xx responses |
| `autvid_http_request_latency_ms_total` | Cumulative HTTP response latency in milliseconds |
| `autvid_http_request_latency_ms_bucket` / `_sum` / `_count` | HTTP response latency histogram for percentile queries |

Admin-specific counters:

| Metric | Meaning |
|---|---|
| `autvid_admin_jobs_started_total` | Durable admin job attempts started by workers |
| `autvid_admin_jobs_succeeded_total` | Durable admin jobs completed successfully |
| `autvid_admin_jobs_failed_total` | Durable admin job attempts that returned an error |
| `autvid_admin_jobs_queued` | Current queued durable admin jobs |
| `autvid_admin_jobs_running` | Current running durable admin jobs |
| `autvid_admin_jobs_failed` | Current failed durable admin jobs |
| `autvid_admin_jobs_succeeded` | Current succeeded durable admin jobs |
| `autvid_admin_oldest_queued_job_age_seconds` | Age of the oldest queued durable admin job |
| `autvid_admin_ffmpeg_runs_total` | FFmpeg rendition runs |
| `autvid_admin_ffmpeg_duration_ms_total` | Cumulative FFmpeg runtime in milliseconds |
| `autvid_admin_antd_requests_total` | Outbound requests from `rust_admin` to `antd` |
| `autvid_admin_antd_request_errors_total` | Failed outbound `antd` requests |
| `autvid_admin_antd_request_latency_ms_total` | Cumulative outbound `antd` latency in milliseconds |
| `autvid_admin_upload_retries_total` | Autonomi upload retries scheduled by `rust_admin` |
| `autvid_admin_ffmpeg_duration_ms_bucket` / `_sum` / `_count` | FFmpeg rendition runtime histogram by resolution |
| `autvid_admin_antd_request_latency_ms_bucket` / `_sum` / `_count` | Outbound `antd` request latency histogram by endpoint |
| `autvid_admin_job_pickup_latency_ms_bucket` / `_sum` / `_count` | Durable job pickup latency histogram |

Stream-specific counters:

| Metric | Meaning |
|---|---|
| `autvid_stream_segment_cache_hits_total` | Segment cache hits |
| `autvid_stream_segment_cache_misses_total` | Segment cache misses |
| `autvid_stream_segment_fetch_coalesced_total` | Requests joined to an in-flight segment fetch |
| `autvid_stream_segment_cache_evictions_total` | Segment cache entries evicted while enforcing cache limits |
| `autvid_stream_segment_cache_bytes_resident` | Current resident bytes in the segment cache |
| `autvid_stream_segment_cache_entries` | Current number of segment cache entries |
| `autvid_stream_segment_fetch_latency_ms_bucket` / `_sum` / `_count` | Segment fetch latency histogram split by `cache_hit` and `cache_miss` |

## Running Locally

Start the local devnet stack with metrics and log collection:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.observability.yml \
  up --build
```

For metrics only, use the split monitoring overlay:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.monitoring.yml \
  up --build
```

Do not combine `docker-compose.observability.yml` with
`docker-compose.monitoring.yml` or `docker-compose.logging.yml` in the same
command; those overlays intentionally define some of the same service and
container names. `make compose-config` checks that shared image defaults stay
aligned between the split and combined overlays.

Open from the Docker host:

| Tool | URL | Default credentials |
|---|---|---|
| Grafana | `http://localhost:3000` | `admin` / `admin` |
| Prometheus | `http://localhost:9090` | none |
| Alertmanager | `http://localhost:9093` | none |
| Loki | `http://localhost:3100` | none |

The observability services bind their HTTP ports to `127.0.0.1` by
default. Override the bind address only when another authenticated edge proxy,
VPN, or private network protects these tools:

```dotenv
GRAFANA_HTTP_BIND=127.0.0.1
GRAFANA_HTTP_PORT=3001
PROMETHEUS_HTTP_BIND=127.0.0.1
PROMETHEUS_HTTP_PORT=9091
ALERTMANAGER_HTTP_BIND=127.0.0.1
ALERTMANAGER_HTTP_PORT=9094
GRAFANA_ADMIN_USER=admin
GRAFANA_ADMIN_PASSWORD=<change-me>
LOKI_HTTP_BIND=127.0.0.1
LOKI_HTTP_PORT=3101
PROMTAIL_HTTP_BIND=127.0.0.1
PROMTAIL_HTTP_PORT=9080
```

## Running in Production

Add the observability overlay to the production Compose command:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.observability.yml \
  up --build -d
```

The older split overlays remain available when you want metrics-only or
logs-only inspection. Use these instead of the combined observability overlay,
not alongside it:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  -f docker-compose.logging.yml \
  up --build -d
```

For internet-facing deployments, keep Grafana, Prometheus, Alertmanager, Loki,
and Promtail behind a private network, VPN, or authenticated reverse proxy.
The overlays publish only to localhost by default; set host firewall rules and
bind overrides that match your deployment model.

Promtail uses the Docker socket to discover containers and read their logs.
Treat access to `autvid_promtail` and the mounted socket as operationally
privileged.

## Grafana Dashboards

Grafana is provisioned with a Prometheus datasource named `Prometheus`. When
`docker-compose.observability.yml` is included, it also gets a `Loki` datasource.
These dashboards are loaded in the `Autonomi Video Management` folder:

| Dashboard | Coverage |
|---|---|
| `Autonomi Video - Service Requests` | Service scrape status, request rate, 5xx rate, p95 latency, and firing Autvid alerts |
| `Autonomi Video - Admin Jobs and Uploads` | Job queue state, starts/successes/failures, FFmpeg p95 runtime, outbound `antd` p95 latency, and upload retries |
| `Autonomi Video - Stream Cache and Health` | Stream service scrape status, segment hit ratio, hit/miss/coalesced/eviction rates, resident cache bytes, cache entries, stream request rate, 5xx rate, and p95 latency |
| `Autonomi Video - antd Health` | `antd` scrape status, gateway request/error/p95 latency metrics, scrape duration, and admin-observed outbound `antd` latency/errors |
| `Autonomi Video - Backups` | Backup status, last successful backup age, duration, and output size |

Latency panels use Prometheus histograms and `histogram_quantile`, so p95
latency is available for service requests, FFmpeg, admin-to-antd calls, job
pickup, and stream segment fetches.

## Alerts

Prometheus loads `monitoring/prometheus/rules/autvid-alerts.yml` and sends
alerts to Alertmanager. The default Alertmanager receiver intentionally drops
notifications until you add email, Slack, webhook, or another receiver for
your environment.

| Alert | Severity | Condition |
|---|---|---|
| `AutvidServiceDown` | critical | Prometheus cannot scrape `rust_admin`, `rust_stream`, or `antd` for 2 minutes |
| `AutvidElevatedHttp5xxRate` | warning | More than 5% of handled HTTP requests are 5xx for 10 minutes while traffic is present |
| `AutvidServiceP95LatencyHigh` | warning | Service p95 HTTP latency is above 2 seconds for 10 minutes while traffic is present |
| `AutvidAdminJobsFailed` | warning | At least one admin job attempt failed in 15 minutes |
| `AutvidAdminJobsLikelyStuck` | warning | The oldest queued admin job has waited more than 30 minutes |
| `AutvidAdminAntdErrorRateHigh` | warning | More than 5% of admin outbound `antd` requests fail for 10 minutes |
| `AutvidUploadRetriesHigh` | warning | More than 5 upload retries are scheduled in 30 minutes |
| `AutvidAdminAntdP95LatencyHigh` | warning | Admin-to-antd p95 latency is above 10 seconds for 10 minutes |
| `AutvidAdminJobPickupLatencyHigh` | warning | Job pickup p95 latency is above 10 seconds for 10 minutes |
| `AutvidStreamCacheHitRatioLow` | warning | Segment cache hit ratio stays below 60% for 15 minutes while segment traffic is present |
| `AutvidStreamSegmentFetchP95LatencyHigh` | warning | Stream segment fetch p95 latency is above 2 seconds for 10 minutes |
| `AutvidAntdP95LatencyHigh` | warning | `antd_service` p95 HTTP latency is above 2 seconds for 10 minutes |
| `AutvidBackupMissing` | warning | Backup textfile metrics have not appeared for 26 hours |
| `AutvidBackupFailed` | warning | The most recent backup attempt failed |
| `AutvidBackupStale` | warning | The last successful backup is older than 26 hours |

`AutvidAdminJobsLikelyStuck` uses the explicit queued-job age gauge emitted by
`rust_admin`; tune the threshold to your expected transcode and upload profile.

## Validation

Render the merged Compose config before starting:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.observability.yml \
  config

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.observability.yml \
  config
```

Validate dashboard JSON locally:

```bash
for file in monitoring/grafana/dashboards/*.json; do
  jq empty "$file"
done
```
