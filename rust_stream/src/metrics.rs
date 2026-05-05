use std::sync::atomic::{AtomicU64, Ordering};

use autvid_common::{push_counter, HttpMetrics};

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

    pub(crate) fn render_prometheus(&self) -> String {
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
        output
    }
}
