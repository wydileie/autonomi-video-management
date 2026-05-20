use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

use crate::prometheus::{push_counter, push_histogram};

const DEFAULT_LATENCY_BUCKETS_MS: &[u64] = &[
    5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000,
];

#[derive(Clone, Debug)]
pub struct HistogramSnapshot {
    pub buckets_ms: Vec<u64>,
    pub cumulative_counts: Vec<u64>,
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
    cumulative_counts: Vec<u64>,
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
                cumulative_counts: vec![0; buckets_ms.len()],
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
                guard.cumulative_counts[index] = guard.cumulative_counts[index].saturating_add(1);
            }
        }
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        let guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        HistogramSnapshot {
            buckets_ms: self.buckets_ms.to_vec(),
            cumulative_counts: guard.cumulative_counts.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_http_metrics_as_prometheus_text() {
        let metrics = HttpMetrics::default();
        metrics.record_request(200, Duration::from_millis(25));
        metrics.record_request(503, Duration::from_millis(75));

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
    fn histogram_snapshot_names_cumulative_bucket_counts() {
        let histogram = LatencyHistogram::new(&[10, 100]);
        histogram.record_ms(10);
        histogram.record_ms(75);

        let snapshot = histogram.snapshot();

        assert_eq!(snapshot.cumulative_counts, vec![1, 2]);
        assert_eq!(snapshot.count, 2);
    }
}
