# Observability

The Compose stack can run an optional Prometheus, Grafana, and Alertmanager
overlay. It scrapes the metrics already exposed by the Rust services and does
not require changing the application containers.

## Metrics Endpoints

Prometheus scrapes these internal Compose targets:

| Service | Target | Metrics path | Primary labels |
|---|---|---|---|
| Admin API | `rust_admin:8000` | `/metrics` | `service="rust_admin"` |
| Stream API | `rust_stream:8081` | `/metrics` | `service="rust_stream"` |
| antd gateway | `antd:8082` | `/metrics` | `service="antd_service"` |

The shared HTTP metrics are counters:

| Metric | Meaning |
|---|---|
| `autvid_http_requests_total` | Total HTTP requests handled by a service |
| `autvid_http_request_errors_total` | Total HTTP requests with 5xx responses |
| `autvid_http_request_latency_ms_total` | Cumulative HTTP response latency in milliseconds |

Admin-specific counters:

| Metric | Meaning |
|---|---|
| `autvid_admin_jobs_started_total` | Durable admin job attempts started by workers |
| `autvid_admin_jobs_succeeded_total` | Durable admin jobs completed successfully |
| `autvid_admin_jobs_failed_total` | Durable admin job attempts that returned an error |
| `autvid_admin_ffmpeg_runs_total` | FFmpeg rendition runs |
| `autvid_admin_ffmpeg_duration_ms_total` | Cumulative FFmpeg runtime in milliseconds |
| `autvid_admin_antd_requests_total` | Outbound requests from `rust_admin` to `antd` |
| `autvid_admin_antd_request_errors_total` | Failed outbound `antd` requests |
| `autvid_admin_antd_request_latency_ms_total` | Cumulative outbound `antd` latency in milliseconds |
| `autvid_admin_upload_retries_total` | Autonomi upload retries scheduled by `rust_admin` |

Stream-specific counters:

| Metric | Meaning |
|---|---|
| `autvid_stream_segment_cache_hits_total` | Segment cache hits |
| `autvid_stream_segment_cache_misses_total` | Segment cache misses |
| `autvid_stream_segment_fetch_coalesced_total` | Requests joined to an in-flight segment fetch |

## Running Locally

Start the local devnet stack with monitoring:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.monitoring.yml \
  up --build
```

Open:

| Tool | URL | Default credentials |
|---|---|---|
| Grafana | `http://localhost:3000` | `admin` / `admin` |
| Prometheus | `http://localhost:9090` | none |
| Alertmanager | `http://localhost:9093` | none |

Override published monitoring ports when they conflict with local tools:

```dotenv
GRAFANA_HTTP_PORT=3001
PROMETHEUS_HTTP_PORT=9091
ALERTMANAGER_HTTP_PORT=9094
GRAFANA_ADMIN_USER=admin
GRAFANA_ADMIN_PASSWORD=<change-me>
```

## Running in Production

Add the monitoring overlay to the production Compose command:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  up --build -d
```

For internet-facing deployments, keep Grafana, Prometheus, and Alertmanager
behind a private network, VPN, or authenticated reverse proxy. The overlay
publishes their ports to the host for convenience; set host firewall rules or
port bindings that match your deployment model.

## Grafana Dashboards

Grafana is provisioned with a Prometheus datasource named `Prometheus` and
these dashboards in the `Autonomi Video Management` folder:

| Dashboard | Coverage |
|---|---|
| `Autonomi Video - Service Requests` | Service scrape status, request rate, 5xx rate, average latency, and firing Autvid alerts |
| `Autonomi Video - Admin Jobs and Uploads` | Job starts/successes/failures, approximate unfinished attempts, FFmpeg runtime, outbound `antd` calls, and upload retries |
| `Autonomi Video - Stream Cache and Health` | Stream service scrape status, segment hit ratio, hit/miss/coalesced fetch rates, stream request rate, 5xx rate, and average latency |
| `Autonomi Video - antd Health` | `antd` scrape status, gateway request/error/latency metrics, scrape duration, and admin-observed outbound `antd` latency/errors |

The services currently emit counters rather than histograms. Dashboard latency
panels therefore show moving averages derived from cumulative latency and
request counters, not percentile latency.

## Alerts

Prometheus loads `monitoring/prometheus/rules/autvid-alerts.yml` and sends
alerts to Alertmanager. The default Alertmanager receiver intentionally drops
notifications until you add email, Slack, webhook, or another receiver for
your environment.

| Alert | Severity | Condition |
|---|---|---|
| `AutvidServiceDown` | critical | Prometheus cannot scrape `rust_admin`, `rust_stream`, or `antd` for 2 minutes |
| `AutvidElevatedHttp5xxRate` | warning | More than 5% of handled HTTP requests are 5xx for 10 minutes while traffic is present |
| `AutvidServiceAverageLatencyHigh` | warning | Average service HTTP latency is above 2 seconds for 10 minutes while traffic is present |
| `AutvidAdminJobsFailed` | warning | At least one admin job attempt failed in 15 minutes |
| `AutvidAdminJobsLikelyStuck` | warning | Jobs started in the last hour, but no jobs completed or failed in the last 30 minutes |
| `AutvidAdminAntdErrorRateHigh` | warning | More than 5% of admin outbound `antd` requests fail for 10 minutes |
| `AutvidUploadRetriesHigh` | warning | More than 5 upload retries are scheduled in 30 minutes |
| `AutvidStreamCacheHitRatioLow` | warning | Segment cache hit ratio stays below 60% for 15 minutes while segment traffic is present |
| `AutvidAntdAverageLatencyHigh` | warning | `antd_service` average HTTP latency is above 2 seconds for 10 minutes while traffic is present |

`AutvidAdminJobsLikelyStuck` is an approximation because the current metrics
surface does not expose a job backlog gauge, lease age, or per-state counts.
Tune or replace it if future service metrics add explicit queued/running job
state.

## Validation

Render the merged Compose config before starting:

```bash
docker compose --env-file .env.local \
  -f docker-compose.yml \
  -f docker-compose.local.yml \
  -f docker-compose.monitoring.yml \
  config

docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  config
```

Validate dashboard JSON locally:

```bash
for file in monitoring/grafana/dashboards/*.json; do
  jq empty "$file"
done
```
