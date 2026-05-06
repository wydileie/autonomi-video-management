use std::collections::HashMap;
use std::time::{Duration, Instant};

use linked_hash_map::LinkedHashMap;
use tokio::sync::{watch, Mutex};

use crate::config::CacheConfig;
use crate::models::{Catalog, VideoManifest};

pub(crate) struct AppCache {
    pub(crate) catalogs: Mutex<HashMap<String, CachedValue<Catalog>>>,
    pub(crate) manifests: Mutex<HashMap<String, CachedValue<VideoManifest>>>,
    pub(crate) segments: Mutex<SegmentCache>,
    pub(crate) segment_fetches: Mutex<HashMap<String, SegmentFetchReceiver>>,
}

pub(crate) type SegmentFetchResult = Option<Result<Vec<u8>, String>>;
pub(crate) type SegmentFetchReceiver = watch::Receiver<SegmentFetchResult>;

pub(crate) struct CachedValue<T> {
    pub(crate) value: T,
    pub(crate) expires_at: Instant,
}

pub(crate) struct SegmentCache {
    entries: LinkedHashMap<String, CachedSegment>,
    total_bytes: usize,
    max_bytes: usize,
    ttl: Duration,
    evictions_total: u64,
}

struct CachedSegment {
    data: Vec<u8>,
    expires_at: Instant,
}

#[derive(Clone, Copy)]
pub(crate) struct SegmentCacheSnapshot {
    pub(crate) evictions_total: u64,
    pub(crate) bytes_resident: usize,
    pub(crate) entries: usize,
}

impl AppCache {
    pub(crate) fn new(config: &CacheConfig) -> Self {
        Self {
            catalogs: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            segments: Mutex::new(SegmentCache::new(
                config.segment_max_bytes,
                config.segment_ttl,
            )),
            segment_fetches: Mutex::new(HashMap::new()),
        }
    }
}

impl SegmentCache {
    pub(crate) fn new(max_bytes: usize, ttl: Duration) -> Self {
        Self {
            entries: LinkedHashMap::new(),
            total_bytes: 0,
            max_bytes,
            ttl,
            evictions_total: 0,
        }
    }

    pub(crate) fn get(&mut self, address: &str) -> Option<Vec<u8>> {
        if self.disabled() {
            return None;
        }

        let now = Instant::now();
        match self.entries.get_refresh(address) {
            Some(entry) if entry.expires_at > now => {
                let data = entry.data.clone();
                Some(data)
            }
            Some(_) => {
                self.evict_address(address);
                None
            }
            None => None,
        }
    }

    pub(crate) fn insert(&mut self, address: String, data: Vec<u8>) {
        if self.disabled() || data.len() > self.max_bytes {
            return;
        }

        let now = Instant::now();
        self.remove(&address);
        self.total_bytes += data.len();
        self.entries.insert(
            address,
            CachedSegment {
                data,
                expires_at: now + self.ttl,
            },
        );
        self.evict_expired(now);
        self.evict_to_limit();
    }

    fn disabled(&self) -> bool {
        self.max_bytes == 0 || self.ttl.is_zero()
    }

    fn remove(&mut self, address: &str) {
        if let Some(entry) = self.entries.remove(address) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.data.len());
        }
    }

    fn evict_address(&mut self, address: &str) {
        if let Some(entry) = self.entries.remove(address) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.data.len());
            self.evictions_total = self.evictions_total.saturating_add(1);
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        let expired_addresses = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(address, _)| address.to_string())
            .collect::<Vec<_>>();

        for address in expired_addresses {
            self.evict_address(&address);
        }
    }

    fn evict_to_limit(&mut self) {
        while self.total_bytes > self.max_bytes {
            let Some(address) = self.entries.front().map(|(address, _)| address.clone()) else {
                break;
            };
            self.evict_address(&address);
        }
    }

    pub(crate) fn snapshot(&self) -> SegmentCacheSnapshot {
        SegmentCacheSnapshot {
            evictions_total: self.evictions_total,
            bytes_resident: self.total_bytes,
            entries: self.entries.len(),
        }
    }
}
