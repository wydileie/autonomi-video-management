use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    sync::Mutex,
    time::Duration,
};

use autvid_common::{
    push_counter, push_gauge, push_histogram_header, push_histogram_samples, HttpMetrics,
    LatencyHistogram,
};

use crate::cache::SegmentCacheSnapshot;

pub(crate) struct StreamMetrics {
    pub(crate) http: HttpMetrics,
    segment_cache_hits_total: AtomicU64,
    segment_cache_misses_total: AtomicU64,
    segment_fetch_coalesced_total: AtomicU64,
    segment_fetch_latency: Mutex<HashMap<String, LatencyHistogram>>,
}

impl Default for StreamMetrics {
    fn default() -> Self {
        Self {
            http: HttpMetrics::default(),
            segment_cache_hits_total: AtomicU64::new(0),
            segment_cache_misses_total: AtomicU64::new(0),
            segment_fetch_coalesced_total: AtomicU64::new(0),
            segment_fetch_latency: Mutex::new(HashMap::new()),
        }
    }
}

impl StreamMetrics {
    pub(crate) fn record_segment_cache_hit(&self) {
        self.segment_cache_hits_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_segment_cache_miss(&self) {
        self.segment_cache_misses_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_segment_fetch_coalesced(&self) {
        self.segment_fetch_coalesced_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_segment_fetch_latency(&self, cache_state: &str, duration: Duration) {
        self.segment_fetch_latency
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .entry(cache_state.to_string())
            .or_default()
            .record_duration(duration);
    }

    pub(crate) fn render_prometheus_with_cache(
        &self,
        segment_cache: Option<SegmentCacheSnapshot>,
    ) -> String {
        let service = "rust_stream";
        let mut output = self.http.render_prometheus(service);
        push_counter(
            &mut output,
            "autvid_stream_segment_cache_hits_total",
            "Total stream segment cache hits.",
            service,
            self.segment_cache_hits_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_stream_segment_cache_misses_total",
            "Total stream segment cache misses.",
            service,
            self.segment_cache_misses_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_stream_segment_fetch_coalesced_total",
            "Total stream segment requests joined to an in-flight fetch.",
            service,
            self.segment_fetch_coalesced_total.load(Ordering::Relaxed),
        );
        {
            let segment_fetch_latency = self
                .segment_fetch_latency
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            if !segment_fetch_latency.is_empty() {
                push_histogram_header(
                    &mut output,
                    "autvid_stream_segment_fetch_latency_ms",
                    "Stream segment fetch latency in milliseconds.",
                );
            }
            for (cache_state, histogram) in segment_fetch_latency.iter() {
                let snapshot = histogram.snapshot();
                push_histogram_samples(
                    &mut output,
                    "autvid_stream_segment_fetch_latency_ms",
                    service,
                    &[("cache_state", cache_state.as_str())],
                    &snapshot,
                );
            }
        }
        if let Some(segment_cache) = segment_cache {
            push_counter(
                &mut output,
                "autvid_stream_segment_cache_evictions_total",
                "Total stream segment cache evictions.",
                service,
                segment_cache.evictions_total,
            );
            push_gauge(
                &mut output,
                "autvid_stream_segment_cache_bytes_resident",
                "Current stream segment cache resident bytes.",
                service,
                segment_cache.bytes_resident as u64,
            );
            push_gauge(
                &mut output,
                "autvid_stream_segment_cache_entries",
                "Current stream segment cache entry count.",
                service,
                segment_cache.entries as u64,
            );
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn renders_stream_metrics_as_prometheus_text() {
        let metrics = StreamMetrics::default();
        metrics.http.record_request(500, Duration::from_millis(33));
        metrics.record_segment_cache_hit();
        metrics.record_segment_cache_miss();
        metrics.record_segment_fetch_coalesced();
        metrics.record_segment_fetch_latency("cache_hit", Duration::from_millis(3));
        metrics.record_segment_fetch_latency("cache_miss", Duration::from_millis(30));

        let rendered = metrics.render_prometheus_with_cache(Some(SegmentCacheSnapshot {
            evictions_total: 2,
            bytes_resident: 4096,
            entries: 3,
        }));

        assert!(rendered.contains("autvid_http_requests_total{service=\"rust_stream\"} 1"));
        assert!(rendered.contains("autvid_http_request_errors_total{service=\"rust_stream\"} 1"));
        assert!(
            rendered.contains("autvid_http_request_latency_ms_total{service=\"rust_stream\"} 33")
        );
        assert!(
            rendered.contains("autvid_stream_segment_cache_hits_total{service=\"rust_stream\"} 1")
        );
        assert!(rendered
            .contains("autvid_stream_segment_cache_misses_total{service=\"rust_stream\"} 1"));
        assert!(rendered
            .contains("autvid_stream_segment_fetch_coalesced_total{service=\"rust_stream\"} 1"));
        assert!(rendered.contains(
            "autvid_stream_segment_fetch_latency_ms_bucket{service=\"rust_stream\",cache_state=\"cache_hit\""
        ));
        assert!(rendered.contains(
            "autvid_stream_segment_fetch_latency_ms_bucket{service=\"rust_stream\",cache_state=\"cache_miss\""
        ));
        assert!(rendered
            .contains("autvid_stream_segment_cache_evictions_total{service=\"rust_stream\"} 2"));
        assert!(rendered
            .contains("autvid_stream_segment_cache_bytes_resident{service=\"rust_stream\"} 4096"));
        assert!(rendered.contains("autvid_stream_segment_cache_entries{service=\"rust_stream\"} 3"));
    }
}
