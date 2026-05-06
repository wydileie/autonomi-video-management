use std::sync::atomic::{AtomicU64, Ordering};

use autvid_common::{push_counter, push_gauge, HttpMetrics};

use crate::cache::SegmentCacheSnapshot;

#[derive(Default)]
pub(crate) struct StreamMetrics {
    pub(crate) http: HttpMetrics,
    segment_cache_hits_total: AtomicU64,
    segment_cache_misses_total: AtomicU64,
    segment_fetch_coalesced_total: AtomicU64,
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
        assert!(rendered
            .contains("autvid_stream_segment_cache_evictions_total{service=\"rust_stream\"} 2"));
        assert!(rendered
            .contains("autvid_stream_segment_cache_bytes_resident{service=\"rust_stream\"} 4096"));
        assert!(rendered.contains("autvid_stream_segment_cache_entries{service=\"rust_stream\"} 3"));
    }
}
