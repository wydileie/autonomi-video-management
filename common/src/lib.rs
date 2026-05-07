use std::{
    env,
    fmt::Write as _,
    fs,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

use http::HeaderValue;
use subtle::ConstantTimeEq;

const DEFAULT_LATENCY_BUCKETS_MS: &[u64] = &[
    5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000,
];

#[derive(Clone, Debug)]
pub struct HistogramSnapshot {
    pub buckets_ms: Vec<u64>,
    pub counts: Vec<u64>,
    pub count: u64,
    pub sum_ms: u64,
}

#[derive(Debug)]
pub struct LatencyHistogram {
    buckets_ms: &'static [u64],
    inner: Mutex<HistogramState>,
}

#[derive(Debug)]
struct HistogramState {
    counts: Vec<u64>,
    count: u64,
    sum_ms: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self::new(DEFAULT_LATENCY_BUCKETS_MS)
    }
}

impl LatencyHistogram {
    pub fn new(buckets_ms: &'static [u64]) -> Self {
        Self {
            buckets_ms,
            inner: Mutex::new(HistogramState {
                counts: vec![0; buckets_ms.len()],
                count: 0,
                sum_ms: 0,
            }),
        }
    }

    pub fn record_duration(&self, duration: Duration) {
        self.record_ms(duration.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    pub fn record_ms(&self, millis: u64) {
        let mut guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        guard.count = guard.count.saturating_add(1);
        guard.sum_ms = guard.sum_ms.saturating_add(millis);
        for (index, bucket_ms) in self.buckets_ms.iter().enumerate() {
            if millis <= *bucket_ms {
                guard.counts[index] = guard.counts[index].saturating_add(1);
            }
        }
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        HistogramSnapshot {
            buckets_ms: self.buckets_ms.to_vec(),
            counts: guard.counts.clone(),
            count: guard.count,
            sum_ms: guard.sum_ms,
        }
    }
}

#[derive(Default)]
pub struct HttpMetrics {
    request_total: AtomicU64,
    request_error_total: AtomicU64,
    request_latency_ms_total: AtomicU64,
    request_latency: LatencyHistogram,
}

impl HttpMetrics {
    pub fn record_request(&self, status: u16, latency: Duration) {
        self.request_total.fetch_add(1, Ordering::Relaxed);
        self.request_latency_ms_total.fetch_add(
            latency.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        self.request_latency.record_duration(latency);
        if status >= 500 {
            self.request_error_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn render_prometheus(&self, service: &str) -> String {
        let mut output = String::new();
        push_counter(
            &mut output,
            "autvid_http_requests_total",
            "Total HTTP requests handled by service.",
            service,
            self.request_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_http_request_errors_total",
            "Total HTTP requests with 5xx responses.",
            service,
            self.request_error_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_http_request_latency_ms_total",
            "Cumulative HTTP response latency in milliseconds.",
            service,
            self.request_latency_ms_total.load(Ordering::Relaxed),
        );
        push_histogram(
            &mut output,
            "autvid_http_request_latency_ms",
            "HTTP response latency in milliseconds.",
            service,
            &[],
            &self.request_latency.snapshot(),
        );
        output
    }
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn secret_env(name: &str, file_name: &str) -> anyhow::Result<Option<String>> {
    if let Some(path) = non_empty_env(file_name) {
        let value = fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("could not read {file_name} at {path}: {err}"))?
            .trim()
            .to_string();
        if !value.is_empty() {
            return Ok(Some(value));
        }
    }
    Ok(non_empty_env(name))
}

pub fn push_counter(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn push_gauge(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn push_histogram(
    output: &mut String,
    name: &str,
    help: &str,
    service: &str,
    extra_labels: &[(&str, &str)],
    snapshot: &HistogramSnapshot,
) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} histogram");
    push_histogram_samples(output, name, service, extra_labels, snapshot);
}

pub fn push_histogram_header(output: &mut String, name: &str, help: &str) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} histogram");
}

pub fn push_histogram_samples(
    output: &mut String,
    name: &str,
    service: &str,
    extra_labels: &[(&str, &str)],
    snapshot: &HistogramSnapshot,
) {
    for (bucket_ms, count) in snapshot.buckets_ms.iter().zip(snapshot.counts.iter()) {
        let labels = prometheus_labels(service, extra_labels, Some(*bucket_ms));
        let _ = writeln!(output, "{name}_bucket{{{labels}}} {count}");
    }
    let labels = prometheus_labels(service, extra_labels, None);
    let _ = writeln!(
        output,
        "{name}_bucket{{{labels},le=\"+Inf\"}} {}",
        snapshot.count
    );
    let _ = writeln!(output, "{name}_sum{{{labels}}} {}", snapshot.sum_ms);
    let _ = writeln!(output, "{name}_count{{{labels}}} {}", snapshot.count);
}

fn prometheus_labels(service: &str, extra_labels: &[(&str, &str)], le: Option<u64>) -> String {
    let mut labels = format!("service=\"{}\"", escape_label_value(service));
    for (name, value) in extra_labels {
        let _ = write!(labels, ",{name}=\"{}\"", escape_label_value(value));
    }
    if let Some(le) = le {
        let _ = write!(labels, ",le=\"{le}\"");
    }
    labels
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', r"\\")
        .replace('"', r#"\""#)
        .replace('\n', r"\n")
}

pub fn normalize_cors_origin(origin: &str) -> anyhow::Result<String> {
    let origin = origin.trim().trim_end_matches('/');
    if origin == "*" {
        anyhow::bail!("CORS_ALLOWED_ORIGINS must list explicit origins, not '*'.");
    }

    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| {
            anyhow::anyhow!("CORS_ALLOWED_ORIGINS entries must start with http:// or https://")
        })?;

    if host.is_empty() || host.contains('/') || host.contains('?') || host.contains('#') {
        anyhow::bail!(
            "CORS_ALLOWED_ORIGINS entries must be origins like 'https://example.com' with no path, query, or wildcard."
        );
    }

    Ok(origin.to_string())
}

pub fn parse_cors_allowed_origins(raw_origins: &str) -> anyhow::Result<Vec<HeaderValue>> {
    raw_origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let origin = normalize_cors_origin(origin)?;
            HeaderValue::from_str(&origin)
                .map_err(|err| anyhow::anyhow!("invalid CORS origin '{}': {}", origin, err))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origin_normalization_accepts_explicit_origins() {
        assert_eq!(
            normalize_cors_origin(" https://example.com/ ").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_cors_origin("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn cors_origin_normalization_rejects_wildcards_paths_and_missing_schemes() {
        assert!(normalize_cors_origin("*").is_err());
        assert!(normalize_cors_origin("https://example.com/app").is_err());
        assert!(normalize_cors_origin("example.com").is_err());
    }

    #[test]
    fn parses_comma_separated_allowed_origins() {
        let origins = parse_cors_allowed_origins("http://localhost, https://example.com/").unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "http://localhost");
        assert_eq!(origins[1], "https://example.com");
    }

    #[test]
    fn renders_http_metrics_as_prometheus_text() {
        let metrics = HttpMetrics::default();
        metrics.record_request(200, std::time::Duration::from_millis(25));
        metrics.record_request(503, std::time::Duration::from_millis(75));

        let rendered = metrics.render_prometheus("test_service");
        assert!(rendered.contains("autvid_http_requests_total{service=\"test_service\"} 2"));
        assert!(rendered.contains("autvid_http_request_errors_total{service=\"test_service\"} 1"));
        assert!(
            rendered.contains("autvid_http_request_latency_ms_total{service=\"test_service\"} 100")
        );
        assert!(rendered.contains(
            "autvid_http_request_latency_ms_bucket{service=\"test_service\",le=\"25\"} 1"
        ));
        assert!(
            rendered.contains("autvid_http_request_latency_ms_count{service=\"test_service\"} 2")
        );
    }

    #[test]
    fn constant_time_comparison_matches_string_equality() {
        assert!(constant_time_eq("same", "same"));
        assert!(!constant_time_eq("same", "different"));
    }
}
